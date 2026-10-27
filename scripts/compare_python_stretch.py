#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Compare `valerian stretch` with the owner's Python autostretch.

The Python side restates `autostretch` and `mtf` from the pipeline's colour.py
(black point 2.8 robust sigmas below the median, white point at the 99.995th
percentile, a midtone solved to put the sky at the target). Nothing is imported
from that repository. For a colour image the levels and midtone are found once,
from the mean of the planes, and applied to every plane, as `valerian stretch`
does when the planes are linked.

    python compare_python_stretch.py LINEAR.fits --valerian STRETCHED.fits [--target 0.10]

where STRETCHED.fits came from `valerian stretch LINEAR.fits -o STRETCHED.fits
--target 0.10`. Needs numpy and astropy. Image data is never committed.
"""
import argparse
import json

import numpy as np
from astropy.io import fits


def mtf(x, m):
    x = np.clip(x, 0.0, 1.0)
    return ((m - 1.0) * x) / ((2.0 * m - 1.0) * x - m)


def autostretch(img, target, shadow_sigma=2.8):
    finite = img[np.isfinite(img)]
    sky = float(np.median(finite))
    mad = 1.4826 * float(np.median(np.abs(finite - sky)))
    if mad <= 0:
        mad = max(float(np.std(finite)), 1e-6)
    black = sky - shadow_sigma * mad
    white = float(np.percentile(finite, 99.995))
    if white <= black:
        white = black + 1.0
    sky_n = (sky - black) / (white - black)
    m = ((target - 1.0) * sky_n) / (2.0 * target * sky_n - target - sky_n)
    m = float(np.clip(m, 1e-4, 0.9))
    return black, white, m


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("linear")
    ap.add_argument("--valerian", required=True)
    ap.add_argument("--target", type=float, default=0.10)
    ap.add_argument("--json", help="write the measured numbers to this JSON file (for python_parity.py)")
    a = ap.parse_args()
    lin = fits.getdata(a.linear).astype(np.float64)
    val = fits.getdata(a.valerian).astype(np.float64)
    planes = lin[None] if lin.ndim == 2 else lin
    vplanes = val[None] if val.ndim == 2 else val
    luma = np.nanmean(planes, axis=0)
    black, white, m = autostretch(luma, a.target)
    print(f"python: black {black:.4f} white {white:.4f} midtone {m:.6f}")
    worst = 0.0
    for i, (p, v) in enumerate(zip(planes, vplanes)):
        py = mtf(np.clip((p - black) / (white - black), 0, 1), m)
        d = np.abs(py - v)
        worst = max(worst, float(np.nanmax(d)))
        print(f"plane {i}: max |diff| {np.nanmax(d):.2e}, mean {np.nanmean(d):.2e}, "
              f"median stretched python {np.nanmedian(py):.4f} valerian {np.nanmedian(v):.4f}")
    print("OK" if worst < 1e-3 else "DIFFERS")
    if a.json:
        with open(a.json, "w") as f:
            json.dump({"black": black, "white": white, "midtone": m, "max_abs_diff": worst}, f, indent=2)


if __name__ == "__main__":
    main()
