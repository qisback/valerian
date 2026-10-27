#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Check the astrometry and photometry outputs against astropy, photutils and numpy.

    python compare_python_astrometry.py residuals SOLVED.fits wcs_residuals.csv
    python compare_python_astrometry.py pcc SOLVED.fits pcc.csv [--aperture 5.1]
    python compare_python_astrometry.py photometry FRAMES_DIR lightcurve.csv
    python compare_python_astrometry.py zeropoint SOLVED.fits zeropoint.csv zeropoint.json calibrated_stars.csv [--aperture 5.6]
    python compare_python_astrometry.py residual-map wcs_residuals.csv residual_map.json

`residuals`: for every matched star of `valerian plot residuals` (or `valerian annotate`), the
catalogue position is projected to pixels with astropy.wcs.WCS read from the solved image's
header and compared with the position the residual implies (detected minus residual); the
residual vectors are recomputed.

`pcc`: for every star of the table written by `valerian photocal --report`, the flux in each
colour plane is measured with photutils (CircularAperture, sigma-clipped median of a
CircularAnnulus from the radius plus 2 pixels) at the same position and radius, the instrumental colours
`-2.5 log10(F_c / F_g)` are compared with the table's, and the lines are refitted with numpy
on the stars the fit used.

`photometry`: for each frame of a monochrome set, the flux of the star at the position and
aperture radius of `lightcurve.csv` is measured with photutils (exact pixel overlap,
sigma-clipped annulus median) and compared with the table's.

`zeropoint`: the green flux of every Gaia star of `zeropoint.csv` is measured with photutils at the
same position and radius, the zero point and colour term are refitted with numpy on the stars the
fit kept and compared with `zeropoint.json`, and the green fluxes of the first 300 clean stars of
`calibrated_stars.csv` are compared with photutils'.

`residual-map`: the linear terms of the residual field are refitted with numpy.linalg.lstsq on
`wcs_residuals.csv` and compared with `residual_map.json`.

Needs numpy, astropy and photutils (see python-parity-requirements.txt). Image data is never
committed.
"""
import argparse
import csv
import glob
import warnings

import numpy as np
from astropy.io import fits
from astropy.stats import SigmaClip
from astropy.wcs import WCS
from photutils.aperture import ApertureStats, CircularAnnulus, CircularAperture, aperture_photometry

warnings.filterwarnings("ignore")


def rows(path):
    return list(csv.DictReader(open(path, encoding="utf-8")))


def residuals(a):
    wcs = WCS(fits.getheader(a.image), naxis=2)
    r = rows(a.table)
    ra = np.array([float(x["ra_deg"]) for x in r])
    dec = np.array([float(x["dec_deg"]) for x in r])
    x = np.array([float(x["x"]) for x in r])
    y = np.array([float(x["y"]) for x in r])
    dx = np.array([float(x["dx_px"]) for x in r])
    dy = np.array([float(x["dy_px"]) for x in r])
    px, py = wcs.all_world2pix(ra, dec, 0)
    ex, ey = x - px, y - py
    print(f"{len(r)} matched stars")
    print(f"predicted pixel position, valerian (detected minus residual) against astropy WCS: "
          f"max |difference| {np.abs((x - dx) - px).max():.4f} px in x, {np.abs((y - dy) - py).max():.4f} px in y")
    print(f"residual vectors: astropy rms {np.sqrt((ex ** 2 + ey ** 2).mean()):.4f} px, valerian rms "
          f"{np.sqrt((dx ** 2 + dy ** 2).mean()):.4f} px; max |difference| of a component "
          f"{max(np.abs(ex - dx).max(), np.abs(ey - dy).max()):.4f} px")


def pcc(a):
    d = fits.getdata(a.image).astype(float)
    r = rows(a.table)
    x = np.array([float(s["x"]) for s in r])
    y = np.array([float(s["y"]) for s in r])
    bp = np.array([float(s["bp_rp"]) for s in r])
    used = np.array([s["used"] == "1" for s in r])
    mine = {c: np.array([float(s[k]) if s[k] else np.nan for s in r]) for c, k in ((0, "red_green_mag"), (2, "blue_green_mag"))}
    flux = []
    for p in range(3):
        pos = np.transpose([x, y])
        ap = CircularAperture(pos, a.aperture)
        an = CircularAnnulus(pos, a.aperture + 2.0, a.aperture + 2.0 + max(0.8 * a.aperture, 3.0))
        stats = ApertureStats(d[p], an, sigma_clip=SigmaClip(3.0))
        total = aperture_photometry(d[p], ap)["aperture_sum"]
        flux.append(np.array(total) - stats.median * ap.area)
    ref = {c: -2.5 * np.log10(flux[c] / flux[1]) for c in (0, 2)}
    for c, name in ((0, "red/green"), (2, "blue/green")):
        ok = np.isfinite(ref[c]) & np.isfinite(mine[c])
        diff = ref[c][ok] - mine[c][ok]
        print(f"{name}: {ok.sum()} stars, photutils minus valerian instrumental colour: median {np.median(diff):+.4f}, "
              f"rms {diff.std():.4f}, max {np.abs(diff).max():.4f} mag")
        sel = ok & used
        slope, zero = np.polyfit(bp[sel] - 0.82, ref[c][sel], 1)
        slope_v, zero_v = np.polyfit(bp[sel] - 0.82, mine[c][sel], 1)
        print(f"   line on the used stars: photutils fluxes slope {slope:+.3f} zero {zero:+.3f}; valerian fluxes slope {slope_v:+.3f} zero {zero_v:+.3f}")


def photometry(a):
    files = sorted(glob.glob(a.frames.rstrip("/") + "/*.fit*"))
    r = rows(a.table)
    diffs = []
    print("frame | valerian flux | photutils flux | ratio")
    for s in r:
        i = int(s["frame"])
        if s["ok"] != "1":
            continue
        d = fits.getdata(files[i]).astype(float)
        pos = [(float(s["x"]), float(s["y"]))]
        rad = float(s["aperture_px"])
        ap = CircularAperture(pos, rad)
        an = CircularAnnulus(pos, rad * 1.5, max(rad * 2.2, rad * 1.5 + 4.0))
        sky = ApertureStats(d, an, sigma_clip=SigmaClip(3.0)).median[0]
        flux = aperture_photometry(d, ap)["aperture_sum"][0] - sky * ap.area
        v = float(s["flux_adu"])
        diffs.append(v / flux)
        print(f"{i} | {v:.1f} | {flux:.1f} | {v / flux:.5f}")
    diffs = np.array(diffs)
    print(f"flux ratio valerian / photutils over {len(diffs)} frames: median {np.median(diffs):.5f}, "
          f"min {diffs.min():.5f}, max {diffs.max():.5f}")


def green_flux(d, pos, rad):
    ap = CircularAperture(pos, rad)
    an = CircularAnnulus(pos, rad + 2.0, rad + 2.0 + max(0.8 * rad, 3.0))
    sky = ApertureStats(d, an, sigma_clip=SigmaClip(3.0)).median
    return np.array(aperture_photometry(d, ap)["aperture_sum"]) - sky * ap.area


def zeropoint(a):
    import json
    d = fits.getdata(a.image).astype(float)
    cal = rows(a.zp_csv)
    js = json.load(open(a.zp_json, encoding="utf-8"))
    g = js["channels"][1]
    x = np.array([float(r["x"]) for r in cal])
    y = np.array([float(r["y"]) for r in cal])
    gm = np.array([float(r["g"]) for r in cal])
    bp = np.array([float(r["bp_rp"]) for r in cal])
    kept = np.array([r["kept_rgb"][1] == "1" for r in cal])
    flux = green_flux(d[1], np.transpose([x, y]), a.aperture)
    zp = gm + 2.5 * np.log10(flux)
    k, z = np.polyfit(bp[kept] - js["reference_bp_rp"], zp[kept], 1)
    res = zp[kept] - (z + k * (bp[kept] - js["reference_bp_rp"]))
    print(f"{kept.sum()} Gaia stars kept in green")
    print(f"zero point: valerian {g['zero']:.4f} +/- {g['zero_error']:.4f}, numpy on photutils fluxes {z:.4f}; "
          f"difference {z - g['zero']:+.4f} mag")
    print(f"colour term: valerian {g['colour_term']:+.4f}, numpy on photutils fluxes {k:+.4f}; "
          f"difference {k - g['colour_term']:+.4f}")
    print(f"scatter: valerian {g['scatter']:.4f}, numpy {res.std():.4f} mag")
    st = [r for r in rows(a.stars) if not set(r["flags"]) & set("SCN")][:300]
    pos = np.transpose([[float(r["x"]) for r in st], [float(r["y"]) for r in st]])
    fv = np.array([float(r["flux_g"]) for r in st])
    ratio = fv / green_flux(d[1], pos, a.aperture)
    print(f"catalogue green flux, valerian / photutils over {len(st)} clean stars: median {np.median(ratio):.5f}, "
          f"min {ratio.min():.5f}, max {ratio.max():.5f}")


def residual_map(a):
    import json
    r = rows(a.table)
    js = json.load(open(a.map_json, encoding="utf-8"))
    x = np.array([float(s["x"]) for s in r]) - js["width"] / 2
    y = np.array([float(s["y"]) for s in r]) - js["height"] / 2
    dx = np.array([float(s["dx_px"]) for s in r])
    dy = np.array([float(s["dy_px"]) for s in r])
    m = np.column_stack([np.ones_like(x), x, y])
    ax = np.linalg.lstsq(m, dx, rcond=None)[0]
    by = np.linalg.lstsq(m, dy, rcond=None)[0]
    lin = js["linear"]
    print(f"{len(r)} stars")
    print(f"a (valerian): {np.round(lin['a'], 7).tolist()}  numpy: {np.round(ax, 7).tolist()}")
    print(f"b (valerian): {np.round(lin['b'], 7).tolist()}  numpy: {np.round(by, 7).tolist()}")
    print(f"scale: valerian {lin['scale_ppm']:+.2f} ppm, numpy {(ax[1] + by[2]) / 2 * 1e6:+.2f} ppm; "
          f"max coefficient difference {max(np.abs(ax - lin['a']).max(), np.abs(by - lin['b']).max()):.2e}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="what", required=True)
    s = sub.add_parser("residuals")
    s.add_argument("image")
    s.add_argument("table")
    s = sub.add_parser("pcc")
    s.add_argument("image")
    s.add_argument("table")
    s.add_argument("--aperture", type=float, default=5.1)
    s = sub.add_parser("photometry")
    s.add_argument("frames")
    s.add_argument("table")
    s = sub.add_parser("zeropoint")
    s.add_argument("image")
    s.add_argument("zp_csv")
    s.add_argument("zp_json")
    s.add_argument("stars")
    s.add_argument("--aperture", type=float, default=5.6)
    s = sub.add_parser("residual-map")
    s.add_argument("table")
    s.add_argument("map_json")
    a = ap.parse_args()
    {"residuals": residuals, "pcc": pcc, "photometry": photometry, "zeropoint": zeropoint,
     "residual-map": residual_map}[a.what](a)


if __name__ == "__main__":
    main()
