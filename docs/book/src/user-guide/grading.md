# How the Seestar sub grader works

The Seestar sub grader looks at each raw sub from a Seestar S50 and gives it a score from 0
(unusable) to 1 (clean). It also says how likely it is that the sub has each of five common
problems. You can use the score to leave the poor subs out of a stack without opening them one
by one.

Grading is optional and off by default. Nothing is graded unless you ask for it: pass `--grade`
on the command line, or tick "Grade with the Seestar model" in the Science panel (the box starts
unticked). Without it, every command behaves exactly as it did before the grader existed.

It is a small neural network. It runs on your own computer, on the processor, with no network
connection and no extra software to install.

## Read this first

* **It is trained on Seestar S50 subs only.** It knows what that camera's raw frames look like.
  It is not a general-purpose grader, and its scores for any other camera are meaningless.
  Frames that are not S50 subs are skipped with a note, never graded.
* **It does not beat the classical limits on stacks.** On the evaluation over 64 targets, stacks
  made after grading were no better than stacks made with the limits of `valerian subframes`,
  and removing frames never beat stacking every frame (details below).
* **What it is good for.** It is better than the measurements at cloud and haze and at trails,
  and it needs no star detection and no registration, so it can grade a sub on its own.
* **The dew output is unreliable.** It is shown, but it did not detect dew in testing; ignore it.
* **It is a helper, not a judge.** It will sometimes reject a sub you would have kept and keep one
  you would have rejected. Look at the table, and change a verdict by hand if you disagree.
* **It looks at the sub as a whole.** It does not measure stars the way
  [subframe analysis](subframes.md) does. The two work well together: the grader spots clouds,
  haze and trails, and the measurements catch a slow slide in focus or a bad registration.

## How well it works

The model is 0.5 MB. On one thread of an AMD EPYC 7543 it takes 15 ms to grade a sub (the
model alone, once the frame is prepared); the frames of a session are graded in parallel.

It was trained on 11,543 subs from 64 targets of the author's own library, and tested by
six-fold cross-validation over whole targets, so every score came from a model that had never
seen that target. Rejecting subs that score below 0.5 flagged 2,229 of the 11,543. Of those,
52 per cent matched the labels, and the grader found 52 per cent of all the subs those labels
call poor (ROC AUC 0.80).

| Output | Precision | Recall |
|---|---|---|
| Cloud or haze | 0.35 | 0.45 |
| Blur | 0.37 | 0.62 |
| Too few stars | 0.40 | 0.83 |
| Trail | 0.62 | 0.62 |
| Dew | not detected (0 of 95) | |

The trail figures are for 16 real labelled trails. On made-up trails it found 99.8 per cent,
with 0.1 per cent false alarms.

**Do not trust the dew output.** The model did not detect any of the 95 subs labelled as dew.
The quality score and the cloud or haze, blur, too few stars and trail outputs are the useful
ones.

For comparison, a plain gradient-boosting model on the same classical measurements (star
count, FWHM, ellipticity, background level) separates the subs the labels call poor better: AUC
0.85, with precision and recall of 0.67 and 0.68 at the same number flagged. That is expected,
because the labels were made from those measurements. The network's advantage is that it needs
no star detection and no registration, and it is better on cloud or haze (AUC 0.83 against
0.76) and on trails (0.98 against 0.56).

On a stack the effect is small, and it does not beat the classical limits. Five sessions were
stacked from raw subs (100 to 150 subs each), with scores from models that had not seen that
target, against these controls: every frame, the same number of frames removed at random, the
limits of `valerian subframes` tuned to remove the same number, and a plain gradient-boosting
model on the same measurements. Removing frames never raised the signal-to-noise (a
star-peak-over-noise measure) above stacking every frame; the lost integration costs more than
the bad frames do. Against random removal the grader chose well on the two sessions with real
cloud or blur (C 39: 141 against 122, M 34: 1963 against 1747), barely on a third, and
not at all on the two cleaner ones. Against the classical limits it did not win: the limits
matched or beat it on signal-to-noise in all five sessions and gave the narrowest stars in
three, and the gradient-boosting model was level with it. The grader and the limits pick
overlapping but different frames (26 of 48 in common on C 39, 62 of 74 on M 34, 4 of 13 on
IC 1396). So use grading when you want a score without measuring and registering every frame,
or to find cloud and trails, not because it stacks better than the limits you can set by hand.

Two things to keep in mind. The labels the model learned from and was tested against come from
classical measurements (star count, FWHM, ellipticity, background level and line detection),
not from a person looking at each frame, so the grader agrees with those measurements, which is
not the same as agreeing with you. And it was trained on one person's library of 64 targets, so it
may do less well on yours.

## What it gives you

| Output | Meaning |
|---|---|
| **Score** | Overall quality, 0 (unusable) to 1 (clean). |
| `cloud_haze` | Chance that cloud or haze is across the frame. |
| `trail` | Chance of a satellite or aircraft trail. |
| `blur` | Chance that the stars are soft or smeared. |
| `dew` | Chance that dew is on the optics. **Not reliable**: the model did not detect any labelled dew. |
| `star_poor` | Chance that there are too few stars for the sub to be useful. |

The five chances are independent, between 0 and 1, and do not add up to 1: a frame can be both
hazy and trailed. A high chance is a hint to look at the frame, not proof.

## Which frames it will grade

Only raw S50 subs: 1080 pixels wide, 1920 tall, in the Bayer pattern GRBG, as the telescope
writes them. Grade the frames before calibration or debayering, because the grader reads the
raw pixels itself. A frame of any other size or pattern, or one that has already been
debayered, is skipped. Valerian says so and carries on; it never stops the run.

## On the command line

Add `--grade` to either command:

```sh
valerian subframes lights/ --report science --grade
valerian stack lights/ -o stack.fits --grade
```

| Option | What it does | Default |
|---|---|---|
| `--grade` | Grade the raw S50 subs before anything else. | off |
| `--min-grade <SCORE>` | Reject frames that score below this. | 0.5 |
| `--reject-defect <NAME:P>` | Also reject frames whose chance of a defect is at or above `P`. `NAME` is `cloud_haze`, `trail`, `blur` or `star_poor` (`dew` is accepted, but that output is not reliable, so do not use it). Repeat the option for several defects. | none |
| `--grader-model <FILE>` | Use this model file instead of the one built into the program. | the built-in model |
| `--grade-weights` | (`stack` only) Also multiply each kept frame's weight by its score, so cleaner frames count for more. | off |

For example, to drop anything under 0.6 and anything that is probably trailed:

```sh
valerian stack lights/ -o stack.fits --grade --min-grade 0.6 --reject-defect trail:0.8
```

### With `valerian subframes`

The score and the five chances join the table as the columns `grade_score`, `cloud_haze`,
`trail`, `blur`, `dew` and `star_poor`, in the printed table, in `subframes.csv` and in
`subframes.json`. A frame that fails the grade limits is rejected, and the reason is added
to its `reasons`, for example `grade 0.31 below 0.5` or `trail 0.91 at or above 0.8`. The grade
is judged together with your other limits: a frame must pass all of them to be approved. A frame
the grader could not judge has empty grade columns and is left to the other limits.

Because the table is the same one `--approve` reads, you can grade once and then stack what you
approved:

```sh
valerian subframes lights/ --report science --grade
valerian stack lights/ -o stack.fits --approve science/subframes.csv
```

### With `valerian stack`

The grader runs first, on the raw frames. Frames that fail are left out, each with a line saying
why (`grade: sub_07.fits skipped: grade 0.31 below 0.5`), and the stack carries on with the
rest. If fewer than two frames are left, the command stops and says so.

With `--grade-weights` the score also scales the weight of every kept frame, on top of the
weighting chosen with `--weights`. A frame that scores 0.9 counts for roughly twice one that
scores 0.45. The weights are rescaled afterwards so their average stays 1, so the stack's
overall level does not change. A frame that could not be graded keeps a score of 1.

## In the desktop app

In the **Science** panel of the Stack step, tick **Grade with the Seestar model** before you
press **Measure frames**.

| Control | What it does | Range | Default |
|---|---|---|---|
| Grade with the Seestar model | Grade the raw S50 subs while measuring. | on or off | off |
| Reject below | Frames that score less than this are rejected. Changing it judges the frames again at once, without measuring again. | 0 to 1 | 0.5 |
| Model file | A model to use instead of the built-in one; leave it empty for the built-in one. | an `.onnx` file | empty |

The grade shows in the hover text and the reasons of each frame, and in the report. Press **Use
for stacking** as usual to leave the rejected frames out of the next stack.

## Using your own model

The grader is a file in the open ONNX format that follows a small published contract, so a
different model can be used with `--grader-model` or the Model file box, and the program checks
that it follows the contract before using it. If it does not, Valerian says what is wrong, for
instance that the model was made for a different version of the contract. The contract is
written up in [The grader model contract](../developer/grader-model.md).

## If something goes wrong

**`not graded: it is ... pixels`, or `not a raw GRBG Bayer frame`.** The frame is not a raw S50
sub, so it was skipped. Grade the original subs, not calibrated or debayered copies.

**`this model follows grader contract N`.** The model was made for a different version of the
contract than this Valerian understands. Use a matching pair.

**Every frame is rejected.** Check the scores in the table first; if they are all low, the
frames may not be what the model was trained on. Lower `--min-grade`, or leave `--grade` off.
