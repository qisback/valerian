# Troubleshooting by stage

The messages below are the ones `valerian` 0.4.0 prints, each with what causes it and what to do. The newest
come from running the whole chain on real Seestar S50 captures
([tutorial 18](../tutorials/seestar-session.md), [tutorial 19](../tutorials/seestar-faint-field-gpu.md)):
a sparse galaxy field, a mosaic folder, raw frames given to a step that needs a stack, and stacks of 120 to 411
frames. The rest were produced on the captures of the other [tutorials](../tutorials/index.md); the
tutorial for each stage has the full context. Errors are printed on standard error as `valerian: ...`,
the command exits 1 and writes nothing for that step. Notes (`note: ...`) do not stop a command.

Find your stage, then the message. If a message is not here, the [command line
reference](cli.md) lists every option with its range.

## Install and start-up

| Symptom | Cause and fix |
|---|---|
| `FAILED` from `sha256sum -c`, or a different hash | The download is damaged or is not the published file. Download it again. [Tutorial 1](../tutorials/install.md) |
| `valerian: command not found` | The program is not on `PATH`. Run it by its path or add its folder. |
| `catalogue: not found` in `valerian info`; `no star catalogue found; looked for gaia-dr3-g11.vcat in: ...` | The catalogue is not beside the program. Copy it there or set `VALERIAN_CATALOGUE`. [Catalogue file](catalogue-format.md) |
| `gpu: unavailable (could not open GPU device: ...)` | No Vulkan (Windows: DirectX 12) driver. Commands still run on the processor. [Tutorial 11](../tutorials/gpu.md) |
| An error naming `GLIBC_2.35` | The Linux build needs glibc 2.35 or newer. |

## Seestar S50 frames

| Symptom | Cause and fix |
|---|---|
| `valerian info` shows `1 plane (raw Bayer mosaic)` and `BAYERPAT GRBG` | That is a raw sub-frame, as the Seestar writes it, and is right. `stack`, `register` and `drizzle` demosaic it themselves; the other steps work on a stack. |
| `plate solving needs a demosaiced image, this one is a raw colour mosaic` (from `solve`, or from `photocal`, which first says `no WCS in the image: solving it from the header first`) | You gave a raw sub-frame. Stack first, then solve the stack. |
| `<frame> is a raw colour mosaic; calibrate it with --debayer first` (from `background`) | The same cause: background extraction works on a stack, not on a raw frame. |
| `stacking failed: frame 0 is a raw Bayer mosaic; demosaic it before stacking` | You gave `--debayer none` with raw frames. Leave `--debayer` at `auto` (or `mhc`, `bilinear`). |
| A picture that is strongly green, or `valerian stretch` on a raw frame writes a one-plane grey picture | A one-shot colour sensor has twice as many green pixels as red or blue, and the planes have different sky levels. Colour-calibrate the stack with `photocal` ([tutorial 9](../tutorials/photometric-colour.md)); `valerian stretch --unlinked` is the quick, colour-losing alternative. |
| The first frames of a session have a sky of 4,000 ADU where the rest has 630, and a weight of 0.04 | Dusk frames. Nothing to fix: the weights and the rejection deal with them. Leave them out with the frame limits ([tutorial 12](../tutorials/subframe-analysis.md)) if you want. |
| A Seestar stack (3 planes, `Stacked_...fit`) in the same folder as raw sub-frames | It is accepted and stacked with them. Keep the Seestar's own stack out of the folder unless you mean it. |

## Calibrate

| Symptom | Cause and fix |
|---|---|
| `nothing to calibrate with: give at least one of --bias, --dark and --flat, or --auto-cosmetic` | No calibration frames given. `--auto-cosmetic` alone is enough: it repairs isolated hot and cold pixels in each light. A Seestar writes no calibration frames. |
| `compute: CPU (the GPU is slower for this command; --backend gpu forces it)` | A note, not an error: `calibrate` and `register` use the processor unless you force the card. |
| `a master frame differs in size from the light` | A calibration frame has another size or binning than the lights. |
| `<path> does not exist`, `<folder> contains no FITS files` | A wrong path, or a folder with no `.fits`, `.fit` or `.fts`. |
| `dark scaling needs a master dark made with a master bias` | `--scale-dark exposure` or `optimise` needs `--bias`. |

[Tutorial 2](../tutorials/calibrate.md)

## Register and stack

| Symptom | Cause and fix |
|---|---|
| `registered 10 of 25 frames`, `skipped: registration failed: best hypothesis matches only 5 stars (need 6)` | The frames do not share a field: other panes of a mosaic, or a few frames whose stars are too poor to match. For panes, see [Mosaics](#mosaics). |
| `frame 25 has 3 planes, the first has 1` | Mono and colour frames mixed. |
| `fewer than two frames registered; nothing to stack` | One frame, or none matched the others. |
| `skipped, only 77 stars` / `skipped, FWHM 3.59 px is too soft` | `--min-stars` or `--max-fwhm-ratio` left the frame out; the table says which. |
| `reference frame 99 out of range (6 frames)` | `--reference` counts from 0. |
| `--low` and `--high` change nothing | With `--rejection auto` they change the limits of the scheme auto picks. They have no effect (and a note says so) with `none`, `minmax` or two frames or fewer. |
| The stack takes too much memory | The peak follows `--memory` (default 4096 MiB), not the number of frames: 184 Seestar frames peaked at 1.2 GB with `--memory 1024`, 1.9 GB by default and 5.3 GB with `--memory 16384`. Lower it. `--science-report` holds every frame and took 9.0 GB for the same 184: leave it out. [Tutorial 19](../tutorials/seestar-faint-field-gpu.md#step-4-how-much-memory) |
| `No space left on device` during a stack | The staging file is frames x width x height x planes x 4 bytes (4.3 GiB for 184 Seestar frames). Point `--scratch-dir` at a disk with room. |
| `compute: CPU (the rejection maps and the noise curve need the CPU combiner)` | A note: `--science-report` forces the processor combiner, so it is slower (62 s against 33 s for 184 frames) and uses more memory. Run `stack` without it, and `valerian subframes` for the plots. |

[Tutorial 3](../tutorials/register.md), [tutorial 4](../tutorials/stack.md)

## Mosaics

* **`note: 27 of 167 frames did not register to the reference. If they are other panes of a mosaic, stack with --mosaic to build one canvas.`**
  The frames are other panes of a mosaic (a Seestar mosaic-mode folder). Without `--mosaic` only the
  panes that overlap the reference pane are kept; their table rows say `skipped: registration failed:
  best hypothesis matches only 4 stars (need 6)`. Stack with `--mosaic`:
  [Tutorial 17](../tutorials/mosaic.md).
* **"the mosaic canvas would be ... a registration put a frame in the wrong place".** A frame
  matched wrongly. Find it in `--report` by its odd shift, leave it out and rerun.
* **`No space left on device` in a mosaic run.** The staging file holds the whole canvas for
  every frame (about 30 GB for 167 frames of 2568 x 4390 pixels); give `--scratch-dir` a big disk.
* **A step remains between panes.** Run `valerian background`; raise `--mosaic-feather`.
* **`--mosaic` with `--science-report`.** The two cannot be used together: make the plots from a separate run.

## Background extraction

| Symptom | Cause and fix |
|---|---|
| `background fit declined, plane left as it is: 62% of the image is star, nebula or target` | Too little clear sky. Not an error; the plane is written unchanged. |
| `is a raw colour mosaic; calibrate it with --debayer first` | Stack first: background works on stacks, not raw frames. |
| `polynomial degree must be 0 to 6` | Use a spline for a more complex surface. |
| The nebula got dimmer | The model follows the target: lower `--degree`. |

[Tutorial 5](../tutorials/background.md)

## Stretch and star removal

| Symptom | Cause and fix |
|---|---|
| `give both --black and --white, or neither` | Both points are needed. With a curve that has its own parameters, add `--curve mtf --midtone M`, or `arcsinh` or `ghs`. |
| `the image records no stretch (no STRETCH keyword)` | `--invert` on a file not made by `valerian stretch`. |
| A stretched file stretched again gives a grey, flat picture | The command does not detect it. Start from the linear file. |
| 33 per cent of the picture filled by `starless` (`5444 stars found, 33.12 % of the pixels filled`) | `--threshold` too low: noise taken for stars. |

[Tutorial 6](../tutorials/stretch.md), [tutorial 7](../tutorials/star-removal.md)

## Plate solve and colour

| Symptom | Cause and fix |
|---|---|
| `no match in 2177 patches of sky: the field is not where the hint says, the scale is outside 0.10 to 0.20 arc seconds per pixel, or there are too few catalogue stars in the field (this catalogue goes to G = 11)` | Wrong position or scale, or too few catalogue stars: use `--blind` with `--scale-range`, or a deeper catalogue. A Seestar S50 is 2.375 arc seconds per pixel. |
| `only 0 stars were detected; at least 10 are needed` | The image has too few stars (a dark frame, a clouded frame, a gradient model). |
| `this image has been stretched; colour calibration works on the linear image` | Run `photocal` before `stretch`. |
| `has no world coordinate system and the header lacks a position and scale to solve one` | Run `valerian solve` first, or give `--wcs`. |
| `colour calibration needs at least 15 usable stars and found 11 of 11 measured: the field is sparse for the catalogue, or the stars are too faint for --min-snr 15. Lower --min-stars or --min-snr, or build a deeper catalogue` | A sparse field: a galaxy, or a poor stack. The plate solve worked; the colour fit has too few stars. The NGC 253 stack of 411 Seestar frames has 11, and `--min-stars 8 --min-snr 8` gave a fit of 9 stars with scatter 0.005 and 0.012 mag. [Tutorial 19](../tutorials/seestar-faint-field-gpu.md#step-2-the-colour-calibration-that-stops) |
| `colour calibration needs at least 40 usable stars and found 26 of 26 measured: ...` | The same message with `--min-stars 40` given: the limit is above what the field offers. Lower `--min-stars` or `--min-snr`, or build a deeper catalogue. |
| A solve that matches only 12 stars, rms 0.56 arc seconds | Fine: that is a quarter of a pixel. Few matches is the warning that the colour fit will have few stars too. |

[Tutorial 8](../tutorials/plate-solve.md), [tutorial 9](../tutorials/photometric-colour.md)

## Deconvolve

| Symptom | Cause and fix |
|---|---|
| `only 0 stars are fit to measure a PSF and 8 are needed; lower --psf-min-snr, or give the PSF with --psf-fwhm` | Too few clean, isolated stars (a nebula-filled field, or a picture with no stars). Lower `--psf-min-snr` or `--threshold`, or give a Gaussian PSF width with `--psf-fwhm`. |
| Bright stars have a dark, cyan-tinged core | Deconvolving saturated or nearly saturated stars does that. 20 iterations show it most: `--iterations 8` made the median M 34 star 2.20 pixels instead of 2.09, ran in 6.4 s instead of 14.0 s, and gave shallower cores. |
| Stars are narrower but not rounder | `--mode round` (the default) does not always bring the eccentricity down. On M 34 it went from 0.106 to 0.063; the Seestar's own stack is 0.042. `--mode full` is sharpest and leaves more elongation. |
| Noisy sky after sharpening | Raise `--tv`, or lower `--iterations`. |

[Tutorial 16](../tutorials/deconvolution.md), [tutorial 18](../tutorials/seestar-session.md)

## Drizzle

| Symptom | Cause and fix |
|---|---|
| `9.263 % of the output without data` | Too few or too little-dithered frames for the scale: use a larger `--pixfrac` or a smaller `--scale`, or more frames. |
| `pixfrac must be above 0 and at most 1 (the drop is that fraction of a pixel)` | `--pixfrac` out of range. |
| `the drizzle scale must be 0.25 to 8` | `--scale` out of range. |

[Tutorial 10](../tutorials/drizzle.md)

## GPU

| Symptom | Cause and fix |
|---|---|
| `--backend gpu: no suitable GPU adapter found` | No adapter at all. Use `--backend cpu`. |
| `gpu: llvmpipe (...) (Vulkan, Cpu)` and a slow stack | Only the software adapter exists. `--backend auto` uses the CPU instead; `--backend gpu` forces the software adapter and says it is slower. |
| `GPU: 549 calls ran on the device, 3 fell back to the CPU (184 frames is more than the 128 the kernel holds)` | A note, not an error: the card combines at most 128 frames at once and the processor does the rest. The result is the same. |
| `--backend gpu` is slower than `--backend cpu` | Normal for Seestar-sized frames: 120 frames took 15.4 s on the processor and 19.6 s on the card on one test machine. Time both on your own frames and keep the faster. [Tutorial 19](../tutorials/seestar-faint-field-gpu.md#step-3-what-the-graphics-card-does) |
| `--frames must be 3 to 128` | `valerian bench` limit. |

[Tutorial 11](../tutorials/gpu.md)

## Any command

| Symptom | Cause and fix |
|---|---|
| `failed printing to stdout: Broken pipe` | A command piped to `head` now ends quietly; older builds panicked. Update Valerian if you still see it. |
| Out of disk space | Registered and calibrated copies are 32-bit float: 1.4 GB for sixty Seestar frames, 65 MB per ASI1600 frame. For a stack, see `--scratch-dir` above. |
| Output files have no site position or device code | Right: Valerian drops them from everything it writes. `--keep-location` keeps them ([Privacy](../user-guide/privacy.md)). |
