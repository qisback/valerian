# Desktop app reference

Every control of `valerian-gui`, with its range and default. [The desktop app](../user-guide/gui.md)
describes the window and [the tutorial](../user-guide/gui-tutorial.md) walks through a session.
Command line: `valerian-gui [FILE]` opens FILE; `-V` and `--version` print the version and exit.

## Top bar

| Control | Does |
|---|---|
| Logo, name | The About page (Ctrl+F1). |
| File chip | The open file, its size and planes (wide windows only). |
| Recent | The last 8 images opened (a menu; the start screen lists them too). |
| Night (F2 or N) | Red night-vision palette. Default off; remembered. |
| GPU | Opens the GPU benchmark; a step on the rail leaves it. |
| Open (Ctrl+O) | Native file dialog for a FITS image. Ctrl+Shift+O chooses a folder of frames to sort instead. |
| About (Ctrl+F1, Esc closes) | Dedication, version, copyright, licence. |

## Start screen

Shown in the middle of the window while no image is open (and the Export step has nothing to export).
It tells you how to start and lists what you worked on lately. See
[Tutorial: sort a folder of frames, and pick a session up again](../tutorials/sort-and-sessions.md).

| Control | Does |
|---|---|
| Recent sessions | Up to 8 sets of frame folders (lights, darks, flats, dark flats, bias), newest first. A session is remembered when you start Calibrate, Register or Stack, and when you accept a sorting. The title is the last two parts of the lights path. |
| Open session | Fills the Calibrate, Register and Stack boxes from the session and opens Calibrate (or Stack when the session has no calibration frames). Folders that no longer exist are left out and named in the message. Greyed out when the lights folder itself is gone. |
| Not found: darks, flats... | In the error colour: the folders of the session that are no longer there. |
| Forget (on a session) | Removes that session from the list. Nothing on disk changes. |
| Recent images | The images opened lately, as in the **Recent** menu. A file that is gone is marked **Not found** and cannot be opened. |
| Forget (on an image) | Removes that image from the list. |
| Clear recent sessions and images | Empties both lists. |

## Sorting dropped frames

Drop a folder (or several files) on the window, or press Ctrl+Shift+O and choose a folder. Valerian reads
the **headers** of every FITS file (`.fit`, `.fits`, `.fts`) under it, down to 6 folders deep and up to
50 000 files, skipping hidden folders, links to folders and its own `valerian-sorted` folder, and shows a
review. One dropped FITS file is opened as an image instead. Dropping while a step runs is refused with a
message; Esc cancels a scan in progress.

How each frame is classified, in this order:

1. The header card `IMAGETYP` (else `FRAMETYP`, then `FRAME`). Case, spaces, underscores and a trailing
   "Frame" or "Field" do not matter.
2. The file name (`Light_M31_001.fit`).
3. The name of the folder it is in (`.../DARKFLAT/...`).
4. Otherwise it is **Unsorted** and left out until you assign it.

| The text names | Becomes | Examples |
|---|---|---|
| light, lights, object, science, sci, target, or starts with "light" | Light | `Light Frame`, `LIGHT`, `Object` |
| dark flat, darkflat, flat dark (any separator) | Dark flat | `DARKFLAT`, `Dark Flat`, `dark_flat` |
| flat | Flat | `Flat Field`, `FLAT`, `Flat Frame` |
| dark | Dark | `Dark Frame`, `DARK` |
| bias, zero, offset | Bias | `Bias Frame`, `Zero` |
| the word master or stack as well | the same kind, marked as a master | `Master Dark`, `MasterFlat` |

The group is then settled from the whole set, and every automatic change is explained in the frame's note:

| Rule | Why |
|---|---|
| A dark whose exposure matches no light but matches the flats' becomes a dark flat. Exposures are the same within 1 percent or 10 ms. | Many cameras write `Dark` for dark flats. |
| Darks whose exposure matches none of the lights are left out. | A dark of another exposure scales wrongly. |
| When lights use several filters, the filter with most frames is kept and the others are left out. | A run calibrates and stacks one filter. |
| Flats and dark flats of a filter that no kept light uses are left out (names compared without case). | The wrong filter's flat would add its dust and vignetting. |
| A kind with single frames leaves its masters out; with only masters, the first is used and the rest left out. | Frames and masters must not be mixed. |
| Frames with no `FILTER` or `EXPTIME` are never left out for that reason. | Missing keys must not drop data. |

### The review

| Control | Does |
|---|---|
| Counts line | Per group, frames used and found, for example `Lights 25/25   Darks 10/10`. |
| Notes (lines starting with a dash) | What is missing or odd: no darks, no flats, several exposures, frames that could not be sorted, files that could not be read. "No frames are set as lights" blocks Accept. |
| Group header (Lights, Darks, Dark flats, Flats, Bias, Unsorted) | How many are used, and the filter and exposure of the group (`Ha, 300 s x 25`). Groups of 10 or fewer start open. |
| Use all, Leave all out | Includes or excludes every frame of the group. |
| Move all to | Assigns every frame of the group to another group. |
| Tick box on a row | Includes or excludes that frame. |
| Group list on a row | Assigns that frame to another group, or **Leave unsorted**. Assigned frames are included. |
| Last column | Why the frame is where it is: from the header, from the file name, from the folder name, set by you, or the reason it was left out. |
| Accept sorting (Enter) | Fills the Calibrate boxes (and the Register and Stack boxes after them), opens Calibrate, and remembers the folders as a session. |
| Cancel sorting (Esc) | Throws the result away; nothing changes. |

On accept, a group that is **all of one folder** is used as that folder, and a group that is **one file**
(a master) as that file. A group that is only part of a folder, or comes from several folders, is
**gathered** into `valerian-sorted/<kind>/` beside the dropped folder as links (hard links, or copies when
the disk differs); the files are not moved or changed, and names that clash get their folder name in front.
Gathering again replaces the links of the earlier gathering. With calibration frames the Calibrate output is
`calibrated` beside the lights, Register reads it and writes `registered`, and Stack reads that and writes
`stack.fits`; with lights only, Register and Stack read the lights.


## Rail

One button per step: Calibrate, Register, Stack, Process, Export. Status: Optional, Ready, Running,
Waiting or Done (with a tick). Below 900 px wide only the numbers show.

## View

Zoom is in screen pixels per pixel of the open image: 100 % is one for one (1:1), 400 % is four screen
pixels to an image pixel. The range is 2 % to 3200 %. Fitted, the picture is never enlarged above 100 %.

| Control | Does |
|---|---|
| Scroll, +, - (also the keys `+`, `=` and `-`) | Zoom about the pointer (keys: about the centre), 2 % to 3200 % of the open image, steps of 25 %. |
| Drag | Pan. |
| Percentage button, double click, key `0` | Fit (never above 100 %). Key `1` shows 100 % (1:1, one screen pixel to an image pixel). |
| Histogram | Show or hide it. Default shown; 128 bins, log scale, of what is drawn. |
| Dismiss | Hide a finished, cancelled or error message. |
| Cancel (Esc) | In the notice while Calibrate, Register, Stack, Drizzle, Star removal, Plate solve or Colour runs: stops the step. See [Cancelling a step](#cancelling-a-step). |

## Cancelling a step

| Control | Does |
|---|---|
| Cancel, in the notice | Asks the running step to stop. The button changes to "Cancelling..." and the headline to "Cancelling: finishing the frame in hand, nothing will be kept...". A second press does nothing. |
| Esc | The same as Cancel while a step runs and the About page is not open (Esc closes About first). |
| Where it applies | Calibrate, Register, Stack, Drizzle, Star removal, Plate solve and Colour, and the reading of a dropped folder and the gathering of its frames. Writing a file (Save, Export, Apply and save) is short and has no Cancel. |
| How soon | At the next check: between files, between bands of a master frame, between frames while registering, warping or drizzling, between colour planes of a stack or of star removal, and between trial patches of sky in a plate solve. The frame or band in hand finishes first, so on large frames it can take a few seconds. |
| What is kept | Nothing. Calibrate, Register and Stack write into a hidden `.valerian-partial-<pid>` folder inside the output folder and move the files into place only when the whole run has finished, so a cancelled (or failed) run removes that folder, removes any output folder it had to create, and leaves files that were already there untouched. |
| What you see | A notice "Cancelled: calibration stopped. Nothing from the run was kept." (registration, stacking, drizzling, star removal, plate solving or colour calibration in the same words) with Dismiss. It is not shown as an error. The step's panel returns to its idle button and the rail row leaves "Running"; the result of an earlier run of the step is kept. |
| If the step finishes first | Cancel arrives after the last check and the step completes normally; its result is kept. |

### Full-resolution tiles

The picture is drawn from a reduced copy (1600 pixels on the long side) and, where the zoom goes
beyond what that holds, from the real pixels. The open image gets a pyramid of levels (each half the
size of the one before, averaging the finite pixels, down to one 512 pixel tile) built on a worker
thread, announced above the picture while it runs (images of 1600 pixels or less are prepared at once,
with no message). The level is chosen from the zoom (`floor(log2(1/zoom))`, level 0 from 100 %), and the
tiles of it that the window shows (512 by 512, smaller at the right and bottom edges) are stretched with
the current stretch, tinted when night vision is on and drawn over the reduced copy. A least-recently-used
cache keeps at most 256 MiB of tile textures; a change of stretch or palette discards them. A frame spends
about 12 ms on tiles and asks for another frame when more are needed. The extra levels hold about a third
of the image's pixels again. Tiles are drawn for the stretched open image (the Stretch, Plate solve and
Export tools, and Colour before it is calibrated); the star-removal layers, the colour-calibrated view and
the pictures of the folder steps are drawn from reduced copies. See [the zoom tutorial](../tutorials/zoom.md).

### Pixel readout

With the pointer over the picture a box at the bottom left shows the pixel under it:

| Line | Meaning |
|---|---|
| `x`, `y` | Image pixel, from 0 at the top left. |
| `raw` | The file's value of each plane (`R`, `G`, `B` for colour), in the file's units. |
| `shown` | The value after the current stretch, 0 to 1 per plane; only where tiles apply (see above). |
| `RA`, `Dec` | Sky position of the pixel centre, when the image has a TAN solution (a solved file, or the result of Plate solve in this session): `21h 00m 03.0s`, `+44d 31' 02"`. |

From 400 % the pixel is outlined. The readout does not change the picture and cannot be switched off;
it is drawn only while the pointer is over the picture.

## Calibrate

| Control | Default | Does |
|---|---|---|
| Lights | empty | Folder or file of lights. Required. |
| Darks, Flats, Dark flats, Bias | empty | Folder, or one master file. At least one of darks, flats and bias is needed. |
| Calibrated frames go to | beside the lights, `calibrated` | Output folder; masters go in `masters/`. |
| Calibrate lights | | Runs. Fixed: automatic rejection, hot 40 and cold 12 sigma, dead below 0.2 of the flat, global flat normalisation, 1 GiB memory. |

## Register

| Control | Default | Does |
|---|---|---|
| Frames | Calibrate output | Folder of frames. |
| Aligned frames go to | `registered` beside it | Output folder. |
| Register frames | | Best frame as reference, default detection, no level matching; raw colour is demosaiced. |

## Stack

| Control | Default | Does |
|---|---|---|
| Mean, Drizzle, Science | Mean | Chooses the stack, the drizzle or the [Science panel](#science). |
| Frames | last step's output | Folder of frames. |
| Stack goes to | `stack.fits` beside it | The output file; the stack is opened afterwards. |
| Rejection | Automatic | Automatic (by frame count), Winsorised, Sigma (3 sigma both sides), None. |
| Frames left out by the Science panel | | Shown when [Use for stacking](#science) has set frames aside, with **Use all** to clear it. The stack's "N of M frames" counts M before they were removed. |
| Stack frames | | Runs on the CPU engine. |

### Drizzle

| Control | Range | Default |
|---|---|---|
| Frames, Choose folder... | folder | empty |
| Scale | 1 to 4, step 0.5 | 2 |
| Pixfrac | 0.2 to 1.0 | 0.6 |
| Reject outliers | on or off | on |
| Rejection (sigma) | 2 to 8 | 4 |
| Drizzle frames, Save drizzled, Open it, Save as... | | |

### Science

Measures a folder of frames and shows the per-frame plots in the viewer. How each number is
measured, and the files the report holds, are in [How subframe
analysis works](../user-guide/subframes.md); the walk-through is [tutorial
12](../tutorials/subframe-analysis.md).

| Control | Range | Default |
|---|---|---|
| Frames, Choose folder... | folder of FITS frames | the folder of the last Register or Calibrate, else the Stack folder |
| Grade with the Seestar model | tick box, off by default (optional): grade raw Seestar S50 subs while measuring and reject the ones that score below the next value; other frames are skipped. See [How the Seestar sub grader works](../user-guide/grading.md) | off |
| Reject below | the lowest score a graded frame may have, 0 to 1; shown when the box above is ticked; changing it judges the frames again at once | 0.50 |
| Model file | an `.onnx` grader model to use instead of the built-in one; empty for the built-in one | empty |
| Measure frames | registers the frames to the sharpest and measures each; runs on a worker thread with progress and Cancel | |
| FWHM above (px) | tick box and value, 0 and up, step 0.05 | off, 4.00 |
| Eccentricity above | tick box and value, 0 to 1, step 0.01 | off, 0.500 |
| Stars below | tick box and count | off, 50 |
| Noise above (ADU) | tick box and value | off, 100.0 |
| SNR weight below | tick box and value, step 0.01 | off, 0.50 |
| Residual above (px) | tick box and value, step 0.01 | off, 1.000 |
| Outliers beyond (sigma) | tick box and value, step 0.1 | on, 3.0 |
| By frame, By time (viewer) | the horizontal axis; By time needs `DATE-OBS` in every frame | By frame |
| Frames list | a tick box per frame (ticked is approved); click to override a verdict, hover for the reasons | the verdicts of the limits |
| Use for stacking | the next Mean stack leaves out the rejected frames | |
| Report, Save report | folder for `subframes.csv`, `subframes.json`, the PNG plots and `report.html` | `science-report` beside the frames folder |
| Light curve: The brightest unsaturated star, x, y, Aperture (FWHM), Measure the star | measures one star in every frame against comparison stars, on a worker thread: tick the box for the brightest unsaturated star or untick it and give its position in the reference frame, the aperture is 1 to 5 FWHM (2.0); the Light curve chip shows the curve and the check star, and the panel says their scatter. See [How the astrometry and photometry outputs work](../user-guide/astrometry.md) | the brightest star, 2.0 |
| Stack for the maps | registers and stacks the folder on the CPU (progress and Cancel) and draws the rejection, count and weight maps, the noise against the square root of the number of frames and the histograms of the result; the stack is not saved. The frames left out by Use for stacking are left out here too | |
| View chips (Subframes, Rejected low, Rejected high, Pixel count, Weight, Noise, Histogram, Light curve) | which picture the viewer shows; the integration chips appear after Stack for the maps. See [How the integration diagnostics work](../user-guide/integration.md) | Subframes, then Rejected high after a run |

Changing a limit judges the frames again at once and discards hand changes to the list.
Measuring again replaces the table.

## Process

Image box, Browse..., Open: the working image. Chips: Stretch, Star removal, Plate solve, Colour.

### Stretch

| Control | Range | Default |
|---|---|---|
| Curve | Auto, MTF, Arcsinh, GHS | Auto |
| Sky target (Auto) | 0.02 to 0.5 | 0.10 |
| Midtone (MTF) | 0.001 to 0.999 | 0.25 |
| Strength (Arcsinh) | 0.5 to 5000, logarithmic | 50 |
| Stretch factor (GHS) | 0 to 6 | 3.0 |
| Local intensity | -5 to 15 | 0 |
| Symmetry point | 0 to 1 | 0 |
| Shadow protection | 0 to the symmetry point | 0 |
| Highlight protection | symmetry point to 1 | 1 |
| Set black and white by hand | on or off | off |
| black, white | any (drag values) | measured |
| Black below sky (sigma) | 0 to 6 | 2.8 |
| White percentile | 99 to 100 | 99.995 |
| Same stretch for every colour plane | on or off | on |
| Reset | | Auto curve |
| Save as..., Apply and save | | `<name>_stretched.fits` |

### Star removal

| Control | Range | Default |
|---|---|---|
| Threshold (sigma) | 2.5 to 12 | 4.0 |
| Mask size | 0.5 to 3 | 1.0 |
| Grow (px) | 0 to 10 | 1.5 |
| Largest radius (px) | 5 to 150 | 60 |
| Add noise to the filled pixels | on or off | on |
| Remove stars | | Runs on the full image. |
| Starless, Original, Stars, Mask | | Which layer is drawn. |
| Strength | 0 to 1 | 1 (applied after the run) |
| Save starless and stars | | `<name>_starless.fits` and `<name>_stars.fits` |

### Plate solve

| Control | Range | Default |
|---|---|---|
| RA, Dec | degrees or dd:mm:ss | from the header |
| Search radius (deg) | 0.1 to 5 | 1.0 |
| Blind: search the whole sky | on or off | off |
| arcsec per pixel | 0.1 to 60 | from the header; searched 15 % either way |
| Catalogue, Choose catalogue... | `.vcat` file | `gaia-dr3-g11.vcat` beside the program |
| Solve, Save with WCS | | `<name>_solved.fits` |

### Optics

What the stars of the open image say about the telescope. How each plot is made is in [How the
optics plots work](../user-guide/optics.md); the walk-through is [tutorial
13](../tutorials/optics.md).

| Control | Range | Default |
|---|---|---|
| Crop (px) | 32 to 512, the side of an inspector crop | 128 |
| Zoom | 1 to 6, enlargement of an inspector crop | 2 |
| Map degree | 1 to 4, the degree of the surface fitted to the stars | 3 |
| Profile stars | 8 to 200, stars stacked in the profile | 50 |
| Smallest star S/N | 3 to 200, peak over noise of a measured star | 10 |
| Measure the stars | measures the open image and draws every plot on a worker thread; needs at least 30 usable stars | |
| Inspector, FWHM map, Eccentricity map, Corners, Star profile | the plot the viewer shows; a plot that could not be drawn has no chip | Inspector |
| Zones, Hint | the median FWHM and eccentricity of the centre and corners, the Moffat and Gaussian FWHM, and the tilt and backfocus hint in words | |
| Report, Save report | folder for the plots, their CSV and JSON and `report.html` | `optics-report` beside the image |

### Astrometry

The plate solution drawn on the open image, its residuals and their map, the colour fit, the zero point of each colour and the limiting magnitude. How each is made is in
[How the astrometry and photometry outputs work](../user-guide/astrometry.md); the walk-through is
[tutorial 15](../tutorials/astrometry.md) and, for the magnitudes, [tutorial 20](../tutorials/zero-point-and-limit.md).

| Control | Range | Default |
|---|---|---|
| RA/Dec grid, Catalogue objects, Gaia stars | tick boxes | all on |
| Faintest star (G) | 4 to 11 | 10 |
| Draw the solution | draws the layers, measures the residuals and (for a linear colour image) fits the colour, on a worker thread; needs a plate solution, from the header or from the Plate solve tool | |
| Annotated, Residuals, Residual map, Colour fit, Zero point, Limiting mag | the picture the viewer shows; a picture that could not be made has no chip (the last three need a linear colour image) | Annotated |
| Result | objects and stars drawn, the residual rms in pixels and arc seconds, the residual-map scale (ppm) and rotation (arc seconds), the colour fit slopes, the green zero point with its error and the 5-sigma limiting magnitude, and the largest objects | |
| Report, Save report | folder for the picture, lists, residuals, residual map, colour fit, zero points, `calibrated_stars.csv` and `report.html` | `astrometry-report` beside the image |

### Colour

| Control | Range | Default |
|---|---|---|
| Neutral at BP-RP | 0.3 to 1.5 | 0.82 |
| Brightest (G) | 4 to 10 | 6 |
| Faintest (G) | 8 to 11 | 11 |
| Aperture (FWHM) | 1 to 4 | 2 |
| Make the background neutral | on or off | on |
| Calibrate colour | | Needs three planes and a plate solution. |
| Calibrated, Original | | Which picture is drawn. |
| Save calibrated | | `<name>_calibrated.fits` |

## Export

| Control | Default | Does |
|---|---|---|
| Format | FITS | FITS: 32-bit float, unstretched, with header. PNG: 8-bit with the current stretch. |
| Save as... | `<name>_export.fits` | The extension follows the format. |
| Export picture | | Writes it. |

## GPU

| Control | Range | Default |
|---|---|---|
| Adapter | Hardware, Software | Hardware |
| Width, Height (px) | 256 to 6144, 256 to 4608 | 2048, 1536 |
| Frames in the combine | 3 to 32 | 12 |
| Run benchmark | | Times each kernel against the CPU. |

## Keyboard shortcuts

F1 or ? shows this list over the window (Esc or the same key closes it). Function keys and chords work
everywhere; plain keys (`N`, `?`, `+`, `-`, `0`, `1`, Enter) are left to a text box that has the keyboard.
Every action says what it did in the notice above the picture; while a step runs, the words appear beside its
progress and the step's own message stays.

| Keys | Does |
|---|---|
| `Ctrl+O` | Open a FITS image: "Choose a FITS image to open in the dialog." |
| `Ctrl+Shift+O` | Choose a folder of frames to sort, then review it. |
| `Ctrl+Enter` | Run the open step or tool exactly as its button does (Calibrate lights, Register frames, Stack frames, Drizzle frames, Apply and save, Remove stars, Solve, Calibrate colour, Export picture, Run benchmark). While a sorting is being reviewed it accepts it. Says why when it cannot: no image open, no output named, something already running. |
| `F5` | The same as `Ctrl+Enter`. |
| `Esc` | Closes the help or the About page; otherwise cancels the running step, as the Cancel button does (see [Cancelling a step](#cancelling-a-step)); otherwise drops a sorting review. Does nothing, and says nothing, when there is nothing to do. |
| `Enter` | Accepts the sorting while it is being reviewed. |
| `Alt+Right` | Next step: "Step 3 of 5: Stack". Stops at Export. |
| `Alt+Left` | Previous step. Stops at Calibrate. |
| `+` | Zoom in by 25 percent: "Zoom 125 percent". |
| `-` | Zoom out. |
| `0` | Fit the picture to the window. |
| `1` | Show the picture at 100 percent of the preview. |
| `F2` | Night vision on or off, remembered. |
| `N` | Night vision on or off. |
| `F1` | This list. |
| `?` | This list. |
| `Ctrl+F1` | The About page; the list's **About Valerian** button opens it too. |

Ctrl is Command on a Mac. Keys that need Shift on your keyboard (`+`, `?`) work as typed; `=` also zooms in.
The picture keys act on the open image; with none open they say "Open an image to zoom it."


## Settings file

`$VALERIAN_GUI_CONFIG`, else `%APPDATA%\valerian\gui.conf` on Windows, else
`$XDG_CONFIG_HOME/valerian/gui.conf`, else `~/.config/valerian/gui.conf`. Plain `key=value`:
`window=WxH`, `night=1`, up to eight `recent=` paths (images) and up to eight `session=` lines (folders:
lights, darks, flats, dark flats and bias, separated by tabs, empty when not used). A damaged or relative
line is skipped. **Clear recent sessions and images** on the start screen empties both lists.
