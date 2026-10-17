//! A stored picture reads back the way it was captured: upright, unmirrored, channels in order.
//!
//! The macOS 0.1.1 capture bridge stored every frame upside down (fixed in 0.1.2). Each platform's
//! capture code is only checkable on its own desktop, but every frame of every platform goes
//! through the same write path (`rsrewind-storage::media::encode_webp`) and read path
//! (`rsrewind_query::history::decode_webp`, used by the window, exports, MCP and OCR re-reads).
//! This pins that shared path, so a flip or channel swap can only come from a backend.

use rsrewind_core::BgraFrame;
use rsrewind_query::history::decode_webp;
use rsrewind_storage::media::encode_webp;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const WHITE: [u8; 3] = [255, 255, 255];

/// Red top left, green top right, blue bottom left, white bottom right; rows padded by `pad`
/// bytes of garbage, as WGC and KWin deliver them.
fn quadrants(w: u32, h: u32, pad: u32) -> BgraFrame {
    let stride = w * 4 + pad;
    let mut pixels = vec![0x5Au8; (stride * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = match (x < w / 2, y < h / 2) {
                (true, true) => RED,
                (false, true) => GREEN,
                (true, false) => BLUE,
                (false, false) => WHITE,
            };
            let i = (y * stride + x * 4) as usize;
            pixels[i..i + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
    BgraFrame {
        width: w,
        height: h,
        stride,
        pixels,
    }
}

fn rgb_at(frame: &BgraFrame, x: u32, y: u32) -> [u8; 3] {
    let i = (y * frame.stride + x * 4) as usize;
    [frame.pixels[i + 2], frame.pixels[i + 1], frame.pixels[i]]
}

fn close(got: [u8; 3], want: [u8; 3]) -> bool {
    got.iter().zip(want).all(|(g, w)| g.abs_diff(w) <= 24)
}

#[test]
fn a_stored_frame_reads_back_upright_unmirrored_and_in_colour() -> TestResult {
    // Odd sizes on purpose (and one with row padding): no even-dimension shortcut hides a bug.
    for (w, h, pad) in [(64, 40, 0), (97, 61, 12), (1366, 768, 64)] {
        let stored = encode_webp(&quadrants(w, h, pad), 90)?;
        let read = decode_webp(&stored)?;
        assert_eq!((read.width, read.height, read.stride), (w, h, w * 4));
        let (left, right, top, bottom) = (w / 8, w - 1 - w / 8, h / 8, h - 1 - h / 8);
        for (x, y, want, corner) in [
            (left, top, RED, "top left"),
            (right, top, GREEN, "top right"),
            (left, bottom, BLUE, "bottom left"),
            (right, bottom, WHITE, "bottom right"),
        ] {
            let got = rgb_at(&read, x, y);
            assert!(
                close(got, want),
                "{w}x{h}: {corner} is {got:?}, expected about {want:?}"
            );
        }
    }
    Ok(())
}
