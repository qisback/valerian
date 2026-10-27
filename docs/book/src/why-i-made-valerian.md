# Why I made Valerian

I work in high-performance computing, and I tend to think about problems the way an HPC person does. You do not sit and watch a job. You automate the work, put it in a queue, let the machine get on with it in batches, and come back to review the results. If they are not good enough, you adjust the requirements and submit again. Doing that loop a few hundred times is how you find out what really matters.

Deep-sky imaging suits that habit well, because a night of subframes is a batch by nature: tens or hundreds of exposures that all need the same treatment. Before Valerian, my deep-sky processing was a workflow of my own, mostly Python, which I built and used on my own captures, around that loop: process, review the results, adjust, run it again. Much of that workflow has now been converted into Valerian, as fast, tested software. That is why the program is built from separate commands that each read files and write files, and not only as a window with a wizard in it.

## How the habits became features

**Automate it and run it unattended.** Every step is its own command: `valerian calibrate`, `register`, `stack`, `background`, `stretch`, `starless`, `solve`, `photocal` and `drizzle`. Each takes its inputs and settings on the command line, writes a FITS file, and exits non-zero if it fails. A session is therefore a short script that chains them, and it runs on a server with no screen. Valerian has no built-in job queue today. The queue is whatever you already use: a shell loop, `make`, or a cluster scheduler. The commands are written so that works.

**Review the results as data.** `valerian stack --report` writes the per-frame table to a text file: for every frame the stars found, the star width, the ellipticity, the sky level, the noise, the weight it was given and how much of it was rejected. `--coverage` writes a map of how many frames contributed to each pixel. `valerian info` gives the header, noise and star statistics of any FITS file. These are what I look at before the picture, to see which frames were poor and whether the stack is even.

**Adjust and resubmit.** The things I change between runs are flags: the rejection scheme and its limits, the frame weights, the level matching, the transform model, `--min-stars` and `--max-fwhm-ratio` to leave out poor frames, and the interpolation kernel. Change one, run again, compare the reports. The FITS files record what was done: the number of frames combined, the reference frame, the rejection and weights used, and `HISTORY` cards with the numbers, so a result says how it was made. A stretch is recorded in the header so that it can be undone.

**Use the machine you have.** The warp and the stack combiner run on a GPU with `--backend gpu`, and `valerian bench` measures every stage on your own hardware, on the processor and on the card, so you can see whether the card helps before you rely on it. On a small set of frames the honest result is that the card does not make the wall clock shorter but does use much less processor time; the [GPU tutorial](tutorials/gpu.md) has the measurements.

**Pick a session up again.** In the desktop app, dropping a folder sorts it into lights, darks, flats and dark flats and fills in the boxes, and the app remembers the sessions you worked on lately. That is the interactive version of a script that finds its own inputs.

## What this does not mean

The desktop app is real, and a lot of care has gone into it: the steps on a rail, an image view with a histogram, a red night-vision palette for the dark. It runs on the same engine as the command line and is not a separate program. If you only ever use the app, you lose nothing. If you have a month of subframes and a server in a cupboard, the commands are there.

## Dedication

Valerian is dedicated to my son, Xaviar, and takes its name from his middle name.

> Valerian is dedicated to my son Xaviar. His middle name is Valerian, because he
> was a very sleepy child, which suits software made for the long, quiet hours of
> the night sky.

I made it for those long quiet hours, and so that the machine could do some of the waiting instead of me.

Laurence Horrocks-Barlow
