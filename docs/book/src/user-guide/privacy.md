# Privacy: location and serial numbers

Cameras and smart telescopes write where you are and which unit you own into every frame.
A Seestar, for instance, puts `SITELAT` and `SITELONG` in the header, and many cameras add
a serial number. If you share a stacked image, a forum reader can read your garden off it.

**Valerian does not write them.** Every file it writes, from any command, leaves out the
site and device cards of the input (the full list is in
[FITS files and other formats](../reference/fits-files.md#privacy-what-is-left-out-of-every-file-written)).
The unit's own identifier is removed from the free-text cards too: the Seestar S50 writes
`TELESCOP = 'S50_a1b2c3d4'`, and Valerian writes `S50`. The other cards, the exposure, filter,
gain, camera model and the plate solution, are kept.

## What you do

Nothing. Stack as usual:

```
valerian stack lights/ -o stack.fits
```

and open `stack.fits` in any FITS viewer: there is no `SITELAT`, `SITELONG`, `OBSGEO-*` or
serial card. The input frames are never changed, so they still hold them.

## Keeping them

If you want the cards (to feed the stack to a tool that needs the site), give `--keep-location`
to the command, or set `VALERIAN_KEEP_LOCATION=1` for the session:

```
valerian stack lights/ -o stack.fits --keep-location
```

It applies to every FITS file that run writes. The desktop app always leaves the cards out.

## How it works

The FITS writer, which every command and the app go through, skips each card whose keyword
names a place, an observer or a device. It also removes identifier-like parts from the text of the other cards. Where Valerian needs the site for the science
(airmass, altitude and azimuth, field rotation), it reads the coordinates from the input and
uses them in memory; nothing writes them back. PNG previews, CSV, JSON and HTML reports never
copied header values. The test `crates/valerian-cli/tests/privacy.rs` feeds made-up
Seestar-style headers through the writers and fails if any output holds one.

## Limits

* Identifiers are recognised by their shape (see the rules in
  [FITS files](../reference/fits-files.md#privacy-what-is-left-out-of-every-file-written)).
  A position written in a `COMMENT` or `HISTORY` line, or an identifier of an unusual
  shape, stays.
* Files written by an older Valerian still hold the cards. Run the step again, or remove the
  cards with another tool, before sharing them.
* File names are not touched. If yours hold a serial number, rename them.
