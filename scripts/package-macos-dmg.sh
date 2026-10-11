#!/usr/bin/env bash
# Wraps dist/rsRewind.app (made by package-macos.sh) in a disk image. Ad-hoc signed, not notarized.
# Usage: scripts/package-macos-dmg.sh <version>
set -euo pipefail
cd "$(dirname "$0")/.."
version=${1:?version}
[ -d dist/rsRewind.app ]
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
cp -R dist/rsRewind.app "$stage/"
ln -s /Applications "$stage/Applications"
dmg="dist/rsrewind-v$version-macos-arm64.dmg"
hdiutil create -volname "rsRewind $version" -srcfolder "$stage" -ov -format UDZO "$dmg"
hdiutil verify "$dmg"
