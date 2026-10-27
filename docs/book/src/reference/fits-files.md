# FITS files and other formats

## What Valerian reads

* **FITS images**, one image in the primary HDU: `NAXIS` 2 (mono or a raw Bayer mosaic) or 3
  (colour planes), `BITPIX` 8, 16, 32, -32 or -64. Physical values are `BZERO + BSCALE x
  raw`, in ADU, not scaled to 0 to 1. Any other `BITPIX` stops with `unsupported BITPIX`.
* Files ending `.fits`, `.fit` or `.fts` (any case) when you give a folder. A file named
  directly is read whatever its name.
* Rows are kept in the order stored in the file (row 0 is the first row of the data), as
  astropy does, so pixel coordinates agree with Python tools and with the WCS keywords.
  `ROWORDER` is carried but not interpreted. The Seestar S50 stores its rows top down and writes
  no `ROWORDER`; its WCS and `BAYERPAT` are for that stored order, so Valerian's previews,
  solutions and demosaicing agree with the Seestar's own without any flip.
* A star catalogue: see [The star catalogue file](catalogue-format.md).

## What Valerian writes

| File | Written by | Format |
|---|---|---|
| calibrated, registered, stacked, drizzled, background-corrected, stretched, starless, solved and colour-calibrated images, masters, models, masks, coverage and weight maps | the matching command | 32-bit float FITS (`BITPIX -32`), the input's header carried over, plus the keywords below |
| `--preview` | `stretch`, `starless` | 8-bit PNG |
| `--stars-out` of `photocal` | `photocal` | CSV: `x,y,g,bp_rp,flux_r,flux_g,flux_b,used` |
| `--report` | `stack`, `drizzle` | the per-frame table as text |
| `--stars-out` of `starless` | `starless` | FITS (original minus starless) |

The input is never changed.

## Privacy: what is left out of every file written

Valerian does not write where you were or which unit you used. Cameras and mounts put the
observing site and a serial number in every frame (the Seestar, for instance, writes
`SITELAT` and `SITELONG`), and a stacked image shared on a forum would carry them with it.
So by default the writers leave these cards out of every FITS file Valerian writes, whatever
command wrote it and whether the GUI or the command line did:

| Cards dropped | What they give away |
|---|---|
| `SITELAT`, `SITELONG`, `SITEELEV`, `SITE...`, `OBSGEO-X`, `-Y`, `-Z`, `-L`, `-B`, `-H`, `LAT-OBS`, `LONG-OBS`, `ALT-OBS`, `LATITUDE`, `LONGITUD`, `GEOLAT`, `GEOLON`, `GEOELEV`, `OBSLAT`, `OBSLONG` and similar | where the data were taken |
| `OBSERVAT`, `OBSERVER` | the observatory and who observed |
| any card with `SERIA` in its name (`SERIALNO`, `CAMSERIA`), `SERNUM`, `SERNO`, `DEVICEID`, `CAMERAID`, `UUID`, `MACADDR`, `HOSTNAME` | which unit took the frames |

Free-text cards are scanned too, so an identifier that sits inside a model name is removed and
the model stays: `TELESCOP = 'S50_a1b2c3d4'` is written as `S50`, and `Seestar S50 SN 12345678`
as `Seestar S50`. The scan covers every string value, its comment and every `COMMENT` and
`HISTORY` line, except dates, times and the WCS. A part is taken for an identifier when it is
six or more letters and digits that are all hexadecimal digits or mostly digits (after
`_`, `-`, `:` or `#`), a bare run of eight or more hexadecimal digits that mixes letters and
digits, or the word after `SN`, `S/N` or `SERIAL`. Camera names such as `ASI294MC` are kept.

Everything else is carried as before (exposure, gain, filter, `INSTRUME`, the model in
`TELESCOP`, `DATE-OBS`, the WCS). The PNG previews, reports, CSV, JSON and HTML files never held header values of
this kind and still do not. The input files are never changed, so they keep their cards.

Nothing Valerian computes needs the site today. When a feature needs it (airmass, altitude
and azimuth, field rotation), it will read the coordinates from the input and use them in
memory; they are not written.

To keep the cards, give `--keep-location` (before or after the command name) or set
`VALERIAN_KEEP_LOCATION=1`. It applies to every file the run writes. A position written into
a `COMMENT` or `HISTORY` line is not recognised, so check those if a camera writes one.

## Keywords Valerian reads

| Keyword | Used for |
|---|---|
| `IMAGETYP`, `FRAMETYP` | a single calibration file whose type says it is a master is used as it is |
| `EXPTIME`, `EXPOSURE` | dark scaling (`--scale-dark exposure`), `EXPTOTAL` |
| `CCD-TEMP`, `GAIN`, `OFFSET`, `XBINNING`, `FILTER` | carried through; not interpreted |
| `BAYERPAT`, `XBAYROFF`, `YBAYROFF` | the Bayer pattern (`RGGB`, `GRBG`, `GBRG`, `BGGR`), applied to the array as stored, shifted by the offsets |
| `RA`, `DEC`, `OBJCTRA`, `OBJCTDEC` | the starting position of `solve` (degrees, or sexagesimal text) |
| `FOCALLEN`, `XPIXSZ` | the starting pixel scale of `solve`: 206.265 x pixel size in micron / focal length in mm |
| `CTYPE1/2`, `CRVAL1/2`, `CRPIX1/2`, `CD`, `PC`, `CDELT`, `CROTA2` and the `A_`, `B_` SIP families | an existing world coordinate system (TAN, TAN-SIP) |
| `STRETCH` and the `STR...` cards | `stretch --invert` |
| `BIASSUB`, `FLATNORM`, `PEDESTAL`, `NCOMBINE` | master frames: whether the bias is already out of a dark, how a flat was normalised |

## Keywords Valerian writes

| Keyword | Written by | Meaning |
|---|---|---|
| `IMAGETYP` | calibrate, stack | `Master Dark`, `Master Flat`, `Calibrated Light`, `Master Light` |
| `NCOMBINE`, `EXPTOTAL` | masters, stack | frames combined, and the summed exposure in seconds |
| `BIASSUB` | master dark | `T` when the master bias was subtracted first |
| `STACKREF`, `STACKALG` | stack | reference frame number; the rejection and weights |
| `DRIZSCL`, `DRIZPIXF`, `DRIZREJ` | drizzle | scale, pixfrac and rejection |
| `STRETCH`, `STRBLACK`, `STRWHITE`, `STRMID`, `STRBETA`, `STRGS`, `STRGB`, `STRGSP`, `STRGLP`, `STRGHP` | stretch | the curve and its parameters, so `--invert` can undo it |
| `WCSAXES`, `CTYPE1/2`, `CUNIT1/2`, `CRVAL1/2`, `CRPIX1/2`, `CD1_1` to `CD2_2`, `RADESYS`, `EQUINOX`, and the SIP terms when used | solve | the world coordinate system |
| `PCCGAINR/G/B`, `PCCOFFR/G/B` | photocal | gains and offsets applied |
| `HISTORY` | every command | what was done and with which numbers |

Cards the writer generates itself (`SIMPLE`, `BITPIX`, `NAXIS`, `NAXIS1` to `3`, `EXTEND`,
`BZERO`, `BSCALE`, `BLANK`, `END`) are never copied from an input header.
