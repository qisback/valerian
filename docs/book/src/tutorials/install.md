# Tutorial 1: Install Valerian

**Goal:** have a `valerian` you have checked, that opens, finds its star catalogue and can see
your graphics card (if you have one). It takes about five minutes and needs no data.

**What you need:** a Windows 10 or 11 (64-bit) PC, or a 64-bit Linux with glibc 2.35 or newer
(Ubuntu 22.04, Debian 12, Fedora 36 or later), and an installer or archive from 12hobbies.studio
(coming soon), or a build from source ([guide](../getting-started/build-from-source.md)). The Linux and Windows steps below were run on
release 0.5.0: Windows 11 for the Windows steps, a 64-bit Linux server for the Linux ones.

The full table of packages (`.deb`, `.rpm`, AppImage, the Windows installer and the
archives) is on the [getting started page](../getting-started/install.md); this tutorial follows
the two archive routes because they work the same everywhere, and says what differs for the
installers.

## Step 1: Download the file and its checksum

If you build from source, skip to [Step 4](#step-4-check-it-runs). Otherwise download **two** things from
your 12hobbies.studio purchase (coming soon) into one folder:

| System | File |
|---|---|
| Windows | `valerian-0.5.0-x86_64-pc-windows-msvc.zip` (or the `-setup.exe` installer) |
| Linux, x86-64 | `valerian-0.5.0-x86_64-unknown-linux-gnu.tar.gz` |
| Linux, ARM (Raspberry Pi 4 or 5 on a 64-bit system) | `valerian-0.5.0-aarch64-unknown-linux-gnu.tar.gz` |

and `SHA256SUMS`. `SHA256SUMS` has one line per file in the release: the checksum, two
spaces, the file name.

## Step 2: Check the download

A checksum catches a download that was cut short, corrupted or swapped. Do this before you
run anything.

**Linux:**

```sh
sha256sum -c SHA256SUMS --ignore-missing
```

```text
valerian-0.5.0-x86_64-unknown-linux-gnu.tar.gz: OK
```

`--ignore-missing` is there because `SHA256SUMS` lists every file of the release and you
downloaded one. The word to look for is `OK`. To see what a bad download looks like, this is
the same check on a copy with one byte added to the end:

```text
bad.tar.gz: FAILED
sha256sum: WARNING: 1 computed checksum did NOT match
```

Delete a file that says `FAILED` and download it again. Do not run it.

**Windows (PowerShell):**

```powershell
(Get-FileHash valerian-0.5.0-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()
```

```text
9609fa94d721bd04f0c4fbd8260db02706cf18dcfd13b1889d08c9d53a8c813e
```

Open `SHA256SUMS` and compare it with the line for the same file name; they must be the same
64 characters (here the line begins `9609fa94d721...`). The Windows files also have a small
`.sha256` file beside them with the same value. 

## Step 3: Install it

**Windows, zip.** Right-click the zip, choose **Extract All**, and open the folder. It holds
`valerian.exe` (the command line tool), `valerian-gui.exe` (the desktop app), the star
catalogue `gaia-dr3-g11.vcat` and the licences. Nothing is installed and nothing is written
to the registry: delete the folder to remove it.

**Windows, installer.** Run `valerian-0.5.0-x86_64-pc-windows-msvc-setup.exe`. The installer is
not code-signed yet, so Windows shows "Windows protected your PC" with "Unknown publisher":
after the checksum matched, choose **More info**, then **Run anyway**. The default, **Just for
me**, installs to `%LOCALAPPDATA%\Programs\Valerian` and needs no administrator rights. The installer
offers to add the `valerian` command to your `PATH`, which you want if you intend to type `valerian`
in any terminal; it is off by default.

**Linux, archive:**

```sh
tar -xzf valerian-0.5.0-x86_64-unknown-linux-gnu.tar.gz
cd valerian-0.5.0-x86_64-unknown-linux-gnu
ls
```

```text
ADDITIONAL-TERMS.md
CC-BY-NC-3.0-IGO.txt
CHANGELOG.md
gaia-dr3-g11.vcat
LICENSE
LOGO-LICENCE.md
README.md
share
THIRD-PARTY-LICENSES.txt
valerian
valerian-gui
```

`valerian` is the command line tool, `valerian-gui` the desktop app and `gaia-dr3-g11.vcat`
the catalogue of 1,247,240 stars that [plate solving](plate-solve.md) needs. Keep the three
together. To type `valerian` anywhere, copy the folder somewhere permanent and add it to your
`PATH`, or install the `.deb` (`sudo apt install ./valerian_0.5.0_amd64.deb`) or `.rpm`
(`sudo dnf install ./valerian-0.5.0-1.x86_64.rpm`), which put the programs in `/usr/bin` and
the catalogue in `/usr/share/valerian`. On other distributions the AppImage runs without
installing: `chmod +x Valerian-0.5.0-x86_64.AppImage`, then
`./Valerian-0.5.0-x86_64.AppImage --cli --version`.

## Step 4: Check it runs

From the extracted folder (on Windows, in PowerShell inside the folder, as `.\valerian.exe`):

```sh
valerian --version
```

```text
valerian 0.5.0
commit: a7ed5a9a
built: 2026-10-20 17:37 UTC
profile: release
target: x86_64-unknown-linux-gnu
rust: rustc 1.99.1 (58cf8f9c3 2026-10-13)
wgpu: 30.0.1

Copyright (C) 2026 Laurence Horrocks-Barlow
Licence: GPL-3.0-or-later
...
```

The first line is the version, `commit` and `built` identify the exact build, and `target` is
the platform (`x86_64-pc-windows-msvc` on Windows). The licence text that follows is part of
the output on purpose: the [additional terms](../about.md) ask that it stays. A development
build says `commit: unknown` if it was made outside a git checkout.

If you are not in the folder, type the path: `./valerian` on Linux, `.\valerian.exe` on
Windows, or the full path. Now look at the machine:

```sh
valerian info --gpu
```

```text
valerian 0.5.0
commit: a7ed5a9a
built: 2026-10-20 17:37 UTC
profile: release
target: x86_64-unknown-linux-gnu
rust: rustc 1.99.1 (58cf8f9c3 2026-10-13)
wgpu: 30.0.1
catalogue: ~/Downloads/valerian-0.5.0-x86_64-unknown-linux-gnu/gaia-dr3-g11.vcat (1247240 stars)
gpu: NVIDIA A100 80GB PCIe (Vulkan, DiscreteGpu)
```

On the Windows 11 laptop the last two lines were:

```text
catalogue: C:\...\valerian-0.5.0-x86_64-pc-windows-msvc\gaia-dr3-g11.vcat (1247240 stars)
gpu: NVIDIA GeForce RTX 3050 Ti Laptop GPU (Vulkan, DiscreteGpu)
```

Two lines matter:

* **`catalogue:`** says the star catalogue was found beside the program, with its star count.
  `valerian solve` and `valerian photocal` need it.
* **`gpu:`** says which graphics adapter Valerian opened, the graphics interface it used
  (Vulkan, or Dx12 on Windows) and its type. `DiscreteGpu` is a graphics card. A type of `Cpu`
  means a software adapter that runs on the processor, which is slower than Valerian's own
  CPU code; see [tutorial 11](gpu.md).

Finally, list the commands:

```sh
valerian --help
```

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

`valerian COMMAND --help` lists the options of one command.

## Settings and environment

| Setting | What it does | Default |
|---|---|---|
| `valerian --version`, `-V` | build information and the licence text | |
| `valerian info` | the same build information plus `catalogue:`; no file needed | |
| `valerian info --gpu` | also opens the graphics adapter and reports it, or says why it could not | off |
| `valerian info --gpu --software` | opens the software (fallback) adapter instead: lavapipe on Linux, WARP on Windows | off |
| `VALERIAN_CATALOGUE` | environment variable: full path of the star catalogue, when it is not beside the program | unset |

## How to tell it worked

* `sha256sum -c` prints `OK` (or the PowerShell hash is the one in `SHA256SUMS`).
* `valerian --version` prints `valerian 0.5.0` or newer.
* `valerian info` prints a `catalogue:` line with a path and `(1247240 stars)`.

## Troubleshooting

**`FAILED` from `sha256sum`, or a hash that does not match.** The file is damaged or is not
the one the release published. Download it again, in a browser or with a tool that follows
redirects, and check again. A hash that differs only in letter case is the same hash (the
Windows command above lower-cases it for that reason).

**`valerian: command not found` (Linux) or `valerian is not recognised` (Windows).** The
program is not on your `PATH`. That is the default for both archives and for the Windows
installer unless you chose to add the command to your `PATH` in the installer. Run it by its path
(`./valerian`, `.\valerian.exe`), or add its folder to `PATH`. On Windows, open a new
terminal after you change `PATH`; terminals that were already open do not see it.

**`catalogue: not found` in `valerian info`, or `valerian: no star catalogue found; looked
for gaia-dr3-g11.vcat in: ...` from `valerian solve`.** The catalogue is not beside the
program. This was reproduced by copying `valerian` alone into an empty folder; the error lists
every place it looked (beside the program, in a `data` folder there, in `../share/valerian`,
and in `VALERIAN_CATALOGUE`). Copy `gaia-dr3-g11.vcat` next to the program, or set
`VALERIAN_CATALOGUE` to its full path (`export VALERIAN_CATALOGUE=/path/gaia-dr3-g11.vcat`, or
`$env:VALERIAN_CATALOGUE = 'C:\path\gaia-dr3-g11.vcat'` in PowerShell). Then
`valerian info` shows the path it uses.

**`gpu: unavailable (could not open GPU device: ...)`.** No graphics driver that supports
Vulkan (or DirectX 12 on Windows) was found. Valerian still works: commands choose the CPU
(`compute: CPU (no GPU: ...)`). To use a graphics card, install the vendor's current driver. On
a Linux server, check `vulkaninfo --summary` lists your card. See [tutorial 11](gpu.md#troubleshooting).

**Windows says "Windows protected your PC".** The installer and the executables are not
code-signed yet. After you have checked the checksum, **More info**, then **Run anyway**.

**The AppImage says it cannot mount itself.** The system has no FUSE. Run it with
`APPIMAGE_EXTRACT_AND_RUN=1 ./Valerian-0.5.0-x86_64.AppImage`, or install `libfuse2`.

**An error naming `GLIBC_2.35` on Linux** ("version `GLIBC_2.35' not found"). The system's C library is older than the build
machine's. Use a newer distribution release, or [build from source](../getting-started/install.md#build-it).

## The desktop app

`valerian-gui` (the Start menu entry **Valerian** on Windows) opens the window. Its **About** page (Ctrl+F1) shows the version and build lines, and its button that opens the GPU adapter adds it, the
same facts as `valerian info --gpu`. See [The desktop app](../user-guide/gui.md).

## Where next

[Tutorial 2](calibrate.md) calibrates a set of mono frames with darks, flats and dark flats;
if you shoot with a Seestar or any camera that has no calibration frames, skip to
[tutorial 3](register.md).
