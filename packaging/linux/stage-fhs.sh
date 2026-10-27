#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Lay out the files of a Linux package in the filesystem hierarchy, under DEST.
#   stage-fhs.sh VERSION BINDIR METADIR DEST [STRIP]
# The same tree feeds the .deb, the .rpm and the AppImage. BINDIR holds the built
# valerian and valerian-gui; METADIR holds THIRD-PARTY-LICENSES.txt. Run from the
# repository root.
set -euo pipefail
ver=$1 bindir=$2 meta=$3 dest=$4 strip=${5:-strip}
date="$(git log -1 --format=%cs)"
rm -rf "$dest"
install -d "$dest/usr/bin"
for b in valerian valerian-gui; do
  install -m 0755 "$bindir/$b" "$dest/usr/bin/$b"
  "$strip" "$dest/usr/bin/$b"
done
install -D -m 0644 packaging/linux/valerian.desktop "$dest/usr/share/applications/valerian.desktop"
for s in 16 24 32 48 64 128 256 512; do
  install -D -m 0644 "assets/branding/icons/valerian-$s.png" "$dest/usr/share/icons/hicolor/${s}x${s}/apps/valerian.png"
done
install -d "$dest/usr/share/metainfo"
sed -e "s/@VERSION@/${ver}/" -e "s/@DATE@/${date}/" packaging/linux/casa.discworld.valerian.metainfo.xml.in \
  > "$dest/usr/share/metainfo/casa.discworld.valerian.metainfo.xml"
doc="$dest/usr/share/doc/valerian"
install -d "$doc"
# The licence texts travel with the program. The logo is all rights reserved:
# LOGO-LICENCE.md stays with it.
install -m 0644 LICENSE ADDITIONAL-TERMS.md LOGO-LICENCE.md README.md "$doc/"
install -m 0644 "$meta/THIRD-PARTY-LICENSES.txt" "$doc/"
# The star catalogue valerian solve reads: usr/bin/../share/valerian is on its search path.
install -D -m 0644 crates/valerian-core/data/gaia-dr3-g11.vcat "$dest/usr/share/valerian/gaia-dr3-g11.vcat"
install -m 0644 LICENSES/CC-BY-NC-3.0-IGO.txt "$doc/"
gzip -9n -c CHANGELOG.md > "$doc/changelog.gz"
chmod 0644 "$doc/changelog.gz"
install -m 0644 packaging/linux/copyright "$doc/copyright"
find "$dest" -type d -exec chmod 0755 {} +
echo "staged $(find "$dest" -type f | wc -l) files in $dest"
