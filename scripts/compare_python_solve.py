#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Check `valerian solve` against an independent solve of the same stars.

    python compare_python_solve.py SOLVED.fits [--mag 15]

where SOLVED.fits came from `valerian solve IMAGE.fits -o SOLVED.fits`. It does
what the owner's Python pipeline does, without importing it:

1. read the WCS Valerian wrote with astropy (which also checks that the TAN-SIP
   keywords are valid, since astropy is a different implementation),
2. detect the stars again with `sep`, a different detector,
3. fetch Gaia DR3 sources to --mag for the field from the ESA archive, a deeper
   catalogue than the G < 11 one Valerian solved against,
4. match them through the Valerian WCS and report the offsets,
5. fit a plain TAN WCS to those matches with `fit_wcs_from_points`, as the
   pipeline's astrometry.py does, and report how far it is from the Valerian
   one at the centre and corners.

Needs numpy, scipy, astropy and sep, and network access to the Gaia archive.
Image data is never committed.
"""
import argparse
import csv
import json
import io
import urllib.parse
import urllib.request

import numpy as np
import sep
from astropy.io import fits
from astropy.wcs import WCS
from astropy.wcs.utils import fit_wcs_from_points
from astropy.coordinates import SkyCoord
import astropy.units as u
from scipy.spatial import cKDTree

TAP = "https://gea.esac.esa.int/tap-server/tap/sync"


def gaia(ra, dec, radius, mag):
    adql = (
        "SELECT ra, dec, phot_g_mean_mag FROM gaiadr3.gaia_source "
        f"WHERE 1=CONTAINS(POINT('ICRS', ra, dec), CIRCLE('ICRS', {ra}, {dec}, {radius})) "
        f"AND phot_g_mean_mag < {mag}"
    )
    data = urllib.parse.urlencode(
        {"REQUEST": "doQuery", "LANG": "ADQL", "FORMAT": "csv", "QUERY": adql}
    ).encode()
    with urllib.request.urlopen(urllib.request.Request(TAP, data=data), timeout=300) as r:
        rows = list(csv.reader(io.StringIO(r.read().decode())))[1:]
    return np.array([[float(a), float(b), float(c)] for a, b, c in rows if a and b and c])


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("solved")
    ap.add_argument("--mag", type=float, default=15.0)
    ap.add_argument("--json", help="write the measured numbers to this JSON file (for python_parity.py)")
    a = ap.parse_args()

    with fits.open(a.solved) as hdul:
        data = hdul[0].data.astype(np.float64)
        header = hdul[0].header
    img = data.mean(axis=0) if data.ndim == 3 else data
    wcs = WCS(header, naxis=2)
    ny, nx = img.shape
    print(f"{a.solved}: {nx} x {ny}, {wcs.wcs.ctype[0]}, sip: {wcs.sip is not None}")
    centre = wcs.pixel_to_world(nx / 2 - 0.5, ny / 2 - 0.5)
    corner = wcs.pixel_to_world(0, 0)
    radius = centre.separation(corner).deg * 1.05
    print(f"centre {centre.ra.deg:.5f} {centre.dec.deg:+.5f}, search radius {radius:.3f} deg")

    bkg = sep.Background(np.ascontiguousarray(img, dtype=np.float32), bw=64, bh=64)
    sub = np.ascontiguousarray(img - bkg.back(), dtype=np.float32)
    objs = sep.extract(sub, 8.0, err=bkg.globalrms, minarea=9)
    objs = objs[(objs["flag"] == 0) & (objs["npix"] < 3000)]
    objs = objs[np.argsort(objs["flux"])[::-1][:800]]
    src = np.column_stack([objs["x"], objs["y"]])
    print(f"sep found {len(src)} stars")

    cat = gaia(centre.ra.deg, centre.dec.deg, radius, a.mag)
    px, py = wcs.world_to_pixel(SkyCoord(cat[:, 0] * u.deg, cat[:, 1] * u.deg))
    inside = (px > 0) & (py > 0) & (px < nx - 1) & (py < ny - 1)
    cat, px, py = cat[inside], px[inside], py[inside]
    print(f"Gaia G < {a.mag}: {len(cat)} sources in the frame")

    tree = cKDTree(np.column_stack([px, py]))
    d, j = tree.query(src, distance_upper_bound=3.0)
    ok = np.isfinite(d)
    # One catalogue star per detection: keep the nearest detection of each.
    best = {}
    for i in np.where(ok)[0]:
        if j[i] not in best or d[i] < d[best[j[i]]]:
            best[j[i]] = i
    ii = np.array(sorted(best.values()))
    jj = np.array([j[i] for i in ii])
    dx, dy = src[ii, 0] - px[jj], src[ii, 1] - py[jj]
    scale = np.sqrt(abs(np.linalg.det(wcs.pixel_scale_matrix))) * 3600.0
    r = np.hypot(dx, dy)
    print(f"matched {len(ii)} stars within 3 px")
    print(f"median offset {np.median(r):.3f} px ({np.median(r) * scale:.3f} arcsec), "
          f"rms {np.sqrt(np.mean(r ** 2)):.3f} px, 90th percentile {np.percentile(r, 90):.3f} px")

    refit = fit_wcs_from_points((src[ii, 0], src[ii, 1]),
                                SkyCoord(cat[jj, 0] * u.deg, cat[jj, 1] * u.deg),
                                projection="TAN")
    worst = 0.0
    for x, y in [(nx / 2, ny / 2), (0, 0), (nx - 1, 0), (0, ny - 1), (nx - 1, ny - 1)]:
        s = wcs.pixel_to_world(x, y).separation(refit.pixel_to_world(x, y)).arcsec
        worst = max(worst, s)
    print(f"pipeline-style TAN refit vs Valerian WCS: worst difference at centre and corners "
          f"{worst:.3f} arcsec ({worst / scale:.3f} px)")
    if a.json:
        with open(a.json, "w") as f:
            json.dump({"stars_matched": len(ii), "median_offset_px": float(np.median(r)),
                       "rms_offset_px": float(np.sqrt(np.mean(r ** 2))), "p90_offset_px": float(np.percentile(r, 90)),
                       "refit_worst_arcsec": float(worst), "refit_worst_px": float(worst / scale)}, f, indent=2)


if __name__ == "__main__":
    main()
