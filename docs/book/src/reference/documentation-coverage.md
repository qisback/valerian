# Documentation coverage

Every user-visible feature should have three things: a **tutorial** (steps on a real
capture, the exact settings, what to expect, what goes wrong), **reference** (every
option and control, with ranges and defaults) and a **changelog entry** that says how
the change works and what it measured, linking the tutorial. This page is the audit
of where Valerian stands, written from the code and the pages as they are, not as
we would like them to be. `cargo xtask docs --check` fails if a command line option
is not mentioned in the user guide, or if a row below links a page that does not exist.

How to read the verdicts:

* **Tutorial**: *Partial* means the feature appears in a numbered step of
  [A first session](../user-guide/walkthrough.md) with its real output, but there is
  no page that walks it on its own with the settings explained, expected result and
  troubleshooting. *Missing* means no worked example at all.
* **Reference**: the generated [CLI reference](cli.md) lists every option with its
  default; *Done* means the user-guide page also explains the choices and ranges.
* **Changelog how**: *Partial* means the entry names the method but gives no measured
  effect or no pointer to the book.
* **Owner**: `V-B` is the agent writing the tutorials for the rows it names; it will
  flip those rows. `#14` is the desktop app issue. Blank means this audit's author
  (agent V-A).

| Feature | Tutorial | Reference | Changelog how | Status | Owner |
|---|---|---|---|---|---|
| Install: Windows, Debian, Fedora, AppImage | Done: [Tutorial 1](../tutorials/install.md) | Done | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Calibrate: master bias, dark, flat, dark flat, streaming | Done: [Tutorial 2](../tutorials/calibrate.md) | Done: [How calibration works](../user-guide/calibration.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Dark scaling (`--scale-dark`) | Done: [Tutorial 2](../tutorials/calibrate.md#troubleshooting) | Done | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Hot and cold pixels (`--hot-sigma`, `--cold-sigma`, `--auto-cosmetic`) | Done: [Tutorial 2](../tutorials/calibrate.md#troubleshooting) | Done | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| One-shot colour calibration and demosaic (`--debayer`, `--flat-per-channel`) | Done: [Tutorial 3](../tutorials/register.md) and [Tutorial 4](../tutorials/stack.md) on Seestar frames, which need no calibration; [Tutorial 2](../tutorials/calibrate.md) for mono | Done | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Register (`valerian register`) | Done: [Tutorial 3](../tutorials/register.md) | Done: [How registration works](../user-guide/registration.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Stack (rejection, weights, normalisation, report, coverage) | Done: [Tutorial 4](../tutorials/stack.md) | Done: [How stacking works](../user-guide/stacking.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Background extraction | Done: [Tutorial 5](../tutorials/background.md) | Done: [How background extraction works](../user-guide/background.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Stretch: auto, MTF, arcsinh, GHS, preview, invert | Done: [Tutorial 6](../tutorials/stretch.md) | Done: [How stretching works](../user-guide/stretching.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Star removal (`valerian starless`) | Done: [Tutorial 7](../tutorials/star-removal.md) | Done: [How star removal works](../user-guide/star-removal.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Plate solving, WCS, blind search, SIP | Done: [Tutorial 8](../tutorials/plate-solve.md) | Done: [How plate solving works](../user-guide/plate-solving.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Photometric colour calibration | Done: [Tutorial 9](../tutorials/photometric-colour.md) | Done: [How colour calibration works](../user-guide/colour-calibration.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| Subframe analysis (`valerian subframes`, `valerian plot subframes`, `--approve`, `--science-report`) and the Science panel | Done: [Tutorial 12](../tutorials/subframe-analysis.md) | Done: [How subframe analysis works](../user-guide/subframes.md) | Done: the Unreleased entry says how and gives the measured effect | Done | |
| Seestar sub grader (`--grade`, `--min-grade`, `--reject-defect`, `--grader-model` and `--grade-weights` of `subframes` and `stack`) and its Science panel controls | Done: the walk-through is in the user guide page | Done: [How the Seestar sub grader works](../user-guide/grading.md), [The grader model contract](../developer/grader-model.md) | Done: the Unreleased entry says how | Done | |
| Optics (`valerian optics`, `valerian plot aberration`, `fwhm-map`, `eccentricity-map`, `tilt`, `psf`) and the Optics tool | Done: [Tutorial 13](../tutorials/optics.md) | Done: [How the optics plots work](../user-guide/optics.md) | Done: the Unreleased entry says how and gives the measured effect | Done | |
| Integration diagnostics (`--science-report` of `stack` and `drizzle`, `valerian plot rejection-low`, `rejection-high`, `weight-map`, `count-map`, `noise`, `histogram`) | Done: [Tutorial 14](../tutorials/integration.md) | Done: [How the integration diagnostics work](../user-guide/integration.md) | Done: the Unreleased entry says how and gives the measured effect | Done | |
| Astrometry and photometry (`valerian annotate`, `valerian photometry`, `valerian photocal --report`, `valerian plot annotate`, `residuals`, `residual-map`, `pcc`, `zeropoint`, `limiting-mag`, `lightcurve`, `photocal --calibrated-stars`) and the Astrometry tool | Done: [Tutorial 15](../tutorials/astrometry.md), [Tutorial 20](../tutorials/zero-point-and-limit.md) | Done: [How the astrometry and photometry outputs work](../user-guide/astrometry.md) | Done: the Unreleased entry says how and gives the measured effect | Done | |
| Drizzle | Done: [Tutorial 10](../tutorials/drizzle.md) | Done: [How drizzle works](../user-guide/drizzle.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| GPU backend and `valerian bench` | Done: [Tutorial 11](../tutorials/gpu.md) | Done: [The GPU kernels](../user-guide/gpu.md) | Done: the 0.1.0 and 0.2.0 entries say how and give the measured effect | Done | |
| `valerian info` (files, `--stats`, `--gpu`, build record) | Partial: `--stats` in the walkthrough | Partial: no page explains the output lines | Partial | Partial | |
| Troubleshooting by stage | Done: every tutorial ends with its own | Done: [Troubleshooting by stage](troubleshooting.md) | Not applicable | Done | |
| FITS keywords read and written | Not applicable | Done: [FITS files and other formats](fits-files.md) | Not applicable | Done | |
| File formats read and written | Not applicable | Done: [FITS files and other formats](fits-files.md) | Not applicable | Done | |
| Star catalogue file (`.vcat`) format and search path | Not applicable | Done: [The star catalogue file](catalogue-format.md) | Done: 0.2.0 | Done | |
| Environment variables and exit status | Not applicable | Done: [Environment variables and exit status](environment.md) | Not applicable | Done | |
| Desktop app: workspace, open, recent files, window size | Partial: [The desktop app](../user-guide/gui.md) | Partial | Done | Partial | #14 |
| Desktop app: Stretch tool | Partial: [How stretching works](../user-guide/stretching.md) has a section | Partial: controls described in prose, no ranges table | Partial | Partial | #14 |
| Desktop app: Star removal tool | Partial: section in the star removal page | Partial | Partial | Partial | #14 |
| Desktop app: Plate solve tool | Partial: section in the plate solving page | Partial | Partial | Partial | #14 |
| Desktop app: Colour tool | Partial: section in the colour page | Partial | Partial | Partial | #14 |
| Desktop app: Drizzle tool | Partial: section in the drizzle page | Partial | Partial | Partial | #14 |
| Desktop app: GPU tool | Partial: section in the GPU page | Partial | Partial | Partial | #14 |
| Desktop app: file chooser | Missing | Missing | Done | Partial | #14 |
| Release process, CI lanes, Python comparison tests | Not applicable | Done: in the source repository | Done | Done | |
