#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Assemble Valerian.app and the disk image. Needs macOS (hdiutil, plutil). Not yet run on a
# Mac: there is no Mac runner (issue #75).
#   build-app.sh VERSION BINDIR METADIR OUTDIR
# BINDIR holds valerian and valerian-gui (a universal or aarch64 build); METADIR holds
# THIRD-PARTY-LICENSES.txt. Writes OUTDIR/Valerian.app and OUTDIR/Valerian-VERSION-macos.dmg.
set -euo pipefail
ver=$1 bindir=$2 meta=$3 out=$4
app="$out/Valerian.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 0755 "$bindir/valerian-gui" "$app/Contents/MacOS/valerian-gui"
# The command line tool rides along inside the bundle so the app is the whole product.
install -m 0755 "$bindir/valerian" "$app/Contents/MacOS/valerian"
strip -x "$app/Contents/MacOS/valerian-gui" "$app/Contents/MacOS/valerian"
install -m 0644 assets/branding/valerian.icns "$app/Contents/Resources/valerian.icns"
sed "s/@VERSION@/${ver}/g" packaging/macos/Info.plist.in > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
res="$app/Contents/Resources"
install -m 0644 LICENSE ADDITIONAL-TERMS.md LOGO-LICENCE.md README.md CHANGELOG.md "$res/"
install -m 0644 "$meta/THIRD-PARTY-LICENSES.txt" "$res/"
# The star catalogue valerian solve reads: Contents/MacOS/data is on its search path.
mkdir -p "$app/Contents/MacOS/data"  # BSD install has no -D
install -m 0644 crates/valerian-core/data/gaia-dr3-g11.vcat "$app/Contents/MacOS/data/gaia-dr3-g11.vcat"
install -m 0644 LICENSES/CC-BY-NC-3.0-IGO.txt "$res/"

# Disk image: the app and a link to /Applications, plus the licences beside them.
dmgsrc="$(mktemp -d)"
cp -R "$app" "$dmgsrc/"
ln -s /Applications "$dmgsrc/Applications"
install -m 0644 LICENSE "$dmgsrc/LICENSE.txt"
install -m 0644 ADDITIONAL-TERMS.md LOGO-LICENCE.md "$dmgsrc/"
dmg="$out/Valerian-${ver}-macos.dmg"
rm -f "$dmg"
hdiutil create -volname "Valerian ${ver}" -srcfolder "$dmgsrc" -fs HFS+ -format UDZO -ov "$dmg"
hdiutil verify "$dmg"
ls -l "$dmg"
