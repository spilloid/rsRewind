#!/usr/bin/env python3
"""Regenerates the app launcher icons from assets/icon/rsrewind-red.png (512x512, the brand mark).

Outputs (committed, so builds need no image tooling):
  assets/icon/rsrewind.ico          Windows: embedded in rsrewind.exe and used by the Start Menu shortcut
  assets/icon/rsrewind.icns         macOS: the app bundle icon
  assets/icon/launcher-{48,128,256}.png   Linux: installed into the user's hicolor icon theme
Needs Pillow (pip install pillow). Run from the repository root.
"""
from PIL import Image

src = Image.open("assets/icon/rsrewind-red.png").convert("RGBA")
assert src.size == (512, 512), src.size

src.save("assets/icon/rsrewind.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
src.save("assets/icon/rsrewind.icns")
for size in (48, 128, 256):
    src.resize((size, size), Image.LANCZOS).save(f"assets/icon/launcher-{size}.png", optimize=True)
