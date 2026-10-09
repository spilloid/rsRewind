# Artwork

- `icon/rsrewind-red.png`, `icon/rsrewind-green.png`: the rsRewind icon, 512 px, transparent background.
  Supplied by the maintainer on 2026-10-09 (1254 px originals, cropped to their content and scaled).
  Red means recording, green means not recording.
- `tray/{red,green}-{22,32,48,64,128}.png`: the same artwork pre-scaled for the notification area
  (Lanczos). `rsrewind-tray` embeds these and draws its state badges (pause bars, stop square, amber
  warning dot) over the green or red one at run time.

To regenerate the tray sizes, scale the 512 px icons down with any Lanczos resizer; keep RGBA.
