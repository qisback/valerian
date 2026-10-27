# Tutorials

These pages are numbered walk-throughs. Each one has a goal, the data it uses, the exact
commands, what the terminal prints (pasted from a real run), a before and after picture,
how to tell it worked and what to do when it did not. They follow one thread, so each
page starts where the previous one ends.

If you want to know **why** a step works the way it does, read the matching "How X works"
page in the user guide; the tutorials link to it. If you want one page that runs the whole
chain once, read [A first session](../user-guide/walkthrough.md). Every option is listed
with its range and default in the [command line reference](../reference/cli.md).

| # | Tutorial | You will | Data |
|---|---|---|---|
| 1 | [Install](install.md) | get Valerian, check the download, check it runs | none |
| 2 | [Calibrate](calibrate.md) | remove dark current, hot pixels, vignetting and dust | ASI1600MM, H-alpha |
| 3 | [Register](register.md) | line sixty frames up on the sharpest one | Seestar S50, IC 5070 |
| 4 | [Stack](stack.md) | combine them with rejection and weights | IC 5070 |
| 5 | [Background extraction](background.md) | take the sky glow out | IC 5070 |
| 6 | [Stretch](stretch.md) | make the linear stack visible | IC 5070 |
| 7 | [Star removal](star-removal.md) | take the stars out, keep them aside | IC 5070 |
| 8 | [Plate solve](plate-solve.md) | find where the picture points | IC 5070, Gaia catalogue |
| 9 | [Photometric colour](photometric-colour.md) | set the white balance from star colours | IC 5070 |
| 10 | [Drizzle](drizzle.md) | recover resolution from dithered frames | IC 5070 |
| 11 | [GPU](gpu.md) | check the adapter and measure the speed-up | IC 5070 |
| 12 | [Subframe analysis](subframe-analysis.md) | plot every frame's FWHM, noise and weight, and leave the poor ones out of the stack | IC 5070, ASI1600MM |
| 13 | [Optics](optics.md) | read the field: corners, maps, tilt hint and star profile | IC 5070 stack, ASI1600MM stack |
| 14 | [Integration diagnostics](integration.md) | see where the combine rejected samples, how many frames each pixel got, the noise against the square root, and the histograms | IC 5070 |
| 15 | [Astrometry and photometry](astrometry.md) | draw the plate solution, check its residuals and the colour fit, follow one star through the night | IC 5070, ASI1600MM |
| 16 | [Deconvolution](deconvolution.md) | narrower, rounder stars | IC 1795 |
| 17 | [Stack a mosaic](mosaic.md) | put every pointing of a mosaic session on one larger canvas | Seestar M 33 and NGC 869 mosaic folders |
| 18 | [A Seestar session, install to picture](seestar-session.md) | check the install, stack 184 raw frames, take out the gradient, plate solve, colour calibrate, stretch and sharpen, on one fresh capture | Seestar M 34 |
| 19 | [A faint field, and the GPU](seestar-faint-field-gpu.md) | fix the colour calibration of a sparse field, then measure the stack on the card, on the processor and with different memory limits | Seestar NGC 253, M 34 |
| 20 | [Zero point, magnitudes and the limit](zero-point-and-limit.md) | measure the zero point and colour term of each colour, list every star with a calibrated magnitude and error, find the limiting magnitude and map the plate solution's residuals | Seestar M 34 |

One tutorial is for the desktop app only: [Cancelling a running step](cancel.md) stops
Calibrate, Register, Stack or a Process tool part way and shows that nothing is left behind.

## Desktop app tutorials

These three walk through features of the desktop app that save typing. They use the same captures.

| Tutorial | You will | Data |
|---|---|---|
| [Sort a folder of frames, pick a session up](sort-and-sessions.md) | drop a folder, check how it was sorted into lights, darks, flats and bias, and reopen the session later | ASI1600MM H-alpha, Seestar IC 5070 |
| [Work from the keyboard](keyboard.md) | run, cancel, step, zoom and open from the keyboard, with the F1 list | either |
| [Zoom to the real pixels](zoom.md) | look at a stack at 1:1 and 4:1 and read the value of any pixel, with its sky position when solved | ASI1600MM H-alpha, Seestar IC 5070 |

## The two captures

**IC 5070, the Pelican Nebula (tutorials 3 to 11).** Sixty 10-second sub-exposures from a
Seestar S50 (250 mm focal length, 1080 by 1920 pixels, raw Bayer data in GRBG order, light
pollution filter, 2026-07-28). They are raw mosaics: Valerian demosaics them itself. A
Seestar writes no darks or flats, so there is nothing to calibrate against and the thread
goes straight from the lights to registration.

**The California Nebula in H-alpha (tutorial 2 only).** Twenty-five 300-second lights from a
ZWO ASI1600MM Pro (monochrome, 4656 by 3520 pixels, Starwatcher Esprit at 840 mm, -10 C
sensor), ten 300-second darks, thirty 2.04-second flats and thirty 2.04-second dark flats.
This is the capture with a full set of calibration frames. The twenty-five lights are five
panels of five (the header `RA` and `DEC` differ between the panels), which [tutorial
2](calibrate.md) uses to show what a mixed set looks like.

Both are real captures. You do not need them: every command takes your own folders.

*Product and project names in this manual are trademarks of their owners and are used only to
identify them; Valerian is not affiliated with them. See [Credits and trademarks](../credits.md).*

## How the numbers and pictures were made

Every block of terminal output is pasted from a run of the `valerian` binary built from
this repository on a 32-core AMD EPYC 7543 Linux server with an NVIDIA A100 (the machine is
shared, so times vary by a few seconds from run to run, and are not a promise for your
computer). Where a page quotes a time, it says which run it came from.

Each picture is a crop or a reduction of a real output file, written by a small script
that reads the FITS file, applies the stated stretch and saves the result. **Linear data
has to be stretched to be seen**, so every linear image on these pages has a screen
stretch applied and the page says so: black point at the sky level minus 2.8 robust sigma,
white point at the 99.9th percentile, and a midtone transfer function that puts the sky at
20 per cent grey. Pictures of data that Valerian has already stretched are shown as they
are. Photographs are saved as JPEG to keep the book small, and black-and-white pictures as
PNG.

## Folders used on these pages

```text
lights/        the raw light frames (FITS)
darks/         dark frames          (tutorial 2 only)
flats/         flat frames          (tutorial 2 only)
dark-flats/    dark flats           (tutorial 2 only)
```

Each tutorial names the files it writes in the folder you run it from, so that the next one
can read them.

## The desktop app

The desktop app does the same steps in a window, apart from background extraction. The tutorials give the command line, which is
where every option is spelled out, and end each page with a short note that names the matching step or tool of the app and links to its
reference. The app has its own walk-through: [a session in the desktop app](../user-guide/gui-tutorial.md).
