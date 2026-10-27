#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Compare `valerian background` with the owner's Python gradient removal.

The Python side restates `target_mask` and `remove_gradient` from the
pipeline's colour.py: an ellipse over the bright target sized from the data,
tile medians (step max(48, min(h, w) / 40)) outside it, a plane fitted to them
with three rounds of 2.5-sigma clipping, and the plane's tilt removed with its
mean put back. Nothing is imported from that repository.

    valerian background IN.fits -o OUT.fits --save-model MODEL.fits \
        --degree 1 --exclude-target --tile STEP
    python compare_python_background.py IN.fits --valerian OUT.fits --model MODEL.fits

STEP is printed by this script as "tile step"; the pipeline's is the same
formula. For a colour image each plane is compared. Needs numpy and astropy.
Image data is never committed.
"""
import argparse
import json

import numpy as np
from astropy.io import fits


def target_mask(img, frac=0.45):
    ny, nx = img.shape
    yy, xx = np.mgrid[0:ny, 0:nx]
    cy, cx = ny / 2.0, nx / 2.0
    r = np.hypot((xx - cx) / (nx / 2.0), (yy - cy) / (ny / 2.0))
    sky = float(np.median(img))
    signal = np.clip(img - sky, 0, None)
    total = signal.sum()
    if total <= 0:
        return r < 0.5
    for cut in np.arange(0.25, 0.96, 0.05):
        if signal[r < cut].sum() >= frac * total:
            return r < min(cut + 0.15, 0.95)
    return r < 0.75


def tile_step(img):
    ny, nx = img.shape
    return max(48, min(ny, nx) // 40)


def remove_gradient(img, mask):
    ny, nx = img.shape
    step = tile_step(img)
    xs, ys, zs = [], [], []
    for y0 in range(0, ny - step, step):
        for x0 in range(0, nx - step, step):
            if mask[y0:y0 + step, x0:x0 + step].any():
                continue
            xs.append(x0 + step / 2.0)
            ys.append(y0 + step / 2.0)
            zs.append(np.median(img[y0:y0 + step, x0:x0 + step]))
    if len(zs) < 12:
        return None, None, len(zs)
    xs, ys, zs = map(np.asarray, (xs, ys, zs))
    keep = np.ones(len(zs), bool)
    for _ in range(3):
        a = np.column_stack([xs[keep], ys[keep], np.ones(keep.sum())])
        coef, *_ = np.linalg.lstsq(a, zs[keep], rcond=None)
        resid = zs - (coef[0] * xs + coef[1] * ys + coef[2])
        s = 1.4826 * np.median(np.abs(resid - np.median(resid)))
        if s <= 0:
            break
        keep = np.abs(resid - np.median(resid)) < 2.5 * s
    yy, xx = np.mgrid[0:ny, 0:nx]
    plane = (coef[0] * xx + coef[1] * yy + coef[2]).astype(np.float32)
    plane -= float(plane.mean())
    return img - plane, plane, len(zs)


def plane_fit(model):
    """Least-squares plane through a model image: (slope x, slope y, mean)."""
    ny, nx = model.shape
    yy, xx = np.mgrid[0:ny:8, 0:nx:8]
    a = np.column_stack([xx.ravel(), yy.ravel(), np.ones(xx.size)])
    c, *_ = np.linalg.lstsq(a, model[::8, ::8].ravel().astype(np.float64), rcond=None)
    return c


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("linear")
    ap.add_argument("--valerian", required=True, help="corrected image from valerian background")
    ap.add_argument("--model", required=True, help="--save-model output")
    ap.add_argument("--json", help="write the measured numbers to this JSON file (for python_parity.py)")
    a = ap.parse_args()
    lin = fits.getdata(a.linear).astype(np.float64)
    val = fits.getdata(a.valerian).astype(np.float64)
    mod = fits.getdata(a.model).astype(np.float64)
    planes = lin[None] if lin.ndim == 2 else lin
    vplanes = val[None] if val.ndim == 2 else val
    mplanes = mod[None] if mod.ndim == 2 else mod
    metrics = {}
    for i, (p, v, m) in enumerate(zip(planes, vplanes, mplanes)):
        mask = target_mask(p)
        step = tile_step(p)
        py, plane, tiles = remove_gradient(p, mask)
        print(f"plane {i}: tile step {step}, target mask covers {mask.mean() * 100:.0f}% of the frame")
        cv = plane_fit(m)
        print(f"  valerian model: slope x {cv[0]:+.5f}, y {cv[1]:+.5f} ADU/px, "
              f"tilt across the frame {abs(cv[0]) * p.shape[1] + abs(cv[1]) * p.shape[0]:.1f} ADU")
        if py is None:
            print(f"  python: declined, only {tiles} clean tiles")
            continue
        cp = plane_fit(plane)
        print(f"  python model:   slope x {cp[0]:+.5f}, y {cp[1]:+.5f} ADU/px, "
              f"tilt across the frame {abs(cp[0]) * p.shape[1] + abs(cp[1]) * p.shape[0]:.1f} ADU")
        d = (m - m.mean()) - plane.astype(np.float64)
        print(f"  model difference (valerian - python, means removed): rms {d.std():.2f}, "
              f"max {np.abs(d).max():.2f} ADU")
        print(f"  corrected-image difference: rms {np.std(v - py):.2f}, "
              f"mean level python {py.mean():.1f} valerian {v.mean():.1f}")
        metrics[f"plane{i}_model_diff_rms"] = float(d.std())
        metrics[f"plane{i}_model_diff_max"] = float(np.abs(d).max())
        metrics[f"plane{i}_corrected_diff_rms"] = float(np.std(v - py))
        metrics[f"plane{i}_tilt_valerian"] = float(abs(cv[0]) * p.shape[1] + abs(cv[1]) * p.shape[0])
        metrics[f"plane{i}_tilt_python"] = float(abs(cp[0]) * p.shape[1] + abs(cp[1]) * p.shape[0])
    if a.json:
        with open(a.json, "w") as f:
            json.dump(metrics, f, indent=2)


if __name__ == "__main__":
    main()
