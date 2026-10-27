#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Check `valerian optics` against astropy and numpy.

    python compare_python_optics.py IMAGE.fits REPORT_DIR [--order 3]

where REPORT_DIR came from `valerian optics IMAGE.fits --report REPORT_DIR`. With
different code it repeats:

1. the FWHM of the stars Valerian measured (`field_stars.csv`), by fitting a two
   dimensional Gaussian plus a constant to a 21 by 21 pixel cut-out of each with
   astropy.modeling, and the median per zone (the four corners, the outer quarter of
   each side, and the centre, as in the tilt hint of `optics.json`);
2. the Moffat profile: a two-dimensional Moffat plus a constant fitted to each of
   the brightest isolated stars with astropy.modeling.Moffat2D, FWHM and beta
   (median over stars), against the stacked-profile fit in `optics.json`;
3. the polynomial surface of the FWHM map: numpy least squares of a polynomial of
   the same degree to Valerian's star table, with the same 3-sigma clipping,
   evaluated on the grid of `fwhm_map.csv`.

Needs numpy, astropy (see python-parity-requirements.txt). Image data is never
committed.
"""
import argparse
import csv
import json
import warnings

import numpy as np
from astropy.io import fits
from astropy.modeling import fitting, models

warnings.filterwarnings("ignore")


def cutout(data, x, y, half=10):
    x0, y0 = int(round(x)), int(round(y))
    if x0 < half + 1 or y0 < half + 1 or x0 > data.shape[1] - half - 2 or y0 > data.shape[0] - half - 2:
        return None
    return data[y0 - half:y0 + half + 1, x0 - half:x0 + half + 1], (x - (x0 - half), y - (y0 - half))


def gauss_fwhm(data, x, y, fit):
    c = cutout(data, x, y)
    if c is None:
        return None
    cut, (cx, cy) = c
    yy, xx = np.mgrid[0:cut.shape[0], 0:cut.shape[1]]
    model = models.Gaussian2D(cut.max() - np.median(cut), cx, cy, 2.0, 2.0) + models.Const2D(np.median(cut))
    try:
        m = fit(model, xx, yy, cut, maxiter=300)
    except Exception:
        return None
    sx, sy = abs(m.x_stddev_0.value), abs(m.y_stddev_0.value)
    return 2.3548 * np.sqrt(sx * sy) if 0.4 < sx < 8 and 0.4 < sy < 8 else None


def moffat_fit(data, x, y, fit):
    c = cutout(data, x, y, 12)
    if c is None:
        return None
    cut, (cx, cy) = c
    yy, xx = np.mgrid[0:cut.shape[0], 0:cut.shape[1]]
    model = models.Moffat2D(cut.max() - np.median(cut), cx, cy, 2.0, 2.5) + models.Const2D(np.median(cut))
    try:
        m = fit(model, xx, yy, cut, maxiter=400)
    except Exception:
        return None
    g = m[0]
    fw, beta = g.fwhm, g.alpha.value
    return (fw, beta) if 0.5 < fw < 20 and 0.5 < beta < 30 else None


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("image")
    ap.add_argument("report")
    ap.add_argument("--order", type=int, default=3)
    a = ap.parse_args()
    with fits.open(a.image) as h:
        d = h[0].data.astype(np.float64)
    lum = d if d.ndim == 2 else d.mean(axis=0)
    h_, w_ = lum.shape
    rows = list(csv.DictReader(open(a.report.rstrip("/") + "/field_stars.csv", encoding="utf-8")))
    xs = np.array([float(r["x"]) for r in rows])
    ys = np.array([float(r["y"]) for r in rows])
    fw = np.array([float(r["fwhm_px"]) for r in rows])
    opt = json.load(open(a.report.rstrip("/") + "/optics.json", encoding="utf-8"))
    fit = fitting.LevMarLSQFitter()

    ref = np.array([gauss_fwhm(lum, x, y, fit) or np.nan for x, y in zip(xs, ys)])
    ok = np.isfinite(ref)
    print(f"stars fitted: {ok.sum()} of {len(xs)}; all-star median FWHM valerian {np.median(fw):.2f}, "
          f"astropy Gaussian fits {np.nanmedian(ref):.2f}")
    zones = {
        "centre": (np.abs(xs / w_ - 0.5) < 0.17) & (np.abs(ys / h_ - 0.5) < 0.17),
        "top-left": (xs / w_ < 0.25) & (ys / h_ < 0.25),
        "top-right": (xs / w_ > 0.75) & (ys / h_ < 0.25),
        "bottom-left": (xs / w_ < 0.25) & (ys / h_ > 0.75),
        "bottom-right": (xs / w_ > 0.75) & (ys / h_ > 0.75),
    }
    print("zone | stars | valerian FWHM | astropy Gaussian-fit FWHM")
    for z in opt["tilt"]["zones"]:
        m = zones[z["name"]] & ok
        if m.sum() >= 5 and z["fwhm"] is not None:
            print(f"{z['name']} | {z['stars']} | {z['fwhm']:.2f} | {np.median(ref[m]):.2f}")
        else:
            print(f"{z['name']} | too few stars")

    # Moffat of the brightest isolated stars.
    order = np.argsort([-float(r["flux_adu"]) for r in rows])
    out = []
    for i in order:
        if np.min(np.hypot(xs - xs[i], ys - ys[i])[np.arange(len(xs)) != i]) < 25:
            continue
        r = moffat_fit(lum, xs[i], ys[i], fit)
        if r:
            out.append(r)
        if len(out) >= 50:
            break
    out = np.array(out)
    p = opt.get("psf")
    print(f"Moffat2D, {len(out)} stars: median FWHM {np.median(out[:, 0]):.2f} px, median beta {np.median(out[:, 1]):.2f}"
          + (f" | valerian profile fit: FWHM {p['fwhm_moffat']:.2f}, beta {p['beta']:.2f}, Gaussian FWHM {p['fwhm_gauss']:.2f}" if p else ""))

    # The surface of the FWHM map.
    keep = np.ones(len(xs), bool)
    terms = [(d_ - j, j) for d_ in range(a.order + 1) for j in range(d_ + 1)]

    def basis(x, y):
        u, v = 2 * x / w_ - 1, 2 * y / h_ - 1
        return np.stack([u ** i * v ** j for i, j in terms], axis=1)

    for _ in range(6):
        coef, *_ = np.linalg.lstsq(basis(xs[keep], ys[keep]), fw[keep], rcond=None)
        res = fw - basis(xs, ys) @ coef
        m = np.median(res[keep])
        sig = 1.4826 * np.median(np.abs(res[keep] - m))
        new = np.abs(res - m) <= 3 * sig
        if (new == keep).all():
            break
        keep = new
    grid = list(csv.DictReader(open(a.report.rstrip("/") + "/fwhm_map.csv", encoding="utf-8")))
    gx = np.array([float(r["x"]) for r in grid])
    gy = np.array([float(r["y"]) for r in grid])
    gv = np.array([float(r["value"]) for r in grid])
    diff = basis(gx, gy) @ coef - gv
    print(f"surface of degree {a.order}: {keep.sum()} of {len(xs)} stars kept; numpy vs valerian on the "
          f"{len(gx)}-point grid: max |difference| {np.abs(diff).max():.4f} px, rms {np.sqrt((diff ** 2).mean()):.4f} px")


if __name__ == "__main__":
    main()
