#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Bin raw frames to make them undersampled, for testing drizzle.

    python make_binned_frames.py RAW_DIR OUT_DIR [--bin 2]

Each FITS file of RAW_DIR is averaged in blocks of --bin by --bin pixels and
written to OUT_DIR as a 32-bit float mono frame. A raw one-shot-colour mosaic
is binned the same way, which is a super-pixel mix of its colours, i.e. a
luminance frame. The exposure keywords are kept and the Bayer pattern is dropped.
A star 2.3 pixels wide becomes 1.15 pixels wide: sampled less than twice per
FWHM, which is what drizzle is for. Image data is never committed.
"""
import argparse
import pathlib

import numpy as np
from astropy.io import fits


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("raw_dir")
    ap.add_argument("out_dir")
    ap.add_argument("--bin", type=int, default=2)
    a = ap.parse_args()
    out = pathlib.Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    k = a.bin
    files = sorted(p for p in pathlib.Path(a.raw_dir).iterdir() if p.suffix.lower() in (".fit", ".fits", ".fts"))
    for p in files:
        with fits.open(p) as h:
            data = h[0].data.astype(np.float32)
            hdr = h[0].header
        ny, nx = (data.shape[0] // k) * k, (data.shape[1] // k) * k
        b = data[:ny, :nx].reshape(ny // k, k, nx // k, k).mean(axis=(1, 3)).astype(np.float32)
        new = fits.Header()
        for key in ("EXPTIME", "EXPOSURE", "DATE-OBS", "OBJECT", "INSTRUME", "FILTER", "GAIN"):
            if key in hdr:
                new[key] = hdr[key]
        new["HISTORY"] = f"binned {k}x{k} by make_binned_frames.py"
        fits.PrimaryHDU(b, header=new).writeto(out / (p.stem + "_bin.fits"), overwrite=True)
    print(f"wrote {len(files)} frames of {nx // k} x {ny // k} to {out}")


if __name__ == "__main__":
    main()
