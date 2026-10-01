//! Pure, platform-independent change detection on captured frames.
//!
//! Windows.Graphics.Capture already filters out "nothing was repainted" (it only delivers a frame
//! when DWM composes new content). This module is the second filter: it decides whether a
//! repainted screen is *meaningfully* different from the last stored one, so a blinking caret, a
//! moving mouse cursor or a clock ticking over does not store a new screenshot.
//!
//! A [`Fingerprint`] is a 64×36 grid of area-averaged luma (one cell ≈ 30×30 pixels on a 1080p
//! display) plus a 64-bit dHash. [`ChangeDetector`] counts the cells whose luma moved by more than
//! `epsilon` and calls the change meaningful when that fraction exceeds `threshold`.

use rsrewind_core::BgraFrame;

/// Fingerprint grid width in cells. 64×36 keeps the 16:9 aspect of common displays.
pub const GRID_WIDTH: usize = 64;
/// Fingerprint grid height in cells.
pub const GRID_HEIGHT: usize = 36;
/// Total number of fingerprint cells.
pub const GRID_CELLS: usize = GRID_WIDTH * GRID_HEIGHT;

/// Default per-cell luma tolerance (0–255 scale).
///
/// Why 8 (~3% of the range): each cell averages ~900 pixels on a 1080p display, so per-pixel
/// noise is divided by ~900 before it reaches the cell. WGC frames are lossless, so the noise we
/// do see is sub-pixel anti-aliasing / ClearType re-rasterisation and gradient dithering, which
/// moves a cell average by 0–2 levels. Real content changes are far larger: a line of text
/// appearing in a cell typically moves its average by 20–60 levels. 8 sits well clear of the
/// noise floor while still catching a single changed word in a cell. A 1–2 px caret moves one
/// cell by ~5–10 levels; whether it counts for that one cell is irrelevant, because one cell is
/// 0.04% of the grid, far below any sensible `threshold`.
pub const DEFAULT_EPSILON: u8 = 8;

/// Default fraction of cells that must change. Mirrors `CaptureConfig::change_threshold`'s
/// default; a 32×32 cursor touches at most 9 of 2304 cells (0.4%).
pub const DEFAULT_THRESHOLD: f32 = 0.02;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FingerprintError {
    #[error("frame has zero width or height")]
    Empty,
    #[error("frame buffer is shorter than its width, height and stride require")]
    Malformed,
}

/// A compact, comparable summary of a frame.
#[derive(Clone, PartialEq, Eq)]
pub struct Fingerprint {
    source_width: u32,
    source_height: u32,
    cells: [u8; GRID_CELLS],
    dhash: u64,
}

impl std::fmt::Debug for Fingerprint {
    // The grid is a (very) low-resolution copy of the screen; keep it out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fingerprint")
            .field("source_width", &self.source_width)
            .field("source_height", &self.source_height)
            .field("dhash", &format_args!("{:016x}", self.dhash))
            .finish_non_exhaustive()
    }
}

/// BT.709 luma in integer arithmetic. Weights sum to 256, so white maps to exactly 255.
#[inline]
fn luma(bgra: &[u8; 4]) -> u64 {
    let b = u64::from(bgra[0]);
    let g = u64::from(bgra[1]);
    let r = u64::from(bgra[2]);
    (54 * r + 183 * g + 19 * b) >> 8
}

/// Splits `0..len` into `n` contiguous spans. When `len >= n` the spans partition the range
/// exactly (area averaging); when `len < n` some source pixels are shared by neighbouring cells
/// so that no cell is ever empty.
fn spans(len: usize, n: usize) -> Vec<(usize, usize)> {
    (0..n)
        .map(|i| {
            let start = i * len / n;
            let end = ((i + 1) * len / n).max(start + 1).min(len);
            (start, end)
        })
        .collect()
}

impl Fingerprint {
    /// Builds the fingerprint of `frame`, honouring its stride (padding bytes are ignored).
    pub fn from_frame(frame: &BgraFrame) -> Result<Self, FingerprintError> {
        if frame.width == 0 || frame.height == 0 {
            return Err(FingerprintError::Empty);
        }
        if !frame.is_well_formed() {
            return Err(FingerprintError::Malformed);
        }
        let width = frame.width as usize;
        let height = frame.height as usize;
        let stride = frame.stride as usize;
        let x_spans = spans(width, GRID_WIDTH);
        let y_spans = spans(height, GRID_HEIGHT);

        let mut cells = [0u8; GRID_CELLS];
        for (cy, &(y0, y1)) in y_spans.iter().enumerate() {
            let mut sums = [0u64; GRID_WIDTH];
            for y in y0..y1 {
                let start = y * stride;
                // is_well_formed() guarantees every row's visible bytes are present.
                let row = frame
                    .pixels
                    .get(start..start + width * 4)
                    .ok_or(FingerprintError::Malformed)?;
                for (sum, &(x0, x1)) in sums.iter_mut().zip(&x_spans) {
                    *sum += row[x0 * 4..x1 * 4]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(luma)
                        .sum::<u64>();
                }
            }
            for (cx, &(x0, x1)) in x_spans.iter().enumerate() {
                let area = ((y1 - y0) * (x1 - x0)) as u64;
                // Rounded mean; a mean of values <= 255 is <= 255, so the cast is lossless.
                cells[cy * GRID_WIDTH + cx] = ((sums[cx] + area / 2) / area) as u8;
            }
        }

        Ok(Self {
            source_width: frame.width,
            source_height: frame.height,
            dhash: dhash(&cells),
            cells,
        })
    }

    pub fn source_size(&self) -> (u32, u32) {
        (self.source_width, self.source_height)
    }

    /// 64-bit difference hash: 9×8 area average of the grid, one bit per horizontal neighbour
    /// pair (`left < right`). Suitable for storage (`visual_states.fingerprint`) and cheap
    /// near-duplicate lookups via [`Fingerprint::hamming`].
    pub fn dhash(&self) -> u64 {
        self.dhash
    }

    /// The dHash as the `i64` SQLite stores (bit-for-bit reinterpretation).
    pub fn dhash_i64(&self) -> i64 {
        i64::from_ne_bytes(self.dhash.to_ne_bytes())
    }

    /// Hamming distance between the two dHashes (0 = identical structure, 64 = opposite).
    pub fn hamming(&self, other: &Self) -> u32 {
        (self.dhash ^ other.dhash).count_ones()
    }

    /// Fraction (0.0–1.0) of cells whose luma differs by more than `epsilon`. Frames of different
    /// source sizes are entirely different by definition (1.0).
    pub fn changed_fraction(&self, other: &Self, epsilon: u8) -> f32 {
        if self.source_size() != other.source_size() {
            return 1.0;
        }
        let changed = self
            .cells
            .iter()
            .zip(other.cells.iter())
            .filter(|(a, b)| a.abs_diff(**b) > epsilon)
            .count();
        changed as f32 / GRID_CELLS as f32
    }
}

fn dhash(cells: &[u8; GRID_CELLS]) -> u64 {
    let x_spans = spans(GRID_WIDTH, 9);
    let y_spans = spans(GRID_HEIGHT, 8);
    let mut hash = 0u64;
    for (row, &(y0, y1)) in y_spans.iter().enumerate() {
        let mut means = [0u32; 9];
        for (mean, &(x0, x1)) in means.iter_mut().zip(&x_spans) {
            let mut sum = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += u32::from(cells[y * GRID_WIDTH + x]);
                }
            }
            *mean = sum / ((y1 - y0) * (x1 - x0)) as u32;
        }
        for col in 0..8 {
            if means[col] < means[col + 1] {
                hash |= 1 << (row * 8 + col);
            }
        }
    }
    hash
}

/// The replaceable decision "is `next` worth storing given `prev`?".
pub trait ChangeDetection {
    fn is_meaningful(&self, prev: &Fingerprint, next: &Fingerprint) -> bool;
}

/// Default detector: meaningful when more than `threshold` of the cells moved by more than
/// `epsilon` luma levels, or when the frame size changed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChangeDetector {
    /// Fraction of cells (0.0–1.0). Strictly greater-than: `0.0` means "any changed cell".
    pub threshold: f32,
    pub epsilon: u8,
}

impl ChangeDetector {
    pub fn new(threshold: f32) -> Self {
        Self {
            threshold,
            epsilon: DEFAULT_EPSILON,
        }
    }
}

impl Default for ChangeDetector {
    fn default() -> Self {
        Self::new(DEFAULT_THRESHOLD)
    }
}

impl ChangeDetection for ChangeDetector {
    fn is_meaningful(&self, prev: &Fingerprint, next: &Fingerprint) -> bool {
        if prev.source_size() != next.source_size() {
            return true;
        }
        prev.changed_fraction(next, self.epsilon) > self.threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn solid(width: u32, height: u32, bgr: [u8; 3]) -> BgraFrame {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width as usize * height as usize {
            pixels.extend_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
        }
        BgraFrame {
            width,
            height,
            stride: width * 4,
            pixels,
        }
    }

    fn set_px(frame: &mut BgraFrame, x: u32, y: u32, bgr: [u8; 3]) {
        let i = (y * frame.stride + x * 4) as usize;
        frame.pixels[i..i + 3].copy_from_slice(&bgr);
    }

    fn fill_rect(frame: &mut BgraFrame, x: u32, y: u32, w: u32, h: u32, bgr: [u8; 3]) {
        for yy in y..(y + h).min(frame.height) {
            for xx in x..(x + w).min(frame.width) {
                set_px(frame, xx, yy, bgr);
            }
        }
    }

    /// Deterministic LCG so tests need no rand crate.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 33) as u32
        }
    }

    /// A white page of "text": dark word-shaped rectangles on 20 px lines, starting at `offset`
    /// (negative = scrolled up). Word layout differs per line, like real prose.
    fn text_page(width: u32, height: u32, offset: i32) -> BgraFrame {
        let mut frame = solid(width, height, [255, 255, 255]);
        let mut rng = Lcg(42);
        for line in 0..200i32 {
            let top = offset + line * 20;
            let mut x = 40u32;
            // Generate the whole line even if off-screen so layout is independent of offset.
            let mut words = Vec::new();
            while x < width - 40 {
                let w = 15 + rng.next() % 60;
                words.push((x, w));
                x += w + 8;
            }
            if top < 0 || top as u32 + 10 > height {
                continue;
            }
            for (wx, ww) in words {
                fill_rect(&mut frame, wx, top as u32, ww, 10, [30, 30, 30]);
            }
        }
        frame
    }

    fn fp(frame: &BgraFrame) -> Result<Fingerprint, FingerprintError> {
        Fingerprint::from_frame(frame)
    }

    #[test]
    fn identical_frames_are_not_meaningful() -> TestResult {
        let a = text_page(1920, 1080, 0);
        let b = a.clone();
        let detector = ChangeDetector::default();
        assert!(!detector.is_meaningful(&fp(&a)?, &fp(&b)?));
        assert_eq!(fp(&a)?.hamming(&fp(&b)?), 0);
        Ok(())
    }

    #[test]
    fn tiny_noise_is_not_meaningful() -> TestResult {
        let a = text_page(1920, 1080, 0);
        let mut b = a.clone();
        let mut rng = Lcg(7);
        // 5000 scattered pixels nudged by up to ±4 levels, plus one pixel flipped to full
        // contrast: anti-aliasing-scale noise.
        for _ in 0..5000 {
            let x = rng.next() % 1920;
            let y = rng.next() % 1080;
            let i = (y * b.stride + x * 4) as usize;
            for c in 0..3 {
                let delta = (rng.next() % 9) as i16 - 4;
                b.pixels[i + c] = (i16::from(b.pixels[i + c]) + delta).clamp(0, 255) as u8;
            }
        }
        set_px(&mut b, 500, 500, [0, 0, 0]);
        let (fa, fb) = (fp(&a)?, fp(&b)?);
        assert_eq!(fa.changed_fraction(&fb, DEFAULT_EPSILON), 0.0);
        assert!(!ChangeDetector::default().is_meaningful(&fa, &fb));
        Ok(())
    }

    #[test]
    fn cursor_sized_blob_is_below_default_threshold() -> TestResult {
        let base = solid(1920, 1080, [128, 128, 128]);
        // Positions chosen to straddle cell boundaries (cells are 30×30 at 1080p), which is the
        // worst case: a 32×32 blob then touches 3×3 cells.
        for (x, y) in [(0, 0), (29, 29), (1000, 500), (1888, 1048)] {
            let mut moved = base.clone();
            fill_rect(&mut moved, x, y, 32, 32, [0, 0, 0]);
            let (fa, fb) = (fp(&base)?, fp(&moved)?);
            let fraction = fa.changed_fraction(&fb, DEFAULT_EPSILON);
            assert!(
                fraction > 0.0,
                "blob at {x},{y} should be visible to the grid"
            );
            assert!(fraction <= 9.0 / GRID_CELLS as f32, "fraction {fraction}");
            assert!(!ChangeDetector::default().is_meaningful(&fa, &fb));
        }
        Ok(())
    }

    #[test]
    fn scrolled_text_is_meaningful() -> TestResult {
        let a = text_page(1920, 1080, 0);
        let b = text_page(1920, 1080, -37);
        let (fa, fb) = (fp(&a)?, fp(&b)?);
        let fraction = fa.changed_fraction(&fb, DEFAULT_EPSILON);
        assert!(fraction > 0.25, "scroll changed only {fraction}");
        assert!(ChangeDetector::default().is_meaningful(&fa, &fb));
        assert!(fa.hamming(&fb) > 0);
        Ok(())
    }

    #[test]
    fn different_dimensions_are_meaningful() -> TestResult {
        let a = solid(1920, 1080, [0, 0, 0]);
        let b = solid(2560, 1440, [0, 0, 0]);
        let (fa, fb) = (fp(&a)?, fp(&b)?);
        assert_eq!(fa.changed_fraction(&fb, DEFAULT_EPSILON), 1.0);
        // Even a detector that would never otherwise fire.
        let never = ChangeDetector {
            threshold: 1.0,
            epsilon: 255,
        };
        assert!(never.is_meaningful(&fa, &fb));
        Ok(())
    }

    #[test]
    fn all_black_vs_all_black_is_not_meaningful() -> TestResult {
        let a = solid(1920, 1080, [0, 0, 0]);
        let b = solid(1920, 1080, [0, 0, 0]);
        let detector = ChangeDetector {
            threshold: 0.0,
            epsilon: 0,
        };
        assert!(!detector.is_meaningful(&fp(&a)?, &fp(&b)?));
        assert_eq!(fp(&a)?.dhash(), 0);
        Ok(())
    }

    #[test]
    fn black_to_white_is_meaningful() -> TestResult {
        let a = solid(640, 360, [0, 0, 0]);
        let b = solid(640, 360, [255, 255, 255]);
        let (fa, fb) = (fp(&a)?, fp(&b)?);
        assert_eq!(fa.changed_fraction(&fb, DEFAULT_EPSILON), 1.0);
        assert!(ChangeDetector::default().is_meaningful(&fa, &fb));
        Ok(())
    }

    #[test]
    fn stride_padding_is_ignored() -> TestResult {
        let packed = text_page(1366, 768, 0);
        let padded_stride = packed.stride + 64;
        let mut pixels = Vec::new();
        for y in 0..packed.height as usize {
            let start = y * packed.stride as usize;
            pixels.extend_from_slice(&packed.pixels[start..start + packed.stride as usize]);
            // Garbage in the padding must not leak into the fingerprint.
            pixels.extend(std::iter::repeat_n(0xAB, 64));
        }
        let padded = BgraFrame {
            width: packed.width,
            height: packed.height,
            stride: padded_stride,
            pixels,
        };
        assert!(padded.is_well_formed());
        assert_eq!(fp(&packed)?, fp(&padded)?);
        Ok(())
    }

    #[test]
    fn last_row_without_trailing_padding_is_accepted() -> TestResult {
        // Mapped D3D textures often end exactly after the last row's visible bytes.
        let frame = BgraFrame {
            width: 2,
            height: 2,
            stride: 16,
            pixels: vec![255; 16 + 8],
        };
        assert!(fp(&frame).is_ok());
        Ok(())
    }

    #[test]
    fn empty_and_malformed_frames_are_rejected() {
        let empty = BgraFrame {
            width: 0,
            height: 10,
            stride: 0,
            pixels: Vec::new(),
        };
        assert_eq!(fp(&empty), Err(FingerprintError::Empty));
        let short = BgraFrame {
            width: 100,
            height: 100,
            stride: 400,
            pixels: vec![0; 100],
        };
        assert_eq!(fp(&short), Err(FingerprintError::Malformed));
        let bad_stride = BgraFrame {
            width: 100,
            height: 1,
            stride: 10,
            pixels: vec![0; 400],
        };
        assert_eq!(fp(&bad_stride), Err(FingerprintError::Malformed));
    }

    #[test]
    fn frames_smaller_than_the_grid_work() -> TestResult {
        let mut a = solid(3, 2, [0, 0, 0]);
        let b = a.clone();
        set_px(&mut a, 2, 1, [255, 255, 255]);
        let (fa, fb) = (fp(&a)?, fp(&b)?);
        assert!(fa.changed_fraction(&fb, DEFAULT_EPSILON) > 0.0);
        assert!(fa.changed_fraction(&fa.clone(), 0) == 0.0);
        Ok(())
    }

    #[test]
    fn luma_extremes() {
        assert_eq!(luma(&[255, 255, 255, 255]), 255);
        assert_eq!(luma(&[0, 0, 0, 255]), 0);
        // Green dominates perceived brightness.
        assert!(luma(&[0, 255, 0, 0]) > luma(&[0, 0, 255, 0]));
        assert!(luma(&[0, 0, 255, 0]) > luma(&[255, 0, 0, 0]));
    }

    #[test]
    fn spans_cover_without_gaps() {
        for (len, n) in [
            (1920, 64),
            (1080, 36),
            (64, 64),
            (10, 64),
            (1, 36),
            (65, 64),
        ] {
            let s = spans(len, n);
            assert_eq!(s.len(), n);
            assert!(s.iter().all(|&(a, b)| a < b && b <= len));
            if len >= n {
                assert_eq!(s[0].0, 0);
                assert_eq!(s[n - 1].1, len);
                assert!(s.windows(2).all(|w| w[0].1 == w[1].0));
            }
        }
    }

    #[test]
    fn dhash_reflects_horizontal_gradient() -> TestResult {
        let mut frame = solid(900, 800, [0, 0, 0]);
        for x in 0..900 {
            let v = (x * 255 / 899) as u8;
            fill_rect(&mut frame, x, 0, 1, 800, [v, v, v]);
        }
        // Brightness increases left to right everywhere: every bit set.
        assert_eq!(fp(&frame)?.dhash(), u64::MAX);
        assert_eq!(fp(&frame)?.dhash_i64(), -1);
        Ok(())
    }
}
