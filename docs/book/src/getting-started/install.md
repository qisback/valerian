# Getting started

**Not sure how to get Valerian?** The [Get Valerian](download.md) page lists the ways, including
[building from source](build-from-source.md). This page has the detail.

Valerian is at its foundation milestone. This page says what you can do with it
today, how to get it and how to check that it runs. The next page,
[A first session](../user-guide/walkthrough.md), runs it from raw frames to a finished
picture with the real output and the real pictures at every step.

## What works today, and what does not

Works, on FITS files, mono or one-shot colour: `valerian calibrate`, `register`,
`stack`, `drizzle`, `background`, `stretch`, `starless`, `solve`, `photocal`, `bench` and
`info`. The GPU kernels for calibration, the warp and the stack combiner are used by
`--backend gpu` (and by `stack` on a real graphics card; see
[The GPU kernels](../user-guide/gpu.md)). The desktop app has the five steps (calibrate, register, stack, process, export) with the
tools for stretching, star removal, plate solving, colour calibration, drizzle and the GPU, a
night-vision palette and the About page; its screenshots in this manual are made by its
tests from real data. Not built yet: mosaics, deconvolution, and background extraction in
the window.
See the [roadmap](../roadmap.md).

## Installers and packages

Valerian is a pre-release. Installers are sold on
[12hobbies.studio](https://12hobbies.studio/projects/valerian/) (coming soon); you can also
[build it from source](build-from-source.md). Each version comes with the files below. Pick **one** row
for your system:

| System | Install with | File | Status |
|---|---|---|---|
| Windows 10 or 11, 64-bit | the installer | `valerian-<version>-x86_64-pc-windows-msvc-setup.exe` | available |
| Windows | unzip and run | `valerian-<version>-x86_64-pc-windows-msvc.zip` | available |
| Debian, Ubuntu, Raspberry Pi OS | `.deb` | `valerian_<version>_amd64.deb`, `valerian_<version>_arm64.deb` | available |
| Fedora, RHEL and relatives | `.rpm` | `valerian-<version>-1.x86_64.rpm`, `valerian-<version>-1.aarch64.rpm` | available |
| Any Linux with glibc 2.35 or newer | AppImage | `Valerian-<version>-x86_64.AppImage`, `Valerian-<version>-aarch64.AppImage` | available |
| Any Linux | unpack the archive | `valerian-<version>-<arch>-unknown-linux-gnu.tar.gz` | available |
| macOS | disk image | `Valerian-<version>-macos.dmg` | [coming](#macos) |

Every file has its line in `SHA256SUMS`. Check a download before
you run it:

```sh
sha256sum -c SHA256SUMS --ignore-missing
```

On Windows: `(Get-FileHash <file> -Algorithm SHA256).Hash.ToLower()` and compare
it with the line in `SHA256SUMS`, or with the `.sha256` file beside the zip.

### Linux on 64-bit ARM

The `arm64` and `aarch64` downloads are built and tested natively on a 64-bit ARM
machine (an Ampere server, not an emulator), against the same glibc 2.35 baseline as
the x86-64 ones. They are meant for:

* Raspberry Pi 5 (and Pi 4) running a 64-bit Raspberry Pi OS or Ubuntu;
* Ampere and AWS Graviton servers, for headless `valerian` runs;
* arm64 laptops and mini PCs running Linux (Snapdragon X, Apple silicon under Asahi).

The release checks run each binary from the `.deb`, `.rpm` and AppImage, and install the
`.deb` in a clean Debian 12 arm64 container. They use a software GPU (Mesa lavapipe), so
GPU speed on a Pi's own graphics chip is not measured: report what you find.

### Windows

Run `valerian-<version>-x86_64-pc-windows-msvc-setup.exe`.

**The first run shows a SmartScreen warning.** The installer is not code-signed
yet, so Windows says "Windows protected your PC" and "Unknown publisher". After
you have checked the checksum, choose **More info**, then **Run anyway**.

The installer shows the GPL and the additional terms, then asks **Who is this
for?** The default, **Just for me**, installs to
`%LOCALAPPDATA%\Programs\Valerian` and needs no administrator rights; **All
users** installs to `C:\Program Files\Valerian` and asks Windows for permission.
It offers a Start menu shortcut (on), a desktop shortcut (off) and adding the
`valerian` command to your PATH (off). Valerian appears in **Settings > Apps >
Installed apps** and uninstalls from there. Double-clicking `valerian-gui.exe`
opens the window with no console behind it; `valerian.exe` is the command line
tool and stays a console program.

For many machines, from a terminal: `valerian-<version>-x86_64-pc-windows-msvc-setup.exe /S /ADDPATH /DESKTOP`.
The switches are `/S`, `/AllUsers` (needs an administrator), `/ADDPATH`, `/DESKTOP`,
`/NOSTART`, `/NOASSOC` and `/D=C:\Where\To\Install` (last, no quotes).

It also lists Valerian under **Open with** for `.fits`, `.fit` and `.fts` files (on by
default, never the default program). **Uninstall:** Settings > Apps > Installed apps >
Valerian > Uninstall. It removes the programs, shortcuts, **Open with** entries and PATH
entry and leaves your settings (`%APPDATA%\valerian`) and images alone.

### Debian, Ubuntu and Raspberry Pi OS

```sh
sudo apt install ./valerian_<version>_amd64.deb      # arm64 on a Raspberry Pi
```

You get `valerian` and `valerian-gui` in `/usr/bin`, a **Valerian** entry in the
applications menu and Valerian in the **Open with** list for FITS files. **Uninstall:** `sudo apt remove valerian`.

### Fedora and other RPM systems

```sh
sudo dnf install ./valerian-<version>-1.x86_64.rpm   # aarch64 on ARM
```

**Uninstall:** `sudo dnf remove valerian`. The RPM has been unpacked and checked on
Debian but not installed on a Fedora machine, because the project has none;
report what you find.

### AppImage

```sh
chmod +x Valerian-<version>-x86_64.AppImage
./Valerian-<version>-x86_64.AppImage                  # the app
./Valerian-<version>-x86_64.AppImage --cli --version  # the command line tool
```

If it says it cannot mount itself, your system lacks FUSE: run it with
`APPIMAGE_EXTRACT_AND_RUN=1` or install `libfuse2`. **Uninstall:** delete the file.

### macOS

**A macOS disk image**, `Valerian-VERSION-macos.dmg`, is not available yet; build from
source ([guide](build-from-source.md)). A disk image would not
be signed or notarised until the project has an Apple Developer ID, so Gatekeeper
will say Valerian "cannot be opened because the developer cannot be verified".
To open it the first time: drag Valerian to Applications, **right-click (or
Control-click) it and choose Open**, then **Open** again in the dialog; or open
**System Settings > Privacy & Security** and choose **Open Anyway** next to the
message about Valerian. You only do this once. **Uninstall:** drag it to the Bin.

### The plain archives

The `.tar.gz` and `.zip` archives hold the same programs without an installer:

```sh
tar -xzf valerian-<version>-x86_64-unknown-linux-gnu.tar.gz
./valerian-<version>-x86_64-unknown-linux-gnu/valerian --version
```

Each holds `valerian`, `valerian-gui`, the licences (`LICENSE`,
`ADDITIONAL-TERMS.md`, `LOGO-LICENCE.md`, `THIRD-PARTY-LICENSES.txt`), the README
and the changelog. Every package carries the same licences: in
`/usr/share/doc/valerian` for the `.deb` and `.rpm`, inside the AppImage, in the
install folder on Windows. The Linux downloads need glibc 2.35 or newer (Ubuntu
22.04, Debian 12, Fedora 36 or later) and a Vulkan driver if you want the GPU
adapter report. The window size and the last eight images you opened (the **Recent** menu
in the top bar) are remembered between runs, and `valerian-gui image.fits` opens an image.
`valerian solve` reads a star catalogue, `gaia-dr3-g11.vcat` (Gaia DR3 to G = 11, 8.7 MB, licence CC BY-NC
3.0 IGO, non-commercial use, shipped with its licence text). Every package puts it where the program looks: beside the
programs (Windows, archives), `/usr/share/valerian` (`.deb`, `.rpm`, AppImage). `valerian info` prints
the `catalogue:` line it found; if you move the programs elsewhere, copy the file with them or point
`VALERIAN_CATALOGUE` at it. Or build
it from source: [Build from source](build-from-source.md).

## Build it

The full guide, for Windows, macOS, Debian, Ubuntu, Raspberry Pi, Fedora, Arch and other Linux
systems, is on its own page: [Build from source](build-from-source.md). In short, install a current
stable Rust and the system packages listed there, then:

```sh
git clone https://github.com/qisback/valerian.git
cd valerian
cargo build --release --locked -p valerian-cli -p valerian-gui
./target/release/valerian --help
```

The first build needs network access to `github.com`, because Valerian
uses Helia's core and GPU crates as pinned git dependencies.

```text
Deep-sky image processing

Usage: valerian <COMMAND>

Commands:
  calibrate   Calibrate light frames with bias, dark, flat and dark-flat frames
  register    Register frames to a reference and write them on its grid
  stack       Register, normalise, weight and combine frames into one image
  drizzle     Combine dithered frames onto a finer grid with drizzle
  bench       Time the GPU kernels against the CPU code and check they agree
  background  Model and remove the sky gradient of an image
  stretch     Stretch a linear image to a viewable one (MTF, arcsinh or GHS)
  starless    Remove or reduce the stars of an image
  solve       Plate-solve an image and write its world coordinate system (TAN-SIP)
  photocal    Calibrate the colour of an image against Gaia star colours
  info        Describe FITS files, or this build and the machine it runs on
  help        Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

## Check it runs

```sh
valerian info
```

```text
valerian 0.5.0
commit: unknown
built: 2026-10-19 20:20 UTC
profile: release
target: x86_64-unknown-linux-gnu
rust: rustc 1.99.1 (58cf8f9c3 2026-10-13)
wgpu: 30.0.1
```

With no file, `info` describes the build. Add `--gpu` to open the graphics adapter
and report it (or `--gpu --software` for the software one).

## Your frames

Valerian reads **FITS** files: lights, and, if you have them, darks, flats, dark
flats and bias frames. Keep them in folders, one folder per kind; every argument
that takes frames takes a file or a folder of files, and can be repeated. If you
have no data yet, [the first session](../user-guide/walkthrough.md) ends with a small
synthetic session you can make with one script and use to try the commands.

## The desktop app

```sh
cargo run --release -p valerian-gui
```

opens a window in the night-sky colours (or the red night-vision palette, F2) with a top
bar, the workflow rail, the image view and the step's panel, and the **About** page (Ctrl+F1). See [The desktop app](../user-guide/gui.md) and
[About Valerian](../about.md).
