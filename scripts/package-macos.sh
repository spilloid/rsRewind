#!/usr/bin/env bash
# CI/development artifact. Ad-hoc signing is not Developer ID signing or notarization.
set -euo pipefail
cd "$(dirname "$0")/.."
binary=${1:-target/aarch64-apple-darwin/release/rsrewind}
version=$(sed -n 's/^version = "\([0-9][0-9.]*\)".*/\1/p' Cargo.toml | head -1)
[ -n "$version" ]
[ "$(lipo -archs "$binary")" = arm64 ]
[ -s THIRD-PARTY-NOTICES.html ]
app=dist/rsRewind.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/rsrewind"
chmod 755 "$app/Contents/MacOS/rsrewind"
sed "s/RSREWIND_VERSION/$version/g" installer/macos/Info.plist > "$app/Contents/Info.plist"
cp LICENSE THIRD-PARTY-NOTICES.html "$app/Contents/Resources/"
cp docs/macos.md "$app/Contents/Resources/README.md"
plutil -lint "$app/Contents/Info.plist"
codesign --force --sign - "$app/Contents/MacOS/rsrewind"
codesign --force --sign - "$app"
codesign --verify --strict --verbose=2 "$app"
# Confirm the packaged executable starts without Xcode or toolchain dylibs.
"$app/Contents/MacOS/rsrewind" --version
otool -L "$app/Contents/MacOS/rsrewind" > dist/macos-linked-libraries.txt
if grep -E '/Applications/Xcode|/Library/Developer|/opt/homebrew|/Users/runner' dist/macos-linked-libraries.txt; then
  echo 'non-system runtime dependency in packaged binary' >&2
  exit 1
fi
ditto -c -k --sequesterRsrc --keepParent "$app" "dist/rsRewind-$version-macos-arm64.zip"
shasum -a 256 "dist/rsRewind-$version-macos-arm64.zip" > "dist/rsRewind-$version-macos-arm64.zip.sha256"
