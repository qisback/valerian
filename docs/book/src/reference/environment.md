# Environment variables and exit status

## Exit status

`valerian` exits 0 on success and 1 on any error, with the message on standard error and
nothing written for the failed step. A usage error (an unknown option, a missing required
argument) is reported by the argument parser and also exits non-zero. `--help` and
`--version` exit 0. Piping the output of a command into `head` ends it quietly when `head` exits
(older builds
panicked with "Broken pipe"). A command stopped that way may not have written its output file.

## Variables Valerian reads

| Variable | Read by | Meaning | Default |
|---|---|---|---|
| `VALERIAN_CATALOGUE` | `valerian solve`, `valerian photocal`, `valerian info` | Full path of the star catalogue (`.vcat`). It is the first place looked at when `--catalogue` is not given. See [The star catalogue file](catalogue-format.md). | unset |
| `VALERIAN_KEEP_LOCATION` | every `valerian` command | Set to `1`, `true` or `yes` to keep site coordinates, device serial numbers and unit identifiers in the headers of files written (the same as `--keep-location`). See [FITS files](fits-files.md). | unset: they are dropped |
| `VALERIAN_GUI_CONFIG` | `valerian-gui` | Full path of the settings file (window size and recent files). | `%APPDATA%\valerian\gui.conf` on Windows, else `$XDG_CONFIG_HOME/valerian/gui.conf`, else `~/.config/valerian/gui.conf` |

A missing or damaged settings file is not an error: the defaults apply and nothing is
written until something changes.

## Variables for tests and developers

| Variable | Meaning |
|---|---|
| `VALERIAN_GPU_TESTS` | `software` or `hardware`: which adapter the GPU parity tests open. |
| `VALERIAN_TEST_DATA`, `VALERIAN_TEST_OSC`, `VALERIAN_TEST_SOLVE` | Folders of real captures for the real-data tests; the tests skip when they are unset. The data is never committed. See [the source repository](https://github.com/qisback/valerian). |

## Variables that belong to other software

Valerian reaches the graphics card through Vulkan (Windows: DirectX 12), so the Vulkan
loader's own variables apply to it, for example `VK_DRIVER_FILES` to choose or hide a
driver. [Tutorial 11](../tutorials/gpu.md) uses it to show the fall-backs.
