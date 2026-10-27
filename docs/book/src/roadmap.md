# Roadmap

The plan, in the order it is being built. The overall scope and every decision
so far are kept in the project's issues on [GitHub](https://github.com/qisback/valerian/issues).
Nothing below is available until its issue is closed and this page says so;
dates are deliberately not given.

## V1-foundation (built)

1. Workspace, continuous integration, licensing checks and documentation.
2. Core engine on the CPU: FITS input and output, calibration, star detection,
   registration, and stacking with outlier rejection and weighting.
3. The `valerian` command line (`calibrate`, `register`, `stack`, `info`) and a
   minimal desktop shell with the About page.

## After the foundation

Deconvolution. GPU kernels for calibration, the registration warp and the stack combiner (`--backend gpu`, `valerian bench`) are done, and pay off for a stack that keeps its frames on the device. Drizzle (`valerian drizzle`: variable-pixel linear reconstruction of dithered frames, mosaics dropped without interpolation) is done on the CPU. Plate solving (`valerian solve`: a TAN or TAN-SIP
WCS against a bundled Gaia catalogue, hinted or blind) and stretching
(`valerian stretch`: midtone, arcsinh and generalised hyperbolic curves, with a
live preview in the desktop app) and background extraction (`valerian
background`: polynomial and thin-plate spline surfaces) and photometric colour
calibration (`valerian photocal`: white balance fitted against Gaia BP-RP
colours) are done on the CPU;
hand-placed background samples wait for the background tool in the window. The desktop
app has the approved design: a workflow rail of five steps (calibrate, register, stack,
process, export), notices, an image view with zoom, pan and a histogram, and a night-vision
palette, with every tool above in its panels. Background extraction and mosaics are next.

## Shared code with Helia

For now Valerian uses Helia's crates as pinned git dependencies. Moving the
common code into a shared workspace is tracked in
[the shared-code issue](https://github.com/qisback/valerian/issues).
