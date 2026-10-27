# The star catalogue file (.vcat)

`valerian solve` and `valerian photocal` read a star catalogue, `gaia-dr3-g11.vcat`: every
Gaia DR3 source brighter than G = 11 (1,247,240 stars, 9,978,676 bytes) with its BP-RP
colour. It ships with every release package. ESA distributes Gaia data under CC BY-NC 3.0 IGO
(NonCommercial): the file is (c) ESA/Gaia/DPAC, is not covered by the GPL, may be used
and shared for non-commercial purposes with attribution, and needs ESA's authorisation
for commercial use (data.licences@esa.int). The licence text ships beside it, and the
acknowledgement is on [Credits and trademarks](../credits.md).

## Where Valerian looks

When `--catalogue` is not given, in this order, using the first that exists:

1. the file named by the `VALERIAN_CATALOGUE` environment variable;
2. `gaia-dr3-g11.vcat` beside the program;
3. `data/gaia-dr3-g11.vcat` beside the program;
4. `../share/valerian/gaia-dr3-g11.vcat` relative to the program (where the `.deb`, `.rpm` and
   AppImage put it);
5. for a build from source, `crates/valerian-core/data/gaia-dr3-g11.vcat` in the repository.

`valerian info` prints the one it found and its star count, or `catalogue: not found`. When
none exists, `solve` stops with `no star catalogue found; looked for gaia-dr3-g11.vcat in:`
followed by every place it tried.

## Format

All integers are little-endian. Positions are at epoch 2016.0 (Gaia DR3); proper motion is
not applied.

| Offset | Size | Content |
|---|---|---|
| 0 | 8 | magic `VCAT0002` (`VCAT0001`, which has no colour byte, still reads) |
| 8 | 4 | `u32` star count n |
| 12 | 4 | `f32` magnitude limit |
| 16 | 4 | `u32` declination band count (180, one degree each) |
| 20 | 4 | `f32` epoch of the positions |
| 24 | 8 | zero |
| 32 | 4 x (bands + 1) | `u32` index of the first star of each band, then n |
| then | 8 x n | the stars, band by band and by right ascension within a band |

Each star of version 2 is 8 bytes: `u32` right ascension (0 to 2^32 for 0 to 360 degrees),
`u16` declination within the band (0 to 65535 for one degree), `u8` (G + 2) x 10 rounded,
`u8` (BP-RP + 1) x 40 rounded (1 to 255, or 0 when Gaia has no BP and RP). Version 1 stars
are 7 bytes without the colour. `photocal` needs version 2.

## Building a deeper one

For fields narrower than about half a degree, G < 11 has too few stars. Build one from the
ESA Gaia archive (needs network access and only the Python standard library):

```sh
python3 scripts/build_catalogue.py --mag 13 -o gaia-dr3-g13.vcat
valerian solve image.fits --catalogue gaia-dr3-g13.vcat
```

G < 13 is 7.4 million stars, about 59 MB. See [How plate solving works](../user-guide/plate-solving.md#the-catalogue).
