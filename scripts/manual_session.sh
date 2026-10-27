#!/bin/bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.
#
# Regenerate the terminal output and the pictures of the manual's "A first session" page
# from staged real frames. Run it where the data is (not committed anywhere):
#
#   scripts/manual_session.sh VALERIAN HA_ROOT OSC_DIR CATALOGUE WORKDIR
#
#   VALERIAN   the valerian binary (cargo build --release -p valerian-cli)
#   HA_ROOT    monochrome session: calibration/*/{LIGHT,FLAT,DARKFLAT}, deepsky/*/DARK
#   OSC_DIR    directory of raw one-shot-colour Light_*.fit frames
#   CATALOGUE  gaia-dr3-g11.vcat (crates/valerian-core/data)
#   WORKDIR    scratch; logs are kept in WORKDIR/logs, one NAME.out (stdout) and NAME.err (stderr)
#
# The pictures are the --preview PNGs; the book keeps them as JPEGs under
# docs/book/src/images/session/. The text blocks on the page are pasted from the logs.
set -eu
V=$(realpath "$1"); H=$(realpath "$2"); O=$(realpath "$3"); export VALERIAN_CATALOGUE=$(realpath "$4")
W=$5
rm -rf "$W"; mkdir -p "$W/logs" "$W/mono"/{lights,darks,flats,dark-flats} "$W/colour/lights"
W=$(realpath "$W")

run() { n=$1; shift; echo "### $n: $*"; "$@" > "$W/logs/$n.out" 2> "$W/logs/$n.err"; }

cd "$W/mono"
for f in $(ls "$H"/calibration/*/LIGHT/*.fits | head -5); do ln -s "$f" lights/; done
for f in "$H"/deepsky/*/DARK/*.fits; do ln -s "$f" darks/; done
for f in "$H"/calibration/*/FLAT/*.fits; do ln -s "$f" flats/; done
for f in "$H"/calibration/*/DARKFLAT/*.fits; do ln -s "$f" dark-flats/; done
run calibrate "$V" calibrate lights/ --dark darks/ --flat flats/ --dark-flat dark-flats/ -o calibrated --masters masters
run register "$V" register calibrated/ -o registered
run stack "$V" stack calibrated/ -o stack.fits --report report.txt --coverage coverage.fits
run info "$V" info stack.fits --stats
run background "$V" background stack.fits -o flat.fits --degree 1 --save-model glow.fits
run oneframe "$V" stretch "$(ls calibrated/*.fits | head -1)" --preview one-frame.png --preview-size 1200
run stretch "$V" stretch flat.fits -o stretched.fits --preview stretched.png --preview-size 1200
run starless "$V" starless flat.fits -o starless.fits --stars-out stars.fits --preview starless.png --preview-size 700

cd "$W/colour"
for f in "$O"/*.fit; do ln -s "$f" lights/; done
run cstack "$V" stack lights/ -o stack.fits --report report.txt
run cinfo "$V" info stack.fits --stats
run cbackground "$V" background stack.fits -o flat.fits --degree 2 --save-model glow.fits
run solve "$V" solve flat.fits -o solved.fits
run photocal "$V" photocal solved.fits -o calibrated.fits --stars-out stars.csv
run cstretch "$V" stretch calibrated.fits -o stretched.fits --preview stretched.png --preview-size 1200
run cstarless "$V" starless calibrated.fits -o starless.fits --preview starless.png --preview-size 700
run drizzle "$V" drizzle lights/ -o drizzled.fits --scale 2 --pixfrac 0.6 --report drizzle-report.txt
run dstretch "$V" stretch drizzled.fits -o drizzled-stretched.fits --preview drizzled.png --preview-size 1200
echo "finished: logs in $W/logs, pictures in $W/mono and $W/colour"
