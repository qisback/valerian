# How stacking works

For a numbered walk-through on real data, with the settings and troubleshooting, see
[Tutorial 4: Stack](../tutorials/stack.md).

Frames taken at several pointings (a mosaic) are stacked with `--mosaic`: see
[How mosaics work](mosaic.md).

Stacking registers the frames, resamples them onto one grid, evens out their
levels, weights them and combines them with outlier rejection. The function is
`valerian_core::stack::stack`; the command is `valerian stack`.
Registration and rejection have pages of their own:
[How registration works](registration.md) and
[How calibration works](calibration.md).

## The steps

1. **Detect** the stars in every frame (on the luminance, for colour frames).
2. **Choose the reference**: the sharpest frame among those with a healthy
   number of stars, or the one you name. If fewer than half of the other frames
   register to the sharpest, the next three sharpest are tried, so a frame of the
   wrong field cannot become the reference. If no candidate gets half, the one
   that most frames register to is used and the rest are reported as skipped, as
   happens with a session that covers several fields (a mosaic).
3. **Register** each frame to the reference. A frame that cannot be registered
   is left out and the reason is reported; the others are unaffected. Frames
   that are too soft can be left out too: `--max-fwhm-ratio 1.5` leaves out any
   frame whose median FWHM is more than one and a half times the reference's
   (the default is no limit), and `--min-stars` (default 20) leaves out frames
   with too few stars to register. The report names the reason for each frame
   left out. The frame limits of `valerian subframes` work on `stack` too:
   `--max-fwhm`, `--max-ellipticity`, `--min-star-count`, `--max-noise`,
   `--min-snr-weight`, `--max-residual` and `--outlier-sigma` leave the frames
   they reject out of the stack (see [Subframe analysis](subframes.md)).
4. **Resample** onto the reference grid. Pixels a frame does not cover are
   NaN, which the combiner reads as "no data", so the edges of a rotated or
   dithered stack are averaged over fewer frames instead of being black.
5. **Normalise.** Each frame's sky level is matched to the reference's, and
   its transparency is matched by the median flux ratio of the matched stars
   (the brightest eight or more that are not saturated), so a frame shot through
   thin cloud counts for what it saw. A frame with too few matched stars to
   measure this keeps a scale of one, and the log says so. The reference's
   sky level stays as the pedestal of the result; where black sits is decided by
   the stretch, not here.
6. **Weight.** The default weight is `1 / (noise^2 * FWHM^2)`, which is what
   maximises the signal-to-noise of a point source in a background-limited
   frame: quiet, sharp frames count for more. Equal, noise-only and FWHM-only
   weights are available.
7. **Combine** with per-pixel rejection, chosen from the number of frames
   unless you ask for one (`--rejection`). For the schemes with limits, `--low` and
   `--high` set the lower and upper limit in sigmas; left out they are 3 and 3 for
   sigma clipping and winsorised clipping, and 5 and 2.5 for linear fit
   (`minmax` drops one sample at each end and `none` keeps all). Satellite trails, cosmic rays and hot pixels all
   disappear here: registration drags a detector-fixed defect to a different sky
   position in every frame, so it is an outlier at each of them.

The result carries the reference header with `NCOMBINE`, `EXPTOTAL` (the summed
exposure of the frames used), `STACKREF` and `STACKALG` added. A coverage plane
says how many frames contributed at each pixel, and a per-frame report gives
the stars, FWHM, registration residual, flux scale, weight and the fraction of
that frame's samples that were rejected.

## Memory: any number of frames

`valerian stack` does not hold the session in memory. It makes three passes
over the files, one frame at a time:

1. **Measure.** Each frame is read and debayered, its stars, sky and noise are
   measured, and its pixels are dropped. The reference is chosen and every
   frame registered from those measurements alone.
2. **Resample.** Each kept frame is read again, warped onto the reference grid
   and level-matched exactly as in the steps above, and written to a staging
   file of 32-bit floats. When the whole set fits `--memory` the staging stays
   in memory and no file is made.
3. **Combine in bands.** Rejection and the weighted mean are per pixel, so a
   band of rows can be read back from every frame, combined and dropped. The
   rows per band follow from `--memory` and the number of frames.

Nothing is approximated: the stack, the coverage plane, the per-frame rejected
fractions and the maps are bit for bit those of an all-in-memory stack (the
tests and a real 46 and 380 frame session check this). The price is a second
read and debayer of every kept frame and scratch disk of `frames x width x
height x planes x 4` bytes (34 GB for 1,349 colour frames of 1080 x 1920);
point `--scratch-dir` at a disk with room. The file is removed when the run
ends, also on an error or a cancel.

`--memory` is in MiB and defaults to 4096. Measured peak memory on 1080 x 1920
colour frames (CPU engine, same machine):

| Frames | Before | After (4096 MiB) |
|---|---|---|
| 46 | 1.8 GB | 1.9 GB (in memory) |
| 380 | 12.7 GB | 2.5 GB |
| 1,349 | about 44 GB (extrapolated from the first two, not run) | 2.3 GB |

`--science-report` still holds every frame, because the rejection maps and the
noise curve need the whole set.

## Leaving out poor frames

`valerian stack --approve TABLE` leaves out the frames a
[subframe analysis](subframes.md) rejected. For raw Seestar S50 subs, the optional `--grade` (off unless given) does it with the
[sub grader](grading.md) instead: each sub is scored by a small neural network before anything
else happens, the ones below `--min-grade` (or over a `--reject-defect` limit) are left out with
the reason printed, and `--grade-weights` also multiplies the weight of every kept frame by its
score. Frames that are not S50 subs are not graded.

## What to expect

* Noise falls as the square root of the number of frames when the frames are
  shifted by whole pixels. When they are rotated or shifted by fractions of a
  pixel, resampling smooths each frame's noise a little, so the per-pixel noise
  of the stack falls somewhat faster, at the cost of correlated noise between
  neighbouring pixels.
* Stars in the stack sit within 0.05 pixel of where the reference puts them.
* Colour frames are registered once, on the luminance, and every plane is
  moved by the same transform.

## Not done yet

* Distortion-aware warps. (Mosaics are done: see [How mosaics work](mosaic.md).) Drizzle for undersampled, dithered data is
  [its own command](drizzle.md).
* Streaming with `--science-report`: a plain stack is streamed (`--memory`, default 4096 MiB, sets
  the peak; the resampled frames wait in a staging file), but a stack that writes the science report holds
  every frame.
* Meridian-flip handling beyond the rotation by 180 degrees that registration
  already accepts, and per-pixel-position rejection masks across sessions.
