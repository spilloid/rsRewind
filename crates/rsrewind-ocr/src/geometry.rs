//! Pure helpers: no COM, no Windows.Media.Ocr activation, nothing that requires an OCR
//! language pack to be installed. These run in plain `cargo test -p rsrewind-ocr`.
//!
//! `windows::Foundation::Rect` is used as the rectangle type even here: it is a plain
//! `#[repr(C)]` struct of four `f32`s with no COM behavior, so pulling it in costs nothing and
//! saves us from inventing a parallel type that `OcrWord::BoundingRect()` would have to be
//! converted into and back out of at every call site.

use rsrewind_core::BgraFrame;
use windows::Foundation::Rect;

/// The union (bounding box) of a set of rectangles, or `None` if `rects` is empty.
pub fn union_rect(rects: &[Rect]) -> Option<Rect> {
    let mut iter = rects.iter();
    let first = iter.next()?;
    let mut min_x = first.X;
    let mut min_y = first.Y;
    let mut max_x = first.X + first.Width;
    let mut max_y = first.Y + first.Height;
    for r in iter {
        min_x = min_x.min(r.X);
        min_y = min_y.min(r.Y);
        max_x = max_x.max(r.X + r.Width);
        max_y = max_y.max(r.Y + r.Height);
    }
    Some(Rect {
        X: min_x,
        Y: min_y,
        Width: max_x - min_x,
        Height: max_y - min_y,
    })
}

/// Scales a rectangle from downscaled-bitmap pixel space back to original-frame pixel space.
pub fn scale_rect(rect: Rect, scale_x: f32, scale_y: f32) -> Rect {
    Rect {
        X: rect.X * scale_x,
        Y: rect.Y * scale_y,
        Width: rect.Width * scale_x,
        Height: rect.Height * scale_y,
    }
}

/// Returns `frame` packed to `stride == width * 4` as a plain pixel buffer, reusing
/// `BgraFrame::into_packed` (core is frozen; we only wrap it here so `recognize` has one place
/// to call and this module has something to unit-test that exercises the packing path we
/// actually use before building a `SoftwareBitmap`).
pub fn packed_pixels(frame: &BgraFrame) -> Vec<u8> {
    frame.clone().into_packed().pixels
}

/// Box/area-average downscale of a tightly packed BGRA8 buffer so that neither dimension
/// exceeds `max_dim`. Returns `(pixels, width, height)` unchanged if already within bounds.
/// `max_dim == 0` is treated as "no limit" (returns the input unchanged) since a real
/// `OcrEngine::MaxImageDimension()` is never zero; this just avoids a divide-by-zero if it
/// somehow were.
pub fn downscale_bgra(pixels: &[u8], width: u32, height: u32, max_dim: u32) -> (Vec<u8>, u32, u32) {
    if max_dim == 0 || (width <= max_dim && height <= max_dim) || width == 0 || height == 0 {
        return (pixels.to_vec(), width, height);
    }

    let scale = (width as f32 / max_dim as f32).max(height as f32 / max_dim as f32);
    let new_width = ((width as f32 / scale).floor() as u32).max(1);
    let new_height = ((height as f32 / scale).floor() as u32).max(1);

    let mut out = vec![0u8; new_width as usize * new_height as usize * 4];
    for oy in 0..new_height {
        let y0 = ((oy as f32) * scale) as u32;
        let y1 = (((oy + 1) as f32) * scale).ceil().max((y0 + 1) as f32) as u32;
        let y1 = y1.min(height);
        for ox in 0..new_width {
            let x0 = ((ox as f32) * scale) as u32;
            let x1 = (((ox + 1) as f32) * scale).ceil().max((x0 + 1) as f32) as u32;
            let x1 = x1.min(width);

            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for y in y0..y1 {
                let row_start = (y as usize) * (width as usize) * 4;
                for x in x0..x1 {
                    let idx = row_start + (x as usize) * 4;
                    sum[0] += u64::from(pixels[idx]);
                    sum[1] += u64::from(pixels[idx + 1]);
                    sum[2] += u64::from(pixels[idx + 2]);
                    sum[3] += u64::from(pixels[idx + 3]);
                    count += 1;
                }
            }

            let out_idx = ((oy as usize) * (new_width as usize) + ox as usize) * 4;
            // `count` is 0 only if `x0..x1`/`y0..y1` were empty, which `.max(x0 + 1)` /
            // `.max(y0 + 1)` above prevent; `checked_div` makes that invariant explicit instead
            // of asserting it with a bare `count > 0` guard.
            out[out_idx] = sum[0].checked_div(count).unwrap_or(0) as u8;
            out[out_idx + 1] = sum[1].checked_div(count).unwrap_or(0) as u8;
            out[out_idx + 2] = sum[2].checked_div(count).unwrap_or(0) as u8;
            out[out_idx + 3] = sum[3].checked_div(count).unwrap_or(0) as u8;
        }
    }

    (out, new_width, new_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect {
            X: x,
            Y: y,
            Width: w,
            Height: h,
        }
    }

    #[test]
    fn union_rect_of_empty_is_none() {
        assert!(union_rect(&[]).is_none());
    }

    #[test]
    fn union_rect_of_one_is_itself() {
        let r = rect(1.0, 2.0, 3.0, 4.0);
        let Some(u) = union_rect(&[r]) else {
            panic!("union_rect of a single rect must be Some");
        };
        assert_eq!((u.X, u.Y, u.Width, u.Height), (1.0, 2.0, 3.0, 4.0));
    }

    #[test]
    fn union_rect_spans_disjoint_rects() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(20.0, 30.0, 5.0, 5.0);
        let Some(u) = union_rect(&[a, b]) else {
            panic!("union_rect of two rects must be Some");
        };
        assert_eq!(u.X, 0.0);
        assert_eq!(u.Y, 0.0);
        assert_eq!(u.Width, 25.0);
        assert_eq!(u.Height, 35.0);
    }

    #[test]
    fn union_rect_handles_negative_origin() {
        let a = rect(-5.0, -5.0, 2.0, 2.0);
        let b = rect(0.0, 0.0, 1.0, 1.0);
        let Some(u) = union_rect(&[a, b]) else {
            panic!("union_rect of two rects must be Some");
        };
        assert_eq!(u.X, -5.0);
        assert_eq!(u.Y, -5.0);
        assert_eq!(u.Width, 6.0);
        assert_eq!(u.Height, 6.0);
    }

    #[test]
    fn scale_rect_scales_all_fields() {
        let r = rect(2.0, 3.0, 4.0, 5.0);
        let s = scale_rect(r, 2.0, 0.5);
        assert_eq!((s.X, s.Y, s.Width, s.Height), (4.0, 1.5, 8.0, 2.5));
    }

    #[test]
    fn packed_pixels_packs_strided_frame() {
        let frame = BgraFrame {
            width: 2,
            height: 2,
            stride: 12,
            pixels: vec![
                1, 1, 1, 1, 2, 2, 2, 2, 0, 0, 0, 0, 3, 3, 3, 3, 4, 4, 4, 4, 0, 0, 0, 0,
            ],
        };
        let packed = packed_pixels(&frame);
        assert_eq!(packed, vec![1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4]);
    }

    #[test]
    fn downscale_noop_when_within_bounds() {
        let pixels = vec![9u8; 4 * 4 * 4];
        let (out, w, h) = downscale_bgra(&pixels, 4, 4, 10);
        assert_eq!((w, h), (4, 4));
        assert_eq!(out, pixels);
    }

    #[test]
    fn downscale_solid_color_stays_solid() {
        // 8x8 solid blue (BGRA) frame, max_dim 4: every output pixel must average to the same
        // color since there is no variation to average away.
        let mut pixels = Vec::with_capacity(8 * 8 * 4);
        for _ in 0..(8 * 8) {
            pixels.extend_from_slice(&[200, 50, 10, 255]);
        }
        let (out, w, h) = downscale_bgra(&pixels, 8, 8, 4);
        assert!(w <= 4 && h <= 4);
        assert!(w >= 1 && h >= 1);
        let (chunks, remainder) = out.as_chunks::<4>();
        assert!(remainder.is_empty());
        for px in chunks {
            assert_eq!(*px, [200, 50, 10, 255]);
        }
    }

    #[test]
    fn downscale_shrinks_both_dimensions_under_max() {
        let pixels = vec![0u8; 100 * 40 * 4];
        let (out, w, h) = downscale_bgra(&pixels, 100, 40, 32);
        assert!(w <= 32);
        assert!(h <= 32);
        assert_eq!(out.len(), (w as usize) * (h as usize) * 4);
    }

    #[test]
    fn downscale_averages_checkerboard() {
        // 2x2 block: black, white, white, black. Averaging all four into one output pixel
        // should land near mid-gray, not exactly either extreme.
        let mut pixels = Vec::new();
        pixels.extend_from_slice(&[0, 0, 0, 255]); // black
        pixels.extend_from_slice(&[255, 255, 255, 255]); // white
        pixels.extend_from_slice(&[255, 255, 255, 255]); // white
        pixels.extend_from_slice(&[0, 0, 0, 255]); // black
        let (out, w, h) = downscale_bgra(&pixels, 2, 2, 1);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out[0], 127); // (0+255+255+0)/4 = 127 (integer division)
    }
}
