//! Shrinking decoded frames for the timeline's cards.
//!
//! A 4K screenshot is 33 MB of BGRA; a card needs a few hundred pixels across. Frames are reduced
//! on a background thread with an area-averaging box filter (every source pixel contributes, so
//! thin text strokes fade to grey instead of aliasing away) before they reach the texture cache.

/// A packed BGRA image (4 bytes per pixel, no row padding).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bgra {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Bgra {
    pub fn bytes(&self) -> usize {
        self.pixels.len()
    }

    /// The same pixels as RGBA, for widgets that want that order.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = self.pixels.clone();
        for px in out.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        out
    }
}

/// Scales a strided BGRA image down to fit within `max_w` x `max_h`, keeping its aspect ratio.
/// Never enlarges. `None` if the input is empty or shorter than its dimensions claim.
pub fn downscale(
    pixels: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    max_w: u32,
    max_h: u32,
) -> Option<Bgra> {
    let (w, h, stride) = (width as usize, height as usize, stride as usize);
    if w == 0 || h == 0 || max_w == 0 || max_h == 0 || stride < w * 4 {
        return None;
    }
    if pixels.len() < stride * (h - 1) + w * 4 {
        return None;
    }
    let ratio = (f64::from(max_w) / w as f64)
        .min(f64::from(max_h) / h as f64)
        .min(1.0);
    let out_w = ((w as f64 * ratio).round() as usize).max(1);
    let out_h = ((h as f64 * ratio).round() as usize).max(1);

    let mut out = Vec::with_capacity(out_w * out_h * 4);
    for oy in 0..out_h {
        let y0 = oy * h / out_h;
        let y1 = ((oy + 1) * h / out_h).max(y0 + 1);
        for ox in 0..out_w {
            let x0 = ox * w / out_w;
            let x1 = ((ox + 1) * w / out_w).max(x0 + 1);
            let mut sum = [0u64; 4];
            for y in y0..y1 {
                let row = &pixels[y * stride + x0 * 4..y * stride + x1 * 4];
                for px in row.as_chunks::<4>().0 {
                    for (s, &c) in sum.iter_mut().zip(px) {
                        *s += u64::from(c);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            out.extend(sum.iter().map(|s| ((s + n / 2) / n) as u8));
        }
    }
    Some(Bgra {
        width: out_w as u32,
        height: out_h as u32,
        pixels: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_aspect_and_never_enlarges() {
        let px = vec![9u8; 400 * 100 * 4];
        let small = downscale(&px, 400, 100, 1600, 200, 200).unwrap_or_else(|| Bgra {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        });
        assert_eq!((small.width, small.height), (200, 50));
        assert!(
            small.pixels.iter().all(|&c| c == 9),
            "a flat image stays flat"
        );
        let same = downscale(&px, 400, 100, 1600, 1000, 1000).map(|b| (b.width, b.height));
        assert_eq!(same, Some((400, 100)));
    }

    #[test]
    fn averages_every_source_pixel() {
        // 2x2 black/white checkerboard -> one grey pixel.
        let px = [
            0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0, 0, 0, 255,
        ];
        let one = downscale(&px, 2, 2, 8, 1, 1).map(|b| b.pixels);
        assert_eq!(one, Some(vec![128, 128, 128, 255]));
    }

    #[test]
    fn respects_row_padding_and_refuses_short_buffers() {
        // 1x2 image with 4 bytes of padding per row; padding must be ignored.
        let px = [
            10, 20, 30, 255, 99, 99, 99, 99, 50, 60, 70, 255, 99, 99, 99, 99,
        ];
        let out = downscale(&px, 1, 2, 8, 1, 1).map(|b| b.pixels);
        assert_eq!(out, Some(vec![30, 40, 50, 255]));
        assert_eq!(downscale(&px[..6], 1, 2, 8, 1, 1), None);
        assert_eq!(downscale(&px, 0, 2, 8, 1, 1), None);
        assert_eq!(
            downscale(&px, 4, 2, 8, 1, 1),
            None,
            "stride shorter than a row"
        );
    }

    #[test]
    fn rgba_swaps_red_and_blue() {
        let b = Bgra {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        };
        assert_eq!(b.to_rgba(), vec![3, 2, 1, 4]);
    }
}
