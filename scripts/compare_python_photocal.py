#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Check `valerian photocal` against an independent measurement of the same stars.

    python compare_python_photocal.py SOLVED.fits CALIBRATED.fits [--mag 13]

where SOLVED.fits is a linear colour image with a WCS (`valerian solve`) and
CALIBRATED.fits came from `valerian photocal SOLVED.fits -o CALIBRATED.fits`. It
measures what Valerian measured with different code:

1. the WCS is read with astropy,
2. Gaia DR3 sources with BP-RP to --mag (deeper than the G < 11 catalogue
   Valerian carries) are fetched from the ESA archive,
3. each star is centred with photutils' centre of mass on the luminance, the
   FWHM is measured with a Gaussian fit to the brightest ones, and the flux in
   each colour plane is `photutils` aperture photometry (2 FWHM radius) minus
   the sigma-clipped mean of an annulus,
4. red/green and blue/green against BP-RP are fitted with astropy's
   outlier-removing linear least squares (Valerian uses a Theil-Sen start and
   a clipped least-squares fit),
5. the gains that make BP-RP 0.82 neutral are compared with the PCCGAIN cards
   Valerian wrote, and the calibrated image is measured the same way to show
   the stars of that colour now come out neutral and the sky grey.

Needs numpy, scipy, astropy and photutils, and network access to the Gaia
archive. Image data is never committed.
"""
import argparse
import csv
import json
import io
import urllib.parse
import urllib.request

import numpy as np
from astropy.io import fits
from astropy.modeling import fitting, models
from astropy.stats import SigmaClip, sigma_clipped_stats
from astropy.wcs import WCS
from astropy.coordinates import SkyCoord
import astropy.units as u
from photutils.aperture import ApertureStats, CircularAnnulus, CircularAperture
from photutils.centroids import centroid_com

TAP = "https://gea.esac.esa.int/tap-server/tap/sync"
REFERENCE = 0.82


def gaia(ra, dec, radius, mag):
    adql = (
        "SELECT ra, dec, phot_g_mean_mag, bp_rp FROM gaiadr3.gaia_source "
        f"WHERE 1=CONTAINS(POINT('ICRS', ra, dec), CIRCLE('ICRS', {ra}, {dec}, {radius})) "
        f"AND phot_g_mean_mag < {mag} AND bp_rp IS NOT NULL"
    )
    data = urllib.parse.urlencode(
        {"REQUEST": "doQuery", "LANG": "ADQL", "FORMAT": "csv", "QUERY": adql}
    ).encode()
    with urllib.request.urlopen(urllib.request.Request(TAP, data=data), timeout=300) as r:
        rows = list(csv.reader(io.StringIO(r.read().decode())))[1:]
    return np.array([[float(v) for v in row] for row in rows])


def centre(luma, x, y, half=6):
    """Centre of mass of the background-subtracted cutout around (x, y)."""
    x0, y0 = int(round(x)), int(round(y))
    cut = luma[y0 - half : y0 + half + 1, x0 - half : x0 + half + 1].astype(float)
    cut = cut - np.median(cut)
    cut[cut < 0] = 0
    if cut.sum() <= 0:
        return None
    cx, cy = centroid_com(cut)
    return x0 - half + cx, y0 - half + cy


def fwhm_of(luma, pos):
    fit = fitting.LevMarLSQFitter()
    out = []
    half = 8
    yy, xx = np.mgrid[-half : half + 1, -half : half + 1]
    for x, y in pos:
        x0, y0 = int(round(x)), int(round(y))
        cut = luma[y0 - half : y0 + half + 1, x0 - half : x0 + half + 1].astype(float)
        if cut.shape != (2 * half + 1, 2 * half + 1):
            continue
        cut = cut - np.median(cut)
        g0 = models.Gaussian2D(cut.max(), 0, 0, 2.0, 2.0)
        try:
            g = fit(g0, xx, yy, cut)
        except Exception:  # noqa: BLE001
            continue
        s = 0.5 * (abs(g.x_stddev.value) + abs(g.y_stddev.value))
        if 0.8 < s < 8:
            out.append(2.3548 * s)
    return float(np.median(out))


def measure(planes, pos, r):
    ap = CircularAperture(pos, r=r)
    an = CircularAnnulus(pos, r_in=r + 2, r_out=r + 2 + max(3, 0.8 * r))
    flux = []
    for p in planes:
        bg = ApertureStats(p, an, sigma_clip=SigmaClip(sigma=3, maxiters=5)).mean
        tot = ApertureStats(p, ap).sum
        flux.append(tot - bg * ap.area)
    return np.array(flux).T


def fit_lines(colour, flux):
    x = colour - REFERENCE
    out = {}
    for name, c in (("red", 0), ("blue", 2)):
        y = -2.5 * np.log10(flux[:, c] / flux[:, 1])
        keep = np.ones(len(x), bool)
        for _ in range(8):
            a, b = np.polynomial.polynomial.polyfit(x[keep], y[keep], 1)
            res = y - a - b * x
            s = 1.4826 * np.median(np.abs(res[keep] - np.median(res[keep])))
            new = np.abs(res) <= 3 * max(s, 0.002)
            if new.sum() == keep.sum() or new.sum() < 10:
                break
            keep = new
        a, b = np.polynomial.polynomial.polyfit(x[keep], y[keep], 1)
        rms = np.sqrt(np.mean((y[keep] - a - b * x[keep]) ** 2))
        out[name] = (a, b, rms, keep.sum())
    return out


def stars(hdul_planes, wcs, cat, mag_lo, mag_hi):
    ny, nx = hdul_planes[0].shape
    luma = np.mean(hdul_planes, axis=0)
    px, py = wcs.world_to_pixel(SkyCoord(cat[:, 0] * u.deg, cat[:, 1] * u.deg))
    ok = (cat[:, 2] >= mag_lo) & (cat[:, 2] <= mag_hi) & (cat[:, 3] > 0.3) & (cat[:, 3] < 2.5)
    ok &= (px > 25) & (py > 25) & (px < nx - 25) & (py < ny - 25)
    idx = np.where(ok)[0]
    pos, keep = [], []
    for i in idx:
        c = centre(luma, px[i], py[i])
        if c is None or np.hypot(c[0] - px[i], c[1] - py[i]) > 3.0:
            continue
        pos.append(c)
        keep.append(i)
    return np.array(pos), cat[keep], luma


def isolated(pos, r):
    """Drop stars with another one inside the annulus."""
    d = np.hypot(pos[:, None, 0] - pos[None, :, 0], pos[:, None, 1] - pos[None, :, 1])
    np.fill_diagonal(d, np.inf)
    return d.min(axis=1) > 2 * r + 6


def run(path, cat, wcs, mag_hi, r=None):
    with fits.open(path) as h:
        planes = h[0].data.astype(float)
        header = h[0].header
    pos, c, luma = stars(planes, wcs, cat, 6.0, mag_hi)
    if r is None:
        order = np.argsort(c[:, 2])[:30]
        r = 2.0 * fwhm_of(luma, pos[order])
    sel = isolated(pos, r)
    pos, c = pos[sel], c[sel]
    flux = measure(planes, pos, r)
    snr_ok = np.all(flux > 0, axis=1)
    return planes, header, pos[snr_ok], c[snr_ok], flux[snr_ok], r


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("solved")
    ap.add_argument("calibrated")
    ap.add_argument("--mag", type=float, default=13.0, help="Gaia G limit for the independent set")
    ap.add_argument("--json", help="write the measured numbers to this JSON file (for python_parity.py)")
    a = ap.parse_args()

    with fits.open(a.solved) as h:
        wcs = WCS(h[0].header, naxis=2)
        ny, nx = h[0].data.shape[-2:]
    cen = wcs.pixel_to_world(nx / 2 - 0.5, ny / 2 - 0.5)
    radius = cen.separation(wcs.pixel_to_world(0, 0)).deg * 1.05
    cat = gaia(cen.ra.deg, cen.dec.deg, radius, a.mag)
    print(f"Gaia G < {a.mag} with BP-RP: {len(cat)} sources in {radius:.2f} deg")

    with fits.open(a.calibrated) as h:
        hdr = h[0].header
        val = {k: hdr.get(k) for k in ("PCCGAINR", "PCCGAING", "PCCGAINB", "PCCOFFR", "PCCOFFG", "PCCOFFB")}
    print("Valerian: " + ", ".join(f"{k} {v:.4f}" for k, v in val.items()))

    metrics = {}
    for label, mag_hi in (("same stars as Valerian (G < 11)", 11.0), (f"deeper set (G < {a.mag})", a.mag)):
        planes, header, pos, c, flux, r = run(a.solved, cat, wcs, mag_hi)
        snr = np.min(flux / np.sqrt(np.abs(flux) + 1), axis=1)
        good = snr > 3
        lines = fit_lines(c[good, 3], flux[good])
        gr = 10 ** (0.4 * lines["red"][0])
        gb = 10 ** (0.4 * lines["blue"][0])
        print(f"\n{label}: {good.sum()} stars, aperture radius {r:.1f} px")
        for n in ("red", "blue"):
            a0, b0, rms, k = lines[n]
            print(f"  {n:>5}: slope {b0:+.3f}, scatter {rms:.3f} mag, {k} kept")
        print(f"  astropy/photutils gains: R {gr:.4f}  B {gb:.4f}")
        print(f"  Valerian gains:          R {val['PCCGAINR']:.4f}  B {val['PCCGAINB']:.4f}"
              f"   (differences {100 * (val['PCCGAINR'] / gr - 1):+.2f} %, {100 * (val['PCCGAINB'] / gb - 1):+.2f} %)")
        key = "same" if mag_hi == 11.0 else "deep"
        metrics[f"{key}_red_gain_diff_percent"] = float(100 * (val["PCCGAINR"] / gr - 1))
        metrics[f"{key}_blue_gain_diff_percent"] = float(100 * (val["PCCGAINB"] / gb - 1))

    # The calibrated image, measured the same way.
    planes, header, pos, c, flux, r = run(a.calibrated, cat, wcs, 11.0)
    near = np.abs(c[:, 3] - REFERENCE) < 0.15
    if near.sum() >= 3:
        rg = np.median(flux[near, 0] / flux[near, 1])
        bg = np.median(flux[near, 2] / flux[near, 1])
        print(f"\ncalibrated image: {near.sum()} stars within 0.15 of BP-RP {REFERENCE}: "
              f"median R/G {rg:.3f}, B/G {bg:.3f} (1.000 is neutral)")
        metrics.update(neutral_rg=float(rg), neutral_bg=float(bg))
    lines = fit_lines(c[:, 3], flux)
    print(f"calibrated image, line value at BP-RP {REFERENCE}: red {lines['red'][0]:+.3f} mag, "
          f"blue {lines['blue'][0]:+.3f} mag (0 is neutral)")
    sky = [sigma_clipped_stats(p, sigma=2.5, maxiters=10)[1] for p in planes]
    print(f"sky medians after sigma clipping: R {sky[0]:.2f} G {sky[1]:.2f} B {sky[2]:.2f}")
    metrics["sky_spread_over_green"] = float((max(sky) - min(sky)) / abs(sky[1])) if sky[1] else 0.0
    if a.json:
        with open(a.json, "w") as f:
            json.dump(metrics, f, indent=2)


if __name__ == "__main__":
    main()
