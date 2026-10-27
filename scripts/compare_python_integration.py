#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Check the integration diagnostics of `valerian stack --science-report` against numpy and astropy.

    valerian register LIGHTS -o reg
    valerian stack LIGHTS -o x.fits --science-report rep --normalisation none \\
        --weights equal --rejection sigma
    python compare_python_integration.py reg rep [--rows 400 700]

`valerian register` writes the frames on the reference grid with no level matching, which
is what the stack above combines. For a band of rows it repeats, with different code:

1. the rejection maps: astropy.stats.sigma_clip over the frames with the same limits
   (3 sigma both sides, 10 iterations, the median as centre and the standard deviation
   about it), counted below and above the median per pixel and summed over the colour
   planes, against `rejection_low.fits` and `rejection_high.fits`;
2. the pixel-count map: the frames with data minus those clipped, against `pixel_count.fits`;
3. the noise curve: for each point of `noise.csv`, the first 2m frames split alternately into
   two means, the clipped standard deviation of their difference over the square root of
   two (astropy.stats.sigma_clipped_stats), over the pixels every frame has.

Needs numpy and astropy (see python-parity-requirements.txt). Image data is never committed.
"""
import argparse
import csv
import glob

import numpy as np
from astropy.io import fits
from astropy.stats import sigma_clip, sigma_clipped_stats


def rms_about_median(d, axis=None, **_):
    m = np.nanmedian(d, axis=axis, keepdims=True)
    return np.sqrt(np.nanmean((d - m) ** 2, axis=axis))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("registered")
    ap.add_argument("report")
    ap.add_argument("--rows", type=int, nargs=2, default=[400, 700])
    a = ap.parse_args()
    files = sorted(glob.glob(a.registered.rstrip("/") + "/*.fits"))
    r0, r1 = a.rows
    cube = []
    for f in files:
        with fits.open(f, memmap=True) as h:
            d = h[0].data
            d = d[None] if d.ndim == 2 else d
            cube.append(np.array(d[:, r0:r1, :], dtype=np.float32))
    cube = np.stack(cube)  # frames, planes, rows, cols
    n = len(files)
    print(f"{n} registered frames, rows {r0}:{r1}, {cube.shape[1]} planes")
    low = np.zeros(cube.shape[2:], np.int32)
    high = np.zeros(cube.shape[2:], np.int32)
    used = np.zeros(cube.shape[2:], np.int32)
    for p in range(cube.shape[1]):
        clipped = sigma_clip(cube[:, p], sigma_lower=3, sigma_upper=3, maxiters=10,
                             cenfunc="median", stdfunc=rms_about_median, axis=0, masked=True)
        centre = np.nanmedian(np.where(clipped.mask, np.nan, cube[:, p]), axis=0)
        below = clipped.mask & np.isfinite(cube[:, p]) & (cube[:, p] < centre)
        above = clipped.mask & np.isfinite(cube[:, p]) & ~(cube[:, p] < centre)
        low += below.sum(axis=0)
        high += above.sum(axis=0)
        if p == 0:
            used = (np.isfinite(cube[:, 0]) & ~clipped.mask).sum(axis=0)
    rep = a.report.rstrip("/")
    for name, mine in (("rejection_low", low), ("rejection_high", high), ("pixel_count", used)):
        v = fits.getdata(f"{rep}/{name}.fits")[r0:r1, :]
        same = (v == mine).mean()
        print(f"{name}: identical in {100 * same:.2f} % of the pixels; "
              f"mean valerian {v.mean():.3f}, astropy {mine.mean():.3f}; max difference {np.abs(v - mine).max():.0f}")

    ok = np.isfinite(cube[:, cube.shape[1] // 2]).all(axis=0)
    plane = cube[:, cube.shape[1] // 2]
    print("frames per half | valerian noise | numpy/astropy noise | ratio")
    for row in csv.DictReader(open(f"{rep}/noise.csv", encoding="utf-8")):
        m = int(row["frames_per_half"])
        sel = plane[:2 * m]
        A = sel[0::2].mean(axis=0)
        B = sel[1::2].mean(axis=0)
        _, _, s = sigma_clipped_stats((A - B)[ok], sigma=3.0, maxiters=6)
        mine = s / np.sqrt(2) / 0.9866
        print(f"{m} | {float(row['noise_adu']):.2f} | {mine:.2f} | {float(row['noise_adu']) / mine:.3f}")


if __name__ == "__main__":
    main()
