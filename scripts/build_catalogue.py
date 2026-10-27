#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Build a Valerian star catalogue (.vcat) from Gaia DR3, standard library only.

    python3 scripts/build_catalogue.py --mag 11 -o crates/valerian-core/data/gaia-dr3-g11.vcat

asks the ESA Gaia archive (TAP, one query per slice of right ascension, split where the archive truncates at 50000 rows)
for every source brighter than --mag in G (with its BP-RP colour, which the
photometric colour calibration uses) and packs it into the format that
`valerian_core::catalogue` reads:

    0   8   magic "VCAT0002" (VCAT0001, without the colour byte, still reads)
    8   4   u32 star count n
    12  4   f32 magnitude limit
    16  4   u32 declination band count (180, one degree each)
    20  4   f32 epoch of the positions (2016.0, Gaia DR3)
    24  8   zero
    32  4*(bands+1)  u32 index of the first star of each band, then n
    ..  7*n stars, band by band and by right ascension within a band:
            u32 right ascension, 0..2^32 for 0..360 degrees
            u16 declination, 0..65535 for the band's one degree
            u8  (G + 2) * 10, rounded
            u8  (BP-RP + 1) * 40, rounded, 1..255; 0 when Gaia has no BP/RP

All integers are little-endian. A deeper catalogue for narrow fields is the
same command with a larger --mag (G < 13 is 7.4 million stars, 52 MB).

Gaia DR3 is (c) ESA/Gaia/DPAC. ESA states it is distributed under CC BY-NC 3.0 IGO
(NonCommercial; commercial use needs ESA authorisation: data.licences@esa.int).
Positions are at epoch 2016.0; proper motion is not applied.
"""
import argparse
import csv
import io
import struct
import sys
import time
import urllib.parse
import urllib.request

TAP = "https://gea.esac.esa.int/tap-server/tap/sync"
BANDS = 180


def query(mag, ra0, ra1):
    adql = (
        "SELECT ra, dec, phot_g_mean_mag, bp_rp FROM gaiadr3.gaia_source "
        f"WHERE phot_g_mean_mag < {mag} AND ra >= {ra0} AND ra < {ra1}"
    )
    data = urllib.parse.urlencode(
        {"REQUEST": "doQuery", "LANG": "ADQL", "FORMAT": "csv", "QUERY": adql}
    ).encode()
    for attempt in range(5):
        try:
            req = urllib.request.Request(TAP, data=data, headers={"User-Agent": "valerian-catalogue/1"})
            with urllib.request.urlopen(req, timeout=600) as r:
                return r.read().decode()
        except Exception as e:  # noqa: BLE001
            print(f"  retry {attempt + 1}: {e}", file=sys.stderr)
            time.sleep(5 * (attempt + 1))
    sys.exit("the Gaia archive did not answer")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--mag", type=float, default=11.0, help="G magnitude limit (default 11)")
    ap.add_argument("--slice", type=float, default=10.0, help="degrees of RA per query")
    ap.add_argument("-o", "--output", required=True)
    a = ap.parse_args()

    stars = []

    def fetch(ra0, ra1):
        """Rows for one RA slice, split in two while the archive truncates (50000)."""
        rows = list(csv.reader(io.StringIO(query(a.mag, ra0, ra1))))[1:]
        if len(rows) >= 50000 and ra1 - ra0 > 1e-3:
            mid = (ra0 + ra1) / 2
            return fetch(ra0, mid) + fetch(mid, ra1)
        return rows

    ra0 = 0.0
    while ra0 < 360.0:
        ra1 = min(360.0, ra0 + a.slice)
        rows = fetch(ra0, ra1)
        for ra, dec, g, c in rows:
            if ra and dec and g:
                stars.append((float(ra), float(dec), float(g), float(c) if c else None))
        print(f"RA {ra0:5.0f}..{ra1:5.0f}: {len(rows)} rows, {len(stars)} total", file=sys.stderr)
        ra0 = ra1

    packed = []
    for ra, dec, g, c in stars:
        band = min(BANDS - 1, int(dec + 90.0))
        ra_u = min(2**32 - 1, int(round(ra / 360.0 * 2**32)))
        dec_u = min(65535, max(0, int(round((dec + 90.0 - band) * 65535.0))))
        mag_u = min(255, max(0, int(round((g + 2.0) * 10.0))))
        col_u = 0 if c is None else min(255, max(1, int(round((c + 1.0) * 40.0))))
        packed.append((band, ra_u, dec_u, mag_u, col_u))
    packed.sort()
    starts = [0] * (BANDS + 1)
    for band, *_ in packed:
        starts[band + 1] += 1
    for i in range(BANDS):
        starts[i + 1] += starts[i]

    with open(a.output, "wb") as f:
        f.write(b"VCAT0002")
        f.write(struct.pack("<IfIf", len(packed), a.mag, BANDS, 2016.0))
        f.write(bytes(8))
        f.write(struct.pack(f"<{BANDS + 1}I", *starts))
        for _, ra_u, dec_u, mag_u, col_u in packed:
            f.write(struct.pack("<IHBB", ra_u, dec_u, mag_u, col_u))
    print(f"wrote {a.output}: {len(packed)} stars", file=sys.stderr)


if __name__ == "__main__":
    main()
