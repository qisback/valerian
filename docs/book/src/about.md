# About Valerian

Valerian is open-source, GPU-accelerated deep-sky image processing: it takes the
subframes you capture of galaxies, nebulae and clusters and turns them into a finished
image, on Linux, Windows and macOS, on x86_64 and ARM.

## Dedication

Valerian is dedicated to my son Xaviar. His middle name is Valerian, because he was a
very sleepy child, which suits software made for the long, quiet hours of the night sky.

The dedication is part of the licence terms: it must be kept in every copy and every
modified version (see below). The desktop app's About page leads with it.

## Its sister

Valerian is the deep-sky sibling of [Helia](https://github.com/qisback/helia),
the solar stacker. The two share their core: file formats, GPU acceleration, the
interface and the release pipeline, so an improvement to one lands in both. Helia's
manual is in Helia's repository.

## Status

Valerian is pre-1.0. On the command line it calibrates, registers and
stacks frames, removes the sky gradient, stretches, removes stars, plate solves,
calibrates the colour against Gaia and drizzles, with GPU kernels for the heavy stages. The
desktop app has the five steps (calibrate, register, stack, process, export) and this page;
background extraction is not in the window yet. The scope and the plan are in the
[roadmap](roadmap.md); this manual grows
with the program, and each page says plainly what does not work yet. See the
[roadmap](roadmap.md) and [a first session](user-guide/walkthrough.md).

## Copyright and licence

Copyright (C) 2026 Laurence Horrocks-Barlow.

Valerian is free software: you can redistribute it and/or modify it under the terms of
the GNU General Public License as published by the Free Software Foundation, either
version 3 of the License, or (at your option) any later version. It is distributed in
the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied
warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.

Additional terms apply under section 7 of the licence: the author attribution and the
dedication above must be kept in every copy and every modified version, and modified
versions must not be presented as the original. They are in `ADDITIONAL-TERMS.md` in the
repository.

## Data that is not ours

* The star catalogue is made from **Gaia DR3**, (c) ESA/Gaia/DPAC, CC BY-NC 3.0 IGO (non-commercial).
* The list of Messier, NGC and IC objects drawn by `valerian annotate` is a subset of **OpenNGC** by
  Mattia Verga (<https://github.com/mattiaverga/OpenNGC>), licensed **CC BY-SA 4.0**. The subset
  is made by `scripts/build_objects.py`; the data file keeps its own licence (`LICENSES/CC-BY-SA-4.0.txt`).
* The fonts are IBM Plex, under the SIL Open Font Licence 1.1.

## Credits

Acknowledgements (Gaia, fonts, libraries), the licence of the star catalogue and the
trademark notice are on [Credits and trademarks](credits.md); the sources of every method are
on [References](reference/references.md).

## More from the author

Valerian is made by Laurence Horrocks-Barlow. If you are interested in anything else I
make, visit [12hobbies.studio](https://12hobbies.studio).
