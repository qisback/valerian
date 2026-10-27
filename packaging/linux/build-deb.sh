#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Build the .deb from a staged tree.
#   build-deb.sh VERSION ARCH STAGE OUTDIR      ARCH is x86_64 or aarch64
set -euo pipefail
ver=$1 arch=$2 stage=$3 out=$4
case "$arch" in
  x86_64) deb=amd64 ;;
  aarch64) deb=arm64 ;;
  *) echo "unknown arch $arch" >&2; exit 2 ;;
esac
dver="${ver//-/\~}"   # 0.5.0-rc.1 sorts before 0.5.0 as 0.5.0~rc.1
pkg="$(mktemp -d)/pkg"
cp -a "$stage" "$pkg"
install -d "$pkg/DEBIAN"
size="$(du -sk --apparent-size "$pkg/usr" | cut -f1)"
cat > "$pkg/DEBIAN/control" <<EOF
Package: valerian
Version: ${dver}
Architecture: ${deb}
Maintainer: Laurence Horrocks-Barlow <9962027+qisback@users.noreply.github.com>
Installed-Size: ${size}
Depends: libc6 (>= 2.35)
Recommends: libvulkan1, mesa-vulkan-drivers, libxkbcommon0
Section: science
Priority: optional
Homepage: https://github.com/qisback/valerian
Description: Deep-sky image processing
 Valerian turns the subframes you capture of galaxies, nebulae and clusters
 into a finished image: calibration, registration, stacking, gradient removal,
 colour calibration and stretching. This package has the desktop app
 (valerian-gui) and the command line tool (valerian).
 .
 Valerian is dedicated to the author's son Xaviar, whose middle name is Valerian.
EOF
install -d "$out"
SOURCE_DATE_EPOCH="$(git log -1 --format=%ct)"
export SOURCE_DATE_EPOCH
dpkg-deb --root-owner-group -Zxz --build "$pkg" "$out/valerian_${dver}_${deb}.deb"
ls -l "$out/valerian_${dver}_${deb}.deb"
