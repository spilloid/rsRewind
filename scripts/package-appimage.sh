#!/usr/bin/env bash
# Builds rsrewind-vX.Y.Z-linux-x86_64.AppImage from the release binary. Not code-signed; integrity is
# the SHA-256 published with the release. appimagetool is pinned and checked against a known hash.
# Usage: scripts/package-appimage.sh <version> [binary]
set -euo pipefail
cd "$(dirname "$0")/.."
version=${1:?version}
binary=${2:-target/release/rsrewind}
[ -x "$binary" ]
[ -s THIRD-PARTY-NOTICES.html ]
tool_url=https://github.com/AppImage/appimagetool/releases/download/1.9.0/appimagetool-x86_64.AppImage
tool_sha=46fdd785094c7f6e545b61afcfb0f3d98d8eab243f644b4b17698c01d06083d1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL --retry 3 -o "$work/appimagetool" "$tool_url"
echo "$tool_sha  $work/appimagetool" | sha256sum --check --strict -
chmod +x "$work/appimagetool"
app="$work/rsRewind.AppDir"
mkdir -p "$app/usr/bin" "$app/usr/share/doc/rsrewind"
cp "$binary" "$app/usr/bin/rsrewind"
cp LICENSE README.md THIRD-PARTY-NOTICES.html "$app/usr/share/doc/rsrewind/"
cp assets/icon/rsrewind-red.png "$app/rsrewind.png"
cat > "$app/rsrewind.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=rsRewind
Comment=Local screen history you can search and rewind
Exec=rsrewind ui --foreground
Icon=rsrewind
Categories=Utility;
Terminal=false
DESKTOP
# With arguments (rsrewind start, search ...) the AppImage behaves as the CLI; with none it opens the window.
cat > "$app/AppRun" <<'RUN'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
if [ "$#" -eq 0 ]; then exec "$here/usr/bin/rsrewind" ui --foreground; fi
exec "$here/usr/bin/rsrewind" "$@"
RUN
chmod 755 "$app/AppRun"
out="rsrewind-v$version-linux-x86_64.AppImage"
ARCH=x86_64 "$work/appimagetool" --appimage-extract-and-run "$app" "$out"
chmod 755 "$out"
