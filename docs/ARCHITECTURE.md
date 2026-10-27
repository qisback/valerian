# Architecture

Valerian turns a set of deep-sky subframes into a finished image. This page is
the map: which crate does what, how data flows, and the rules that keep the CPU
and GPU paths honest. The foundation milestone has landed the workspace; each
section says what exists today.

## Crate map

| Crate | Kind | Role |
|---|---|---|
| `valerian-core` | library | Image maths and file formats on the CPU. No GPU code, `#![forbid(unsafe_code)]`. Builds on `helia-core`. |
| `valerian-gpu` | library | wgpu compute backend on the device `helia-gpu` opens: WGSL kernels for calibration, the registration warp and the stack combiner, a fused stack that keeps frames on the device, and the benchmark. Implements `valerian_core::compute::Compute`. |
| `valerian-science` | library | The science outputs: SubframeSelector-style subframe analysis (measurements, limits, approve/reject table, CSV and JSON), the Seestar sub grader (`grader.rs`: a small ONNX neural network run by the pure-Rust `tract` runtime, whose contract is in the developer guide), optics (star field, FWHM and eccentricity maps, tilt hint, aberration inspector, Moffat star profile), integration diagnostics (rejection, count and weight maps, the noise against the square root of the number of frames from split halves, channel histograms), plate-solve outputs (the annotated image with the bundled NGC, IC and Messier list, the residuals of the solution), the colour-calibration scatter, aperture photometry across frames with its light curve, small least-squares tools, pure-Rust plotting (`plotters`, drawn onto an RGB buffer and encoded as PNG) and the self-contained HTML report. Depends on `valerian-core` only; the CLI and the app both call it. |
| `valerian-cli` | binary `valerian` | `calibrate`, `register`, `stack`, `background`, `stretch`, `subframes`, `plot` and `info`, with a per-frame table; the clap definition also generates the CLI reference. |
| `valerian-gui` | binary `valerian-gui` | The desktop app: night-sky and night-vision themes, embedded IBM Plex fonts, the workflow rail (`rail`), the folder steps Calibrate, Register and Stack (`pipeline`), the image view with zoom, pan and histogram (`view`), the Process tools, Export, notices and the About page. |
| `xtask` | binary | Repository automation: generated documentation and the licensing check. |

`valerian-core` modules today:

* `about`: the author, copyright, licence and dedication notices, and the build
  record (commit, date, profile, target, compiler) that `build.rs` captures.
  Shared by `--version` and the About page, so they cannot drift apart.
* `fits`: FITS reader and writer that keep the whole header and the physical
  pixel values in ADU (the reader in `helia-core` drops the header and scales
  to 0..1). `Image` is a header plus one or more `helia_core::Frame` planes.
  Rows stay in file order.
* `stats`: median, MAD, percentiles, clipped statistics and noise estimates
  that skip NaN (NaN means "no data" throughout).
* `compute`: the `Compute` trait through which calibration, the warp and the combiner
  (and a whole stack plane) are run, with `Cpu` as the reference implementation that a
  GPU engine must match, and `stack_plane_classic`, the specification of the fused
  stack.
* `combine`: the per-pixel combiner under masters and stacks: min/max, sigma,
  winsorised sigma and linear fit rejection, weights, and per-frame rejection
  counts; with `Options::maps` also per-pixel low and high rejection counts and
  the summed weight of the samples kept.
* `calibrate`: master bias, dark, flat and dark flat, the hot/cold/dead pixel
  map, cosmetic correction, dark scaling and `Calibrator`.
* `streaming`: the same masters as `calibrate`, built from files a band of rows
  at a time with a byte budget, so memory does not grow with the number of
  frames. `fits::Reader` reads rows of a plane without reading the file.
* `debayer`: bilinear and Malvar-He-Cutler demosaicing to three planes.
* `background`: sky-gradient extraction: tile samples, a polynomial or
  thin-plate spline surface fitted with rejection, star and nebula masks, the
  target mask, subtract or divide, and the rules that decline a fit when there is
  no clear sky.
* `stars`: background model, star detection, windowed centroids, FWHM and
  ellipticity.
* `transform`: similarity, affine and homography transforms and their
  least-squares fits.
* `register`: triangle matching with voting and a RANSAC search, then a
  least-squares refinement on every matching star.
* `stack`: `align` (reference choice, registration, resampling, level matching), `integrate` (weights, rejection, header) and `stack` for both: reference choice, registration,
  resampling, level matching, noise and FWHM weights, rejection, and a
  per-frame report.
* `drizzle`: variable-pixel linear reconstruction on a finer grid: registration
  through `stack::register_frames`, square drops mapped through each frame's
  transform and clipped exactly against the output pixels (banded across
  threads), raw Bayer mosaics dropped into their own colour plane, and a second
  pass that rejects pixels outside the range of the first result read back.
* `warp`: resampling through a transform (bilinear, bicubic, Lanczos-3), NaN
  outside the source footprint.
* `stretch`: levels (black and white points) and the tone curves: midtone
  transfer function, arcsinh and the generalised hyperbolic stretch, each with
  its inverse, and an automatic stretch that puts the sky at a target. The
  stretch is recorded in the header so it can be undone.
* `testutil` (feature `testutil`): a seeded random generator, a pixel-integrated
  Gaussian star field and a model sensor (bias, dark current with hot pixels,
  flat with vignetting and dust, dead pixels, read and shot noise) for
  known-answer tests.

## Data flow

Subframes are read by FITS input, calibrated with master bias, dark and flat
frames, debayered when they come from a one-shot-colour camera, searched for
stars, registered to a reference by matching star patterns, and stacked with
outlier rejection and weighting. That much exists, on the CPU. Plate solving,
colour calibration and star removal follow; stretching exists as `valerian
stretch` and background extraction as `valerian background`, both on finished
stacks.
The CPU implementation is the reference that any GPU kernel must match.

## Relationship to Helia

Valerian is the deep-sky sibling of [Helia](https://github.com/qisback/helia).
Until the shared code is extracted into a common workspace
([tracked on GitHub](https://github.com/qisback/valerian/issues)), Valerian
depends on `helia-core` and `helia-gpu` as git dependencies pinned to a commit in
the `valerian-core` and `valerian-gpu` manifests. The same lint set, formatting, `deny.toml`, licensing
headers, documentation rules and CI shape apply.

## Licensing

Every source file carries an SPDX header and a pointer to
`ADDITIONAL-TERMS.md` (GPL section 7). `cargo xtask reuse-check` enforces the
headers, the `REUSE.toml` annotations for everything else, and that the
copyright line and the dedication stay in the README, the terms, the book
introduction and `crates/valerian-core/src/about.rs`.
