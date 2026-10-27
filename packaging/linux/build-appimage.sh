#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
# SPDX-License-Identifier: GPL-3.0-or-later
# See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

# Build the AppImage from a staged tree.
#   build-appimage.sh VERSION ARCH STAGE OUTDIR APPIMAGETOOL [RUNTIME]
# APPIMAGETOOL is a path to appimagetool (itself an AppImage: it is run with
# APPIMAGE_EXTRACT_AND_RUN, so FUSE is not needed). For a foreign ARCH pass the
# matching runtime file; the image is then assembled without running target code.
set -euo pipefail
ver=$1 arch=$2 stage=$3 out=$4 tool=$5 runtime=${6:-}
app="$(mktemp -d)/Valerian.AppDir"
cp -a "$stage" "$app"
cat > "$app/AppRun" <<'EOF'
#!/bin/sh
# valerian-gui by default; "AppRun --cli ARGS" runs the command line tool instead.
here="$(dirname "$(readlink -f "$0")")"
export PATH="$here/usr/bin:$PATH"
if [ "${1:-}" = "--cli" ]; then shift; exec "$here/usr/bin/valerian" "$@"; fi
exec "$here/usr/bin/valerian-gui" "$@"
EOF
chmod 0755 "$app/AppRun"
cp "$app/usr/share/applications/valerian.desktop" "$app/valerian.desktop"
cp "$app/usr/share/icons/hicolor/256x256/apps/valerian.png" "$app/valerian.png"
ln -s valerian.png "$app/.DirIcon"
install -d "$out"
name="Valerian-${ver}-${arch}.AppImage"
args=()
if [ -n "$runtime" ]; then args+=(--runtime-file "$runtime"); fi
ARCH="$arch" APPIMAGE_EXTRACT_AND_RUN=1 "$tool" "${args[@]}" "$app" "$out/$name"
chmod 0755 "$out/$name"
ls -l "$out/$name"
