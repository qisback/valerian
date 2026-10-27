#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
"""Write a small synthetic imaging session as FITS files, standard library only.

    python3 scripts/make_synthetic_session.py demo/

makes demo/{lights,darks,flats,dark-flats}/ of 320 x 240 frames: a made-up star
field that drifts a few pixels from frame to frame, with a bias level, dark
current, hot pixels, vignetting, dust, a cosmic ray here and there and one frame
of poor seeing. It is a way to try the tools, not a picture of the sky. The same
seed gives the same files.
"""
import math
import os
import random
import struct
import sys

W, H = 320, 240
LIGHT_S, FLAT_S = 60.0, 2.0
BIAS, DARK_RATE, SKY = 500.0, 0.8, 200.0   # ADU, ADU per second, ADU
READ_NOISE = 6.0
N_LIGHTS, N_DARKS, N_FLATS = 12, 8, 8


def card(key, value, comment=""):
    if isinstance(value, bool):
        v = "T" if value else "F"
        v = v.rjust(20)
    elif isinstance(value, str):
        v = ("'" + value.replace("'", "''").ljust(8) + "'").ljust(20)
    else:
        v = repr(float(value)) if isinstance(value, float) else str(value)
        v = v.rjust(20)
    text = f"{key:<8}= {v}" + (f" / {comment}" if comment else "")
    return text[:80].ljust(80)


def write_fits(path, data, header):
    cards = [
        card("SIMPLE", True), card("BITPIX", -32), card("NAXIS", 2),
        card("NAXIS1", W), card("NAXIS2", H),
    ]
    cards += [card(k, v, c) for k, v, c in header]
    cards.append("END".ljust(80))
    head = "".join(cards)
    head += " " * (-len(head) % 2880)
    body = b"".join(struct.pack(f">{W}f", *data[y * W:(y + 1) * W]) for y in range(H))
    body += b"\0" * (-len(body) % 2880)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "wb") as f:
        f.write(head.encode("ascii") + body)


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "synthetic-session"
    rng = random.Random(20260719)

    stars = [(rng.uniform(15, W - 15), rng.uniform(15, H - 15), rng.uniform(300, 9000))
             for _ in range(70)]
    hot = {rng.randrange(W * H): rng.uniform(600, 2500) for _ in range(40)}
    dust = [(rng.uniform(40, W - 40), rng.uniform(40, H - 40), rng.uniform(6, 12)) for _ in range(3)]

    def flat_gain(x, y):
        r2 = ((x - W / 2) / (W / 2)) ** 2 + ((y - H / 2) / (H / 2)) ** 2
        g = 1.0 - 0.30 * r2 / 2.0
        for dx, dy, r in dust:
            d = math.hypot(x - dx, y - dy)
            if d < r:
                g *= 1.0 - 0.08 * (1.0 - d / r)
        return g

    gain = [flat_gain(x, y) for y in range(H) for x in range(W)]

    def noise(sigma):
        return rng.gauss(0.0, sigma)

    def meta(kind, exp):
        return [("IMAGETYP", kind, "kind of frame"), ("EXPTIME", float(exp), "[s] exposure"),
                ("FILTER", "Ha", ""), ("GAIN", 120, ""), ("CCD-TEMP", -10.0, "[C]")]

    # Darks and dark flats: bias + dark current + hot pixels.
    def dark(exp):
        d = []
        for i in range(W * H):
            level = BIAS + DARK_RATE * exp + hot.get(i, 0.0) * exp / LIGHT_S
            d.append(level + noise(READ_NOISE))
        return d

    for n in range(N_DARKS):
        write_fits(f"{out}/darks/dark_{n + 1:02d}.fits", dark(LIGHT_S), meta("Dark Frame", LIGHT_S))
        write_fits(f"{out}/dark-flats/darkflat_{n + 1:02d}.fits", dark(FLAT_S), meta("Dark Frame", FLAT_S))

    for n in range(N_FLATS):
        lamp = 22000.0 * rng.uniform(0.97, 1.03)
        f = []
        for i in range(W * H):
            level = BIAS + DARK_RATE * FLAT_S + hot.get(i, 0.0) * FLAT_S / LIGHT_S + lamp * gain[i]
            f.append(level + noise(math.sqrt(level) * 0.8 + READ_NOISE))
        write_fits(f"{out}/flats/flat_{n + 1:02d}.fits", f, meta("Flat Field", FLAT_S))

    for n in range(N_LIGHTS):
        # The field drifts, as it does with imperfect guiding.
        ox, oy = n * 0.6 - 3.0 + rng.uniform(-0.5, 0.5), -n * 0.4 + 2.0 + rng.uniform(-0.5, 0.5)
        fwhm = 6.0 if n == 7 else rng.uniform(3.2, 3.9)      # frame 8: poor seeing
        sigma = fwhm / 2.355
        sky = [SKY * gain[i] for i in range(W * H)]
        img = [0.0] * (W * H)
        for sx, sy, flux in stars:
            cx, cy = sx + ox, sy + oy
            r = int(4 * sigma) + 1
            amp = flux / (2 * math.pi * sigma * sigma)
            for y in range(max(0, int(cy) - r), min(H, int(cy) + r + 1)):
                for x in range(max(0, int(cx) - r), min(W, int(cx) + r + 1)):
                    img[y * W + x] += amp * math.exp(-((x - cx) ** 2 + (y - cy) ** 2) / (2 * sigma * sigma))
        out_px = []
        for i in range(W * H):
            signal = img[i] * gain[i] + sky[i]
            level = BIAS + DARK_RATE * LIGHT_S + hot.get(i, 0.0) + signal
            out_px.append(level + noise(math.sqrt(max(signal, 1.0)) + READ_NOISE))
        if n % 4 == 1:                                          # a cosmic ray hit
            cr = rng.randrange(W * H)
            out_px[cr] += 4000.0
        write_fits(f"{out}/lights/light_{n + 1:02d}.fits", out_px, meta("Light Frame", LIGHT_S))

    print(f"wrote {N_LIGHTS} lights, {N_DARKS} darks, {N_FLATS} flats and {N_DARKS} dark flats to {out}/")


if __name__ == "__main__":
    main()
