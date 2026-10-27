#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Check `valerian subframes` against astropy and photutils on single-plane frames.

    python compare_python_subframes.py REPORT_DIR LIGHTS_DIR [--frames 3 13 21 23]

where REPORT_DIR holds the `subframes.csv` written by `valerian subframes LIGHTS_DIR`
and LIGHTS_DIR the same monochrome frames. For each chosen frame it measures, with
different code:

1. the time stamp, as astropy.time.Time(DATE-OBS) seconds since the Unix epoch;
2. the sky level and the noise as photutils.Background2D (sigma-clipped, 128 pixel
   boxes, median background, standard-deviation rms) gives them;
3. the noise as the sigma-clipped standard deviation of horizontal differences two
   columns apart over the square root of two (astropy.stats.sigma_clipped_stats);
4. the FWHM, by centring stars with photutils.DAOStarFinder and fitting a
   two-dimensional Gaussian plus a constant to a 21 by 21 pixel cut-out of each of
   up to 75 bright, unsaturated stars (a 20 sigma detection threshold for frames
   sharper than 4 pixels, 5 sigma otherwise) with astropy.modeling (FWHM is 2.3548 times the
   geometric mean of the two sigmas, the median is taken over the stars).

Needs numpy, astropy and photutils (see python-parity-requirements.txt). Image data
is never committed.
"""
import argparse
import csv
import glob
import warnings

import numpy as np
from astropy.io import fits
from astropy.modeling import fitting, models
from astropy.stats import SigmaClip, sigma_clipped_stats
from astropy.time import Time
from photutils.background import Background2D, MedianBackground, StdBackgroundRMS
from photutils.detection import DAOStarFinder

warnings.filterwarnings("ignore")


def gaussian_fwhm(data, bkg, src):
    """Median FWHM of the bright unsaturated stars of `src`, by 2-D Gaussian fits."""
    sel = src[np.argsort(-src["peak"])][5:80]
    yy, xx = np.mgrid[0:21, 0:21]
    fit = fitting.LevMarLSQFitter()
    out = []
    for x, y in zip(sel["x_centroid"], sel["y_centroid"]):
        x0, y0 = int(round(x)), int(round(y))
        if x0 < 11 or y0 < 11 or x0 > data.shape[1] - 12 or y0 > data.shape[0] - 12:
            continue
        cut = data[y0 - 10:y0 + 11, x0 - 10:x0 + 11] - bkg[y0, x0]
        if cut.max() > 60000:
            continue
        model = models.Gaussian2D(cut.max(), 10, 10, 3, 3) + models.Const2D(0)
        try:
            fitted = fit(model, xx, yy, cut, maxiter=200)
        except Exception:
            continue
        sx, sy = abs(fitted.x_stddev_0.value), abs(fitted.y_stddev_0.value)
        if 0.5 < sx < 8 and 0.5 < sy < 8:
            out.append(2.3548 * np.sqrt(sx * sy))
    return float(np.median(out)) if out else float("nan")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("report")
    ap.add_argument("lights")
    ap.add_argument("--frames", type=int, nargs="+", default=[3, 13, 21, 23])
    a = ap.parse_args()
    files = sorted(glob.glob(a.lights.rstrip("/") + "/*.fit*"))
    rows = list(csv.DictReader(open(a.report.rstrip("/") + "/subframes.csv", encoding="utf-8")))
    print("frame | quantity | valerian | astropy/photutils")
    for i in a.frames:
        with fits.open(files[i]) as h:
            data = h[0].data.astype(np.float64)
            epoch = Time(h[0].header["DATE-OBS"], scale="utc").unix
        row = rows[i]
        bk = Background2D(
            data, (128, 128), filter_size=(3, 3),
            sigma_clip=SigmaClip(sigma=3.0, maxiters=10),
            bkg_estimator=MedianBackground(), bkg_rms_estimator=StdBackgroundRMS(),
        )
        diff = (data[:, 2:] - data[:, :-2]).ravel()
        _, _, sd = sigma_clipped_stats(diff, sigma=3.0, maxiters=10)
        sharp = float(row["fwhm_px"]) < 4
        # Sharp frames are full of noise peaks at 5 sigma: ask for 20 sigma stars.
        stars = DAOStarFinder(fwhm=2.5 if sharp else 7.0,
                              threshold=(20.0 if sharp else 5.0) * float(np.median(bk.background_rms)))
        src = stars(data - bk.background)
        fwhm = gaussian_fwhm(data, bk.background, src) if src is not None and len(src) > 20 else float("nan")
        print(f"{i} | time_utc | {row['time_utc']} | {Time(epoch, format='unix').isot[:19]}")
        print(f"{i} | sky_adu | {row['sky_adu']} | {np.median(bk.background):.1f}")
        print(f"{i} | noise_adu | {row['noise_adu']} | {sd / np.sqrt(2):.2f} (differences), "
              f"{np.median(bk.background_rms):.2f} (Background2D rms, includes structure)")
        print(f"{i} | fwhm_px | {row['fwhm_px']} | {fwhm:.2f} (Gaussian fits)")


if __name__ == "__main__":
    main()
