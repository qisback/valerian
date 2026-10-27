# Introduction

Valerian is open-source, GPU-accelerated deep-sky image processing. It
takes the subframes you capture of galaxies, nebulae and clusters and turns
them into a finished image: calibration, star registration, stacking with
outlier rejection, plate solving, gradient removal, photometric colour
calibration, stretching and star removal, on Linux, Windows and macOS, on
x86_64 and ARM.

Valerian is written in Rust and is the deep-sky sibling of
[Helia](https://github.com/qisback/helia), the solar stacker. The two
share their core: file formats, GPU acceleration, the interface and the release
pipeline.

Why it exists, and how a batch-and-review habit from high-performance computing shaped
it, is told in [Why I made Valerian](why-i-made-valerian.md).

Measured speeds, with graphs, are on [Performance and benchmarks](user-guide/benchmarks.md).

## Dedication

Valerian is dedicated to my son Xaviar. His middle name is Valerian, because he
was a very sleepy child, which suits software made for the long, quiet hours of
the night sky.

## What works today

This is the foundation milestone. The book grows with the program, and each page
says plainly what does not work yet. Today, on the command line and as a library:

* `valerian calibrate`: master bias, dark, flat and dark-flat frames with
  sigma, winsorised or linear-fit rejection, dark scaling, hot, cold and dead
  pixel repair, optional demosaicing (see [How calibration works](user-guide/calibration.md));
* `valerian register`: star detection, triangle matching with a RANSAC fit
  (similarity, affine or homography) and resampling onto a reference grid (see
  [How registration works](user-guide/registration.md));
* `valerian stack`: reference choice, sky and transparency matching, weights by
  noise and star width, and per-pixel outlier rejection (see
  [How stacking works](user-guide/stacking.md));
* `valerian background`: a polynomial or thin-plate spline fitted to tile
  samples of the sky, with star and nebula masks, subtracted or divided out;
  declined when there is no clear sky to measure (see
  [How background extraction works](user-guide/background.md));
* `valerian stretch`: levels and a midtone, arcsinh or generalised hyperbolic
  curve, recorded in the header so it can be undone (see
  [How stretching works](user-guide/stretching.md));
* `valerian starless`: classical star removal, with the stars and the mask kept
  (see [How star removal works](user-guide/star-removal.md));
* `valerian solve`: a TAN or TAN-SIP world coordinate system against a bundled
  Gaia catalogue (see [How plate solving works](user-guide/plate-solving.md));
* `valerian photocal`: white balance fitted against Gaia star colours (see
  [How colour calibration works](user-guide/colour-calibration.md));
* `valerian drizzle`: drizzle for dithered, undersampled frames (see
  [How drizzle works](user-guide/drizzle.md));
* `valerian bench` and `--backend gpu`: GPU kernels for calibration, the
  registration warp and the stack combiner (see [The GPU kernels](user-guide/gpu.md));
* `valerian info`: header, noise and star statistics of FITS files, the build
  record and the GPU adapter;
* a desktop app with the five steps of a session (calibrate, register, stack, process and
  export) on a rail, an image view with zoom, pan and a histogram, notices while work runs,
  a red night-vision palette, the tools above in its panels, and the About page (see
  [The desktop app](user-guide/gui.md)).

Start with [Get Valerian](getting-started/download.md) (installers, or build from source),
then [Getting started](getting-started/install.md) and
[a first session](user-guide/walkthrough.md), which takes a monochrome and a
one-shot-colour session from raw frames to a finished picture with the real
output and the real pictures at every step: calibrate, register, stack, background,
stretch, star removal, plate solving, colour calibration and drizzle.
The [quick start](user-guide/quick-start.md) is the short version.

## Copyright and licence

Copyright (C) 2026 Laurence Horrocks-Barlow. Valerian is free software under the
GNU General Public License, version 3 or (at your option) any later version,
with no warranty, and with the additional terms in `ADDITIONAL-TERMS.md` that
keep the author's name and this dedication with the program. Valerian is made
by Laurence Horrocks-Barlow. If you are interested in anything else I make,
visit [12hobbies.studio](https://12hobbies.studio).
