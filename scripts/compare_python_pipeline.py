#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Compare a Valerian stack with the owner's Python pipeline on the same frames.

The Python side reproduces the author's astrophotography pipeline/measure.py
(`sep` detection) and register.py (`astroalign` registration, a bicubic warp,
sky subtraction, 99.5th percentile scaling, a 3-sigma one-pass clip and a
1/rms^2 weighted mean) on the calibrated frames Valerian wrote, so the two
stacks come from the same input. Nothing here is imported from that
repository; the algorithm is restated so this script runs on its own.

    python compare_python_pipeline.py FRAME.fits... --valerian STACK.fits \
        [--reference N] [--python-out PYSTACK.fits]

Needs numpy, scipy, astropy, scikit-image, sep and astroalign. Image data is
never committed; run it on a scratch copy.
"""
import argparse
import json
import sys

import astroalign as aa
import numpy as np
import sep
from astropy.io import fits
from astropy.stats import sigma_clip
from skimage.transform import warp

aa.MIN_MATCHES_FRACTION = 0.6
aa.NUM_NEAREST_NEIGHBORS = 8


def read(path):
    with fits.open(path, memmap=False) as hd:
        return hd[0].data.astype(np.float32), hd[0].header.copy()


def measure(data, max_stars=500):
    """Sky, noise, FWHM and a star list, as pipeline/measure.py does."""
    bkg = sep.Background(data, bw=64, bh=64, fw=3, fh=3)
    sub = data - bkg.back()
    rms = float(bkg.globalrms)
    objs = sep.extract(sub, 5.0, err=rms, minarea=5, deblend_cont=0.005)
    objs = objs[(objs["flag"] == 0) & (objs["npix"] < 3000)]
    if len(objs) > 10:
        rad0, _ = sep.flux_radius(sub, objs["x"], objs["y"], 6.0 * objs["a"], 0.5,
                                  normflux=objs["flux"])
        fwhm0 = float(np.median(rad0) * 2.0)
        objs = objs[objs["npix"] >= max(4, int(0.5 * np.pi * (fwhm0 / 2.0) ** 2))]
    order = np.argsort(objs["flux"])[::-1][:max_stars]
    objs = objs[order]
    radius, _ = sep.flux_radius(sub, objs["x"], objs["y"], 6.0 * objs["a"], 0.5,
                                normflux=objs["flux"])
    return dict(rms=rms, fwhm=float(np.median(radius) * 2.0),
                xy=np.column_stack([objs["x"], objs["y"]]).astype(np.float64))


def combine(cube, weights, sigma=3.0):
    """Weighted sigma-clipped mean, as pipeline/register.py does."""
    n, ny, nx = cube.shape
    out = np.zeros((ny, nx), np.float32)
    w = np.asarray(weights, np.float32)[:, None, None]
    for y0 in range(0, ny, 256):
        y1 = min(y0 + 256, ny)
        block = cube[:, y0:y1, :]
        clipped = sigma_clip(block, sigma=sigma, maxiters=1, axis=0, masked=True, copy=True)
        good = (~clipped.mask) & np.isfinite(block)
        wb = np.broadcast_to(w, block.shape) * good
        denom = wb.sum(axis=0)
        denom[denom == 0] = np.nan
        out[y0:y1, :] = np.nansum(np.where(np.isfinite(block), block, 0.0) * wb, axis=0) / denom
    return np.nan_to_num(out, nan=0.0)


def python_stack(paths, ref):
    stats = []
    for p in paths:
        data, _ = read(p)
        stats.append(measure(data))
        print(f"  python measure {p.split('/')[-1][-34:]}: fwhm {stats[-1]['fwhm']:.2f} px, "
              f"{len(stats[-1]['xy'])} stars, rms {stats[-1]['rms']:.2f}")
    planes, weights = [], []
    for i, p in enumerate(paths):
        data, _ = read(p)
        if i == ref:
            reg = data
        else:
            tform, _ = aa.find_transform(stats[i]["xy"], stats[ref]["xy"])
            reg = warp(data, inverse_map=tform.inverse, output_shape=data.shape, order=3,
                       mode="constant", cval=np.nan, preserve_range=True).astype(np.float32)
        reg -= float(np.nanmedian(reg))
        planes.append(reg)
        weights.append(1.0 / (stats[i]["rms"] ** 2))
    cube = np.stack(planes, axis=0)
    levels = np.array([np.nanpercentile(p, 99.5) for p in cube])
    ref_level = np.nanmedian(levels)
    for i, lv in enumerate(levels):
        if lv > 0:
            cube[i] *= float(ref_level / lv)
    return combine(cube, weights), stats


def star_table(img):
    bkg = sep.Background(img, bw=64, bh=64, fw=3, fh=3)
    sub = img - bkg.back()
    objs = sep.extract(sub, 5.0, err=float(bkg.globalrms), minarea=5, deblend_cont=0.005)
    objs = objs[(objs["flag"] == 0) & (objs["npix"] < 3000)]
    order = np.argsort(objs["flux"])[::-1][:800]
    return objs[order], float(bkg.globalrms), sub


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("frames", nargs="+")
    ap.add_argument("--valerian", required=True)
    ap.add_argument("--reference", type=int, default=0)
    ap.add_argument("--python-out")
    ap.add_argument("--json", help="write the measured numbers to this JSON file (for python_parity.py)")
    a = ap.parse_args()

    py, _ = python_stack(a.frames, a.reference)
    if a.python_out:
        fits.PrimaryHDU(py).writeto(a.python_out, overwrite=True)
    va, _ = read(a.valerian)

    # Compare where both have data: the Python stack is 0 outside its footprint,
    # Valerian's is NaN.
    ok = np.isfinite(va) & (py != 0)
    ok[:80, :] = ok[-80:, :] = False
    ok[:, :80] = ok[:, -80:] = False
    # Both on the same sky level: the Python stack has none, Valerian keeps a pedestal.
    sky_v = float(np.median(va[ok]))
    sky_p = float(np.median(py[ok]))
    dv, dp = va - sky_v, py - sky_p
    diff = (dv - dp)[ok]
    print(f"\ncompared {ok.sum() / 1e6:.2f} Mpx; sky: valerian {sky_v:.1f}, python {sky_p:.1f} (pedestal)")

    sv, nv, subv = star_table(np.where(np.isfinite(va), va, sky_v).astype(np.float32))
    sp, npy, subp = star_table(np.where(py != 0, py, 0.0).astype(np.float32))
    print(f"background noise: valerian {nv:.2f}, python {npy:.2f} ADU")
    d_rms = float(np.sqrt(np.mean(diff ** 2)))
    metrics = {"noise_valerian": nv, "noise_python": npy, "diff_rms_over_noise": d_rms / nv}
    print(f"pixel difference (sky removed): rms {d_rms:.2f} ADU = {d_rms / nv:.2f} x valerian noise, "
          f"{d_rms / npy:.2f} x python noise")

    # Match stars between the two stacks.
    px = np.column_stack([sp["x"], sp["y"]])
    dx, dy, fr = [], [], []
    for o in sv:
        d = np.hypot(px[:, 0] - o["x"], px[:, 1] - o["y"])
        j = int(np.argmin(d))
        if d[j] < 1.5 and 80 < o["x"] < py.shape[1] - 80 and 80 < o["y"] < py.shape[0] - 80:
            dx.append(sp[j]["x"] - o["x"])
            dy.append(sp[j]["y"] - o["y"])
            fr.append(o["flux"] / sp[j]["flux"])
    dx, dy, fr = map(np.array, (dx, dy, fr))
    print(f"{len(dx)} stars matched between the stacks: "
          f"mean offset ({dx.mean():+.4f}, {dy.mean():+.4f}) px, "
          f"scatter {np.hypot(dx, dy).std():.3f} px, rms {np.sqrt(np.mean(dx ** 2 + dy ** 2)):.3f} px")
    print(f"star flux valerian / python: median {np.median(fr):.4f}, scatter {np.std(fr):.4f}")
    metrics.update(stars_matched=len(dx), offset_dx=float(dx.mean()), offset_dy=float(dy.mean()),
                   offset_rms_px=float(np.sqrt(np.mean(dx ** 2 + dy ** 2))),
                   flux_ratio_median=float(np.median(fr)), flux_ratio_scatter=float(np.std(fr)))

    def fwhm(objs, sub):
        r, _ = sep.flux_radius(sub, objs["x"], objs["y"], 6.0 * objs["a"], 0.5, normflux=objs["flux"])
        return float(np.median(r) * 2.0)

    fv, fp = fwhm(sv, subv), fwhm(sp, subp)
    print(f"median star FWHM: valerian {fv:.2f} px, python {fp:.2f} px")
    metrics.update(fwhm_valerian=fv, fwhm_python=fp, fwhm_ratio=fv / fp)
    if a.json:
        with open(a.json, "w") as f:
            json.dump(metrics, f, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
