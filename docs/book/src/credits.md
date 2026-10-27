# Credits and trademarks

## Not affiliated

Valerian is independent software. It is not affiliated with, endorsed by or sponsored by any of the
companies, projects or products named in this manual. Their names are used only to say what a
camera or a file is, or what a tool is compared with, and they are trademarks or registered
trademarks of their owners. They include Seestar, ZWO, ASI, Starwatcher, N.I.N.A., PixInsight, Siril, DeepSkyStacker, ASTAP, Python, NumPy, SciPy,
Astropy, Windows, macOS, Linux, Vulkan, Metal and NVIDIA. Where Valerian does something that
another program is known for (for example a midtone transfer function, a generalised hyperbolic
stretch or a background model), the manual describes the method and its published source on the
[References](reference/references.md) page; the behaviour was matched from published descriptions
and no code was taken from those programs.

The Valerian name and logo are covered by the
[logo licence](https://github.com/qisback/valerian/blob/main/LOGO-LICENCE.md)
in the repository, which is separate from the GPL.

## Gaia

The star catalogue `gaia-dr3-g11.vcat` holds data from the European Space Agency (ESA) mission
Gaia. ESA's acknowledgement for Gaia DR3 reads:

> This work has made use of data from the European Space Agency (ESA) mission Gaia
> (https://www.cosmos.esa.int/gaia), processed by the Gaia Data Processing and Analysis Consortium
> (DPAC, https://www.cosmos.esa.int/web/gaia/dpac/consortium). Funding for the DPAC has been
> provided by national institutions, in particular the institutions participating in the Gaia
> Multilateral Agreement.

Please cite Gaia Collaboration, Prusti et al. (2016), "The Gaia mission", Astronomy and
Astrophysics 595, A1, and Gaia Collaboration, Vallenari et al. (2023), "Gaia Data Release 3:
summary of the content and survey properties", Astronomy and Astrophysics 674, A1.

**Licence of the data.** ESA's Gaia licence page says: "Gaia data are distributed under the CC BY-NC
3.0 IGO license" (https://www.cosmos.esa.int/web/gaia-users/license, checked 2026-10-21). That is
Creative Commons Attribution-NonCommercial 3.0 IGO. ESA's terms for its science archives also say
that, before any commercial use, "including any use or application that directly or indirectly
generates a financial gain", a request for authorisation must be sent to data.licences@esa.int
(https://www.cosmos.esa.int/web/esdc/terms-and-conditions). The catalogue file is therefore not
covered by the GPL that covers Valerian's code: use and share it for non-commercial purposes, with
this credit, and ask ESA before any commercial use. Earlier releases and pages of this manual
called it CC BY-SA 3.0 IGO; that label has been corrected. This is not legal advice.

The file is kept apart from the program. It is read at run time and is not compiled into the
binary, so the program's own licence is unaffected by it.

## Fonts

The desktop app embeds IBM Plex Sans and IBM Plex Mono, copyright 2017 IBM Corp. with Reserved
Font Name "Plex", under the SIL Open Font License 1.1 (the text ships with each release as part
of `THIRD-PARTY-LICENSES.txt`).

## Libraries

Valerian is built with open-source Rust crates, each under its own licence (almost all MIT or
Apache-2.0, with some BSD, ISC, Zlib, Unicode and Boost licences). The full list and the licence
texts ship with every release in `THIRD-PARTY-LICENSES.txt`, and a software bill of materials
lists them. Valerian's image-format and GPU core comes from Helia, by the same author.

## Programs used only to check Valerian

The scripts in `scripts/` compare Valerian's results with independent Python libraries. They run
on the developer's machine and in the nightly job, are not part of any release and are not linked
into Valerian. Licences, as published on PyPI: numpy (BSD-3-Clause and others), SciPy (BSD),
Astropy (BSD-3-Clause), photutils (BSD-3-Clause), scikit-image (BSD), `sep` (LGPLv3+),
astroalign (MIT), and their support packages (BSD, MIT, Apache-2.0 or HPND). No code from them is
copied into Valerian.
