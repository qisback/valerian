#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Build crates/valerian-science/data/objects.csv, the bundled list of deep-sky objects.

    python build_objects.py NGC.csv > crates/valerian-science/data/objects.csv

NGC.csv is `database_files/NGC.csv` of OpenNGC (https://github.com/mattiaverga/OpenNGC,
copyright Mattia Verga, licence CC-BY-SA-4.0), a database of the NGC and IC objects and the
Messier catalogue. The output is a subset of that database and so is under the same licence
(see LICENSES/CC-BY-SA-4.0.txt, REUSE.toml and the acknowledgement in the book). It keeps:

* every Messier object, whatever its type;
* every other NGC or IC galaxy, cluster, planetary or other nebula or supernova remnant that is
  bright (V 11.5 or brighter, or B 12.5 or brighter) or large (a major axis of 8 arc minutes or
  more);

and drops duplicates, non-existent objects, plain stars and unclassified entries. Each row is
`name,type,ra_deg,dec_deg,major_arcmin,minor_arcmin,pos_angle_deg,mag,common_name`; `type` is
OpenNGC's code, `mag` is V (or B where V is missing), and an empty field is unknown.
"""
import csv
import sys

DROP = {"Dup", "NonEx", "Other", "*", "**", "*Ass", "Nova"}


def hms(s):
    h, m, sec = s.split(":")
    return 15.0 * (int(h) + int(m) / 60 + float(sec) / 3600)


def dms(s):
    sign = -1 if s.startswith("-") else 1
    d, m, sec = s.lstrip("+-").split(":")
    return sign * (int(d) + int(m) / 60 + float(sec) / 3600)


def num(s):
    try:
        return float(s)
    except ValueError:
        return None


def main():
    rows = list(csv.DictReader(open(sys.argv[1], encoding="utf-8"), delimiter=";"))
    out = []
    for r in rows:
        messier = r["M"].strip()
        # M24, M40, M45 and M73 are star clouds, doubles and associations: keep them.
        if r["Type"] in {"Dup", "NonEx", "Other", "Nova"} or (r["Type"] in DROP and not messier):
            continue
        if not r["RA"] or not r["Dec"]:
            continue
        v, b, maj = num(r["V-Mag"]), num(r["B-Mag"]), num(r["MajAx"])
        bright = (v is not None and v <= 11.5) or (b is not None and b <= 12.5)
        large = maj is not None and maj >= 8.0
        if not (messier or bright or large):
            continue
        raw = r["Name"]
        prefix = "NGC" if raw.startswith("NGC") else "IC"
        number = raw[len(prefix):].strip().lstrip("0") or "0"
        name = f"M{int(messier)}" if messier else f"{prefix} {number}"
        mag = v if v is not None else b
        common = r["Common names"].split(",")[0].strip()
        out.append((
            name, r["Type"], hms(r["RA"]), dms(r["Dec"]), maj, num(r["MinAx"]), num(r["PosAng"]), mag, common,
        ))

    def key(o):
        n = o[0]
        if n.startswith("M"):
            return (0, int(n[1:]), "")
        num_part = n.split()[1]
        digits = "".join(c for c in num_part if c.isdigit())
        return (1 if n.startswith("NGC") else 2, int(digits), num_part[len(digits):])

    out.sort(key=key)
    w = csv.writer(sys.stdout, lineterminator="\n")
    w.writerow(["name", "type", "ra_deg", "dec_deg", "major_arcmin", "minor_arcmin", "pos_angle_deg", "mag", "common_name"])
    for o in out:
        w.writerow([o[0], o[1], f"{o[2]:.4f}", f"{o[3]:.4f}"] + ["" if x is None else (f"{x:.1f}" if isinstance(x, float) else x) for x in o[4:]])


if __name__ == "__main__":
    main()
