#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Measure what drizzle gains over a plain stack, with astropy and photutils.

    python compare_python_drizzle.py --stack STACK.fits --drizzle DRIZZLE.fits \\
        [--truth FULL.fits --truth-scale 2]

The test needs frames that are *undersampled*, which good amateur data is not
(a star 2.3 pixels wide is already sampled). So bin the real frames to make
them undersampled and keep the unbinned stack as the answer:

    python make_binned_frames.py RAW_DIR binned/ --bin 2
    valerian stack binned/ -o stack_b.fits --reference N
    valerian drizzle binned/ -o drizzle_b.fits --scale 2 --pixfrac 0.7 --reference N
    valerian stack RAW_DIR -o full.fits --reference N        # the truth, 2x finer

STACK.fits is on the binned grid, DRIZZLE.fits is DRIZSCL times finer, and the
truth is --truth-scale times finer (2 for a 2x2 bin). Frame N is the same
frame for all, so the grids line up. The script finds bright isolated stars in
STACK.fits with photutils' DAOStarFinder and, for each star and image:

* fits a 2-D Gaussian (astropy's Levenberg-Marquardt fitter) and reports its
  FWHM in *binned* pixels (each image's divided by its scale),
* measures the diameter that holds half the star's flux (photutils aperture
  sums on a curve of growth), in the same units,
* and, after binning the drizzled image back down, the background noise.

The resolution gain is the ratio of FWHMs; how much of the loss that binning
caused drizzle gets back is `(stack - drizzle) / (stack - truth)`. Colour
images are averaged to luminance. Needs numpy, scipy, astropy and photutils.
Image data is never committed.
"""
import argparse
import json

import numpy as np
from astropy.io import fits
from astropy.modeling import fitting, models
from astropy.stats import sigma_clipped_stats
from photutils.aperture import CircularAperture, aperture_photometry
from photutils.detection import DAOStarFinder


def load(path):
    with fits.open(path) as h:
        d = h[0].data.astype(float)
        hdr = h[0].header
    return (d.mean(axis=0) if d.ndim == 3 else d), hdr


def isolated_stars(img, n, fwhm, edge=30, gap=20):
    _, med, std = sigma_clipped_stats(img, sigma=3)
    tab = DAOStarFinder(fwhm=fwhm, threshold=15 * std, exclude_border=True)(img - med)
    cols = tab.colnames
    xs = np.array(tab["x_centroid" if "x_centroid" in cols else "xcentroid"])
    ys = np.array(tab["y_centroid" if "y_centroid" in cols else "ycentroid"])
    pk = np.array(tab["peak"])
    ny, nx = img.shape
    ok = (xs > edge) & (ys > edge) & (xs < nx - edge) & (ys < ny - edge)
    ok &= pk < np.percentile(pk, 97)  # not saturated
    d = np.hypot(xs[:, None] - xs[None, :], ys[:, None] - ys[None, :])
    np.fill_diagonal(d, np.inf)
    ok &= d.min(axis=1) > gap
    idx = np.where(ok)[0]
    idx = idx[np.argsort(pk[idx])[::-1]][:n]
    return xs[idx], ys[idx]


def gaussian_fwhm(img, x, y, r):
    x0, y0 = int(round(x)), int(round(y))
    cut = img[y0 - r : y0 + r + 1, x0 - r : x0 + r + 1]
    if cut.shape != (2 * r + 1, 2 * r + 1) or not np.all(np.isfinite(cut)):
        return None
    yy, xx = np.mgrid[-r : r + 1, -r : r + 1]
    bg = np.median(np.concatenate([cut[0], cut[-1], cut[:, 0], cut[:, -1]]))
    g0 = models.Gaussian2D(cut.max() - bg, 0, 0, r / 3, r / 3) + models.Const2D(bg)
    try:
        g = fitting.LevMarLSQFitter()(g0, xx, yy, cut, maxiter=300)
    except Exception:  # noqa: BLE001
        return None
    sx, sy = abs(g[0].x_stddev.value), abs(g[0].y_stddev.value)
    if not (0.3 < sx < r and 0.3 < sy < r):
        return None
    return 2.3548 * np.sqrt(sx * sy)


def half_flux_diameter(img, x, y, rmax):
    radii = np.linspace(0.5, rmax, 40)
    h = 3 * rmax // 2
    _, med, _ = sigma_clipped_stats(img[int(y) - h : int(y) + h, int(x) - h : int(x) + h], sigma=2.5)
    tab = aperture_photometry(img - med, [CircularAperture((x, y), r=r) for r in radii])
    flux = np.array([tab[f"aperture_sum_{i}"][0] for i in range(len(radii))])
    if flux[-1] <= 0:
        return None
    return 2 * np.interp(0.5 * flux[-1], flux, radii)


def binned(img, s):
    k = int(round(s))
    h, w = (img.shape[0] // k) * k, (img.shape[1] // k) * k
    cut = img[:h, :w]
    return np.nanmean(cut.reshape(h // k, k, w // k, k), axis=(1, 3))


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--stack", required=True)
    ap.add_argument("--drizzle", required=True)
    ap.add_argument("--truth")
    ap.add_argument("--truth-scale", type=float, default=2.0)
    ap.add_argument("--stars", type=int, default=60)
    ap.add_argument("--fwhm", type=float, default=1.5, help="rough star FWHM for detection, binned pixels")
    ap.add_argument("--json", help="write the measured numbers to this JSON file (for python_parity.py)")
    a = ap.parse_args()

    st, _ = load(a.stack)
    dz, hdr = load(a.drizzle)
    s = float(hdr.get("DRIZSCL", dz.shape[1] / st.shape[1]))
    images = [("plain stack", st, 1.0), (f"drizzle (scale {s:g}, pixfrac {hdr.get('DRIZPIXF')})", dz, s)]
    if a.truth:
        tr, _ = load(a.truth)
        images.append(("full-resolution stack (truth)", tr, a.truth_scale))
    xs, ys = isolated_stars(st, a.stars, a.fwhm)
    got = {name: ([], []) for name, _, _ in images}
    rows = []
    for x, y in zip(xs, ys):
        vals = []
        for name, img, sc in images:
            ux, uy = (x + 0.5) * sc - 0.5, (y + 0.5) * sc - 0.5
            f = gaussian_fwhm(img, ux, uy, max(4, int(round(5 * sc))))
            h = half_flux_diameter(img, ux, uy, max(4, int(round(5 * sc))))
            vals.append(None if f is None or h is None else (f / sc, h / sc))
        if None not in vals:
            rows.append(vals)
    print(f"{len(rows)} isolated stars measured of {len(xs)} found (FWHM and half-flux diameter in binned pixels)")
    med = []
    for k, (name, _, _) in enumerate(images):
        f = np.median([r[k][0] for r in rows])
        h = np.median([r[k][1] for r in rows])
        med.append((f, h))
        print(f"  {name:<44} FWHM {f:.3f}   half-flux diameter {h:.3f}")
    gain_f, gain_h = med[0][0] / med[1][0], med[0][1] / med[1][1]
    metrics = {"stars_measured": len(rows), "fwhm_stack": med[0][0], "fwhm_drizzle": med[1][0],
               "gain_fwhm": gain_f, "gain_half_flux": gain_h}
    print(f"resolution gain of drizzle over the plain stack: FWHM x{gain_f:.3f}, half-flux diameter x{gain_h:.3f}")
    ratios = np.array([r[0][0] / r[1][0] for r in rows])
    print(f"  per-star FWHM ratio: median {np.median(ratios):.3f}, 16th to 84th percentile "
          f"{np.percentile(ratios, 16):.3f} to {np.percentile(ratios, 84):.3f}")
    if a.truth:
        lost = med[0][0] - med[2][0]
        print(f"  of the {lost:.3f} px of FWHM that binning cost, drizzle gets back "
              f"{100 * (med[0][0] - med[1][0]) / lost:.0f} %")
        metrics["recovered_percent"] = 100 * (med[0][0] - med[1][0]) / lost
    _, _, sd = sigma_clipped_stats(dz, sigma=2.5, maxiters=10)
    _, _, sb = sigma_clipped_stats(binned(dz, s), sigma=2.5, maxiters=10)
    _, _, ss = sigma_clipped_stats(st, sigma=2.5, maxiters=10)
    print(f"background noise: stack {ss:.2f}; drizzle {sd:.2f} per output pixel, {sb:.2f} after binning "
          f"to the input grid")
    metrics.update(noise_stack=float(ss), noise_drizzle_binned=float(sb))
    if a.json:
        with open(a.json, "w") as f:
            json.dump(metrics, f, indent=2)


if __name__ == "__main__":
    main()
