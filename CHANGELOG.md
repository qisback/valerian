# Changelog

What is new in each release, newest first.

## [0.5.0]

- New photometry outputs: `valerian photocal --report DIR` now also writes zero points, a calibrated star catalogue, the limiting magnitude and an astrometric residual map.
- Fixed: `valerian deconvolve` no longer leaves black cores and dark crescents around bright stars, and `--mode round` now makes stars rounder.
- Fixed: the manual no longer says the Seestar S50 writes a row-order card (it does not), `valerian photocal` on a sparse field gives a clear message instead of "registration failed", and `valerian stack` on a folder no longer prints "read 1 frames".
- The GitHub repository carries the source; installers are sold at 12hobbies.studio, and a Build from source page covers Windows, macOS and Linux.

## Earlier releases

- Valerian is open-source software for processing deep-sky captures, including Seestar S50 sessions. It calibrates, registers and stacks frames, removes sky gradients, stretches, and works with FITS files.
- Measurement: subframe analysis, optics and aberration plots, integration diagnostics, plate solving, astrometry and photometry, and photometric colour calibration against Gaia.
- Processing: drizzle for dithered frames, star removal, deconvolution for narrower and rounder stars, and `valerian stack --mosaic` for multi-pane mosaics.
- Memory use no longer grows with the number of frames in a stack, and location and device serial numbers are no longer written to the files Valerian produces.
- A desktop app with a real-pixel viewer, a manual with tutorials on real sessions, and a download page.
