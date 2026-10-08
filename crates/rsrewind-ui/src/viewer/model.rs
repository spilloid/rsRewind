//! The image viewer as plain math: fitting a screenshot to the window, zooming toward the
//! pointer, panning with the edges clamped, the 100 % / fit toggle, mapping OCR boxes onto the
//! screen, and deciding which recognized-text blocks match the current search.
//!
//! Coordinates: the viewport is in logical pixels (origin top-left of the viewer); the image is in
//! its own pixels. A scale is logical pixels per image pixel, so "100 %" (one image pixel per
//! physical screen pixel) is `1 / window_scale_factor`.
//!
//! Nothing here knows about iced or the GPU; the widget feeds pointer input in and draws the
//! [`Placement`] that comes out.

use crate::timeline::model::Rect;
use rsrewind_core::OcrBlock;

/// The furthest the viewer zooms in, relative to 100 %.
pub const MAX_OVER_ACTUAL: f32 = 8.0;
/// Two scales this close are the same zoom (float noise from repeated zoom steps).
const SAME: f32 = 1e-3;

/// How the image is shown, independent of the window size, so a resize keeps it sensible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// The whole image, as large as fits, centred.
    Fit,
    /// `scale` logical px per image px, with image point `center` at the middle of the viewport.
    Scale { scale: f32, center: (f32, f32) },
}

/// Where the image lands on screen this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub rect: Rect,
    pub scale: f32,
}

impl Placement {
    /// An image-pixel rectangle on screen.
    pub fn to_screen(self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect {
            x: self.rect.x + x * self.scale,
            y: self.rect.y + y * self.scale,
            w: w * self.scale,
            h: h * self.scale,
        }
    }

    /// The image pixel under a viewport point (may lie outside the image).
    pub fn to_image(self, x: f32, y: f32) -> (f32, f32) {
        let s = self.scale.max(f32::MIN_POSITIVE);
        ((x - self.rect.x) / s, (y - self.rect.y) / s)
    }
}

fn valid(size: (f32, f32)) -> bool {
    size.0.is_finite() && size.1.is_finite() && size.0 > 0.0 && size.1 > 0.0
}

/// The scale at which the whole image just fits the viewport (aspect ratio kept).
pub fn fit_scale(image: (f32, f32), viewport: (f32, f32)) -> f32 {
    if !valid(image) || !valid(viewport) {
        return 1.0;
    }
    (viewport.0 / image.0).min(viewport.1 / image.1)
}

/// Smallest and largest scale allowed: never smaller than fit (or than 100 % for an image smaller
/// than the window), never past [`MAX_OVER_ACTUAL`] times 100 %.
fn limits(image: (f32, f32), viewport: (f32, f32), actual: f32) -> (f32, f32) {
    let fit = fit_scale(image, viewport);
    let lo = fit.min(actual);
    (lo, (actual * MAX_OVER_ACTUAL).max(lo))
}

/// Keeps the visible part of the image on screen: an axis smaller than the viewport is centred,
/// a larger one may not be dragged past its edges.
fn clamp_center(center: f32, image: f32, viewport: f32, scale: f32) -> f32 {
    let half = viewport / 2.0 / scale;
    if image * scale <= viewport || !center.is_finite() {
        image / 2.0
    } else {
        center.clamp(half, image - half)
    }
}

/// Lays the image out for this viewport.
pub fn place(zoom: Zoom, image: (f32, f32), viewport: (f32, f32)) -> Placement {
    let (scale, center) = match zoom {
        Zoom::Fit => (fit_scale(image, viewport), (image.0 / 2.0, image.1 / 2.0)),
        Zoom::Scale { scale, center } => {
            let scale = if scale.is_finite() && scale > 0.0 {
                scale
            } else {
                fit_scale(image, viewport)
            };
            (
                scale,
                (
                    clamp_center(center.0, image.0, viewport.0, scale),
                    clamp_center(center.1, image.1, viewport.1, scale),
                ),
            )
        }
    };
    Placement {
        rect: Rect {
            x: viewport.0 / 2.0 - center.0 * scale,
            y: viewport.1 / 2.0 - center.1 * scale,
            w: image.0 * scale,
            h: image.1 * scale,
        },
        scale,
    }
}

/// Clamps a zoom into the allowed range; a scale at fit becomes [`Zoom::Fit`].
fn normalize(zoom: Zoom, image: (f32, f32), viewport: (f32, f32), actual: f32) -> Zoom {
    let Zoom::Scale { scale, center } = zoom else {
        return Zoom::Fit;
    };
    let (lo, hi) = limits(image, viewport, actual);
    let scale = scale.clamp(lo, hi);
    let fit = fit_scale(image, viewport);
    if (scale - fit).abs() <= fit * SAME {
        return Zoom::Fit;
    }
    Zoom::Scale {
        scale,
        center: (
            clamp_center(center.0, image.0, viewport.0, scale),
            clamp_center(center.1, image.1, viewport.1, scale),
        ),
    }
}

/// Zooms by `factor`, keeping the image point under `at` (viewport coordinates) where it is.
pub fn zoom_at(
    zoom: Zoom,
    factor: f32,
    at: (f32, f32),
    image: (f32, f32),
    viewport: (f32, f32),
    actual: f32,
) -> Zoom {
    if !(factor.is_finite() && factor > 0.0) || !valid(image) || !valid(viewport) {
        return zoom;
    }
    let before = place(zoom, image, viewport);
    let (lo, hi) = limits(image, viewport, actual);
    let scale = (before.scale * factor).clamp(lo, hi);
    let (ix, iy) = before.to_image(at.0, at.1);
    // The new origin puts (ix, iy) back under the pointer; the centre follows from it.
    let (ox, oy) = (at.0 - ix * scale, at.1 - iy * scale);
    let center = (
        (viewport.0 / 2.0 - ox) / scale,
        (viewport.1 / 2.0 - oy) / scale,
    );
    normalize(Zoom::Scale { scale, center }, image, viewport, actual)
}

/// Moves the image by a pointer drag of `(dx, dy)` logical pixels.
pub fn pan(
    zoom: Zoom,
    dx: f32,
    dy: f32,
    image: (f32, f32),
    viewport: (f32, f32),
    actual: f32,
) -> Zoom {
    let Zoom::Scale { scale, center } = zoom else {
        return Zoom::Fit;
    };
    if !(dx.is_finite() && dy.is_finite()) {
        return zoom;
    }
    let center = (center.0 - dx / scale, center.1 - dy / scale);
    normalize(Zoom::Scale { scale, center }, image, viewport, actual)
}

/// Shows the image at 100 %, keeping the image point under `at` in place.
pub fn actual_size(
    zoom: Zoom,
    at: (f32, f32),
    image: (f32, f32),
    viewport: (f32, f32),
    actual: f32,
) -> Zoom {
    let now = place(zoom, image, viewport).scale;
    zoom_at(
        zoom,
        actual / now.max(f32::MIN_POSITIVE),
        at,
        image,
        viewport,
        actual,
    )
}

/// Double-click: from fit (or anything other than 100 %) to 100 % at the pointer; from 100 % back
/// to fit.
pub fn toggle(
    zoom: Zoom,
    at: (f32, f32),
    image: (f32, f32),
    viewport: (f32, f32),
    actual: f32,
) -> Zoom {
    let now = place(zoom, image, viewport).scale;
    let at_actual = (now - actual).abs() <= actual * SAME;
    if at_actual && zoom != Zoom::Fit {
        Zoom::Fit
    } else {
        actual_size(zoom, at, image, viewport, actual)
    }
}

/// The zoom as a percentage of 100 % (for the label).
pub fn percent(placement: &Placement, actual: f32) -> u32 {
    if actual <= 0.0 || !actual.is_finite() {
        return 100;
    }
    (placement.scale / actual * 100.0)
        .round()
        .clamp(1.0, 100_000.0) as u32
}

/// An OCR block's rectangle in the pixels of the picture being shown. Blocks are stored in the
/// original capture's pixels (`captured`); the shown picture may have been reduced.
pub fn block_rect(block: &OcrBlock, captured: (f32, f32), shown: (f32, f32)) -> Option<[f32; 4]> {
    if !valid(captured) || !valid(shown) {
        return None;
    }
    let (sx, sy) = (shown.0 / captured.0, shown.1 / captured.1);
    let r = [
        block.x * sx,
        block.y * sy,
        block.width * sx,
        block.height * sy,
    ];
    (r.iter().all(|v| v.is_finite()) && r[2] > 0.0 && r[3] > 0.0).then_some(r)
}

/// One searched phrase: its words (lowercase), and whether the last one is a prefix (`term*`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Phrase {
    words: Vec<String>,
    prefix: bool,
}

/// Letter/digit runs, lowercased: how SQLite's unicode61 tokenizer splits text.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// The phrases of a search, read the way `rsrewind_query::parse_query` reads them: bare words,
/// `"quoted phrases"`, a trailing `*` for a prefix, everything else literal.
fn phrases(query: &str) -> Vec<Phrase> {
    let chars: Vec<char> = query.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() || chars[i].is_control() {
            i += 1;
            continue;
        }
        let (content, mut next) = if chars[i] == '"' {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && chars[end] != '"' {
                end += 1;
            }
            let text: String = chars[start..end].iter().collect();
            (text, if end < chars.len() { end + 1 } else { end })
        } else {
            let start = i;
            let mut end = i;
            while end < chars.len() && !chars[end].is_whitespace() && chars[end] != '*' {
                end += 1;
            }
            let text: String = chars[start..end].iter().collect();
            (text, end)
        };
        let mut prefix = false;
        while chars.get(next) == Some(&'*') {
            prefix = true;
            next += 1;
        }
        // A bare word with `*` in the middle continues after it as another phrase.
        let words = words(&content);
        if !words.is_empty() {
            out.push(Phrase { words, prefix });
        }
        i = next.max(i + 1);
    }
    out
}

fn contains_phrase(haystack: &[String], phrase: &Phrase) -> bool {
    let n = phrase.words.len();
    if n == 0 || haystack.len() < n {
        return false;
    }
    haystack.windows(n).any(|window| {
        window
            .iter()
            .zip(&phrase.words)
            .enumerate()
            .all(|(k, (have, want))| {
                if phrase.prefix && k == n - 1 {
                    have.starts_with(want.as_str())
                } else {
                    have == want
                }
            })
    })
}

/// Indices of the blocks that show any phrase of `query` (the gold outlines). Search matches a
/// whole screen when every phrase appears somewhere on it; each phrase is outlined where it is.
pub fn matching_blocks(blocks: &[OcrBlock], query: &str) -> Vec<usize> {
    let wanted = phrases(query);
    if wanted.is_empty() {
        return Vec::new();
    }
    blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| {
            let have = words(&block.text);
            wanted.iter().any(|p| contains_phrase(&have, p))
        })
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMG: (f32, f32) = (1920.0, 1080.0);
    const VP: (f32, f32) = (1000.0, 800.0);
    const ACTUAL: f32 = 0.8; // 125 % scaling: one image pixel = 0.8 logical px

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-2
    }

    #[test]
    fn fit_keeps_the_aspect_ratio_and_centres() {
        let p = place(Zoom::Fit, IMG, VP);
        assert!(close(p.scale, 1000.0 / 1920.0));
        assert!(close(p.rect.w / p.rect.h, 1920.0 / 1080.0));
        assert!(close(p.rect.x, 0.0) && close(p.rect.w, 1000.0));
        assert!(
            close(p.rect.y, (800.0 - p.rect.h) / 2.0),
            "letterboxed vertically"
        );
        // A tall viewport letterboxes the other way.
        let tall = place(Zoom::Fit, IMG, (500.0, 900.0));
        assert!(close(tall.rect.w, 500.0) && tall.rect.y > 0.0);
        // Degenerate sizes do not produce NaN.
        let bad = place(Zoom::Fit, (0.0, 0.0), VP);
        assert!(bad.rect.x.is_finite() && bad.scale.is_finite());
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let at = (700.0, 300.0);
        let before = place(Zoom::Fit, IMG, VP);
        let target = before.to_image(at.0, at.1);
        let zoomed = zoom_at(Zoom::Fit, 2.0, at, IMG, VP, ACTUAL);
        let after = place(zoomed, IMG, VP);
        assert!(close(after.scale, before.scale * 2.0));
        let still = after.to_image(at.0, at.1);
        assert!(
            close(still.0, target.0) && close(still.1, target.1),
            "{still:?} vs {target:?}"
        );
        // Zooming back out by the same factor returns to fit exactly.
        assert_eq!(zoom_at(zoomed, 0.5, at, IMG, VP, ACTUAL), Zoom::Fit);
    }

    #[test]
    fn zoom_is_limited_to_fit_and_eight_times_actual() {
        assert_eq!(
            zoom_at(Zoom::Fit, 0.1, (0.0, 0.0), IMG, VP, ACTUAL),
            Zoom::Fit
        );
        let mut z = Zoom::Fit;
        for _ in 0..50 {
            z = zoom_at(z, 1.5, (500.0, 400.0), IMG, VP, ACTUAL);
        }
        assert!(close(place(z, IMG, VP).scale, ACTUAL * MAX_OVER_ACTUAL));
        assert_eq!(zoom_at(z, f32::NAN, (0.0, 0.0), IMG, VP, ACTUAL), z);
        assert_eq!(zoom_at(z, -2.0, (0.0, 0.0), IMG, VP, ACTUAL), z);
    }

    #[test]
    fn panning_stops_at_the_edges_and_centres_small_axes() {
        let z = actual_size(Zoom::Fit, (500.0, 400.0), IMG, VP, ACTUAL);
        let p = place(z, IMG, VP);
        assert!(close(p.scale, ACTUAL));
        // 1920 x 0.8 = 1536 wide (> 1000): pannable; 1080 x 0.8 = 864 tall (> 800): pannable.
        let far = pan(z, 1e6, 1e6, IMG, VP, ACTUAL);
        let p = place(far, IMG, VP);
        assert!(
            close(p.rect.x, 0.0) && close(p.rect.y, 0.0),
            "left/top edge reached: {p:?}"
        );
        let other = place(pan(z, -1e6, -1e6, IMG, VP, ACTUAL), IMG, VP);
        assert!(
            close(other.rect.x + other.rect.w, VP.0) && close(other.rect.y + other.rect.h, VP.1)
        );
        // At fit nothing pans.
        assert_eq!(pan(Zoom::Fit, 50.0, 50.0, IMG, VP, ACTUAL), Zoom::Fit);
        // An axis narrower than the viewport stays centred while the other pans.
        let wide_vp = (2000.0, 600.0);
        let z = actual_size(Zoom::Fit, (1000.0, 300.0), IMG, wide_vp, ACTUAL);
        let p = place(pan(z, 300.0, 0.0, IMG, wide_vp, ACTUAL), IMG, wide_vp);
        assert!(close(p.rect.x, (2000.0 - 1536.0) / 2.0), "{p:?}");
    }

    #[test]
    fn double_click_toggles_between_actual_size_and_fit() {
        let at = (250.0, 600.0);
        let one = toggle(Zoom::Fit, at, IMG, VP, ACTUAL);
        let p = place(one, IMG, VP);
        assert!(close(p.scale, ACTUAL));
        assert_eq!(toggle(one, at, IMG, VP, ACTUAL), Zoom::Fit);
        // From some other zoom, toggle goes to 100 % first.
        let two = zoom_at(Zoom::Fit, 3.0, at, IMG, VP, ACTUAL);
        assert!(close(
            place(toggle(two, at, IMG, VP, ACTUAL), IMG, VP).scale,
            ACTUAL
        ));
        assert_eq!(percent(&place(one, IMG, VP), ACTUAL), 100);
        // A picture smaller than the window: fit is bigger than 100 %, and 100 % is reachable.
        let small = (400.0, 300.0);
        let p = place(toggle(Zoom::Fit, at, small, VP, ACTUAL), small, VP);
        assert!(close(p.scale, ACTUAL) && p.rect.x > 0.0, "{p:?}");
    }

    #[test]
    fn a_resize_keeps_the_zoom_meaningful() {
        let z = zoom_at(Zoom::Fit, 4.0, (100.0, 100.0), IMG, VP, ACTUAL);
        let small = place(z, IMG, (300.0, 200.0));
        // 4 x fit = 2.08 px per image px: 3999 x 2250 on screen, inside 5000 x 3000.
        let big = place(z, IMG, (5000.0, 3000.0));
        assert!(small.scale.is_finite() && big.scale.is_finite());
        // In a viewport bigger than the zoomed image, it is centred, never off to one side.
        assert!(close(big.rect.x, (5000.0 - big.rect.w) / 2.0), "{big:?}");
        assert!(close(big.rect.y, (3000.0 - big.rect.h) / 2.0), "{big:?}");
    }

    fn block(text: &str, x: f32) -> OcrBlock {
        OcrBlock {
            text: text.into(),
            x,
            y: 10.0,
            width: 100.0,
            height: 20.0,
            confidence: None,
            line_index: 0,
        }
    }

    #[test]
    fn ocr_boxes_follow_the_shown_picture() {
        let b = block("x", 960.0);
        let r = block_rect(&b, (3840.0, 2160.0), (1920.0, 1080.0));
        assert_eq!(r, Some([480.0, 5.0, 50.0, 10.0]));
        assert_eq!(block_rect(&b, (0.0, 2160.0), (1920.0, 1080.0)), None);
        let p = place(Zoom::Fit, (1920.0, 1080.0), (960.0, 540.0));
        let on_screen = p.to_screen(480.0, 5.0, 50.0, 10.0);
        assert!(close(on_screen.x, 240.0) && close(on_screen.w, 25.0));
    }

    #[test]
    fn matching_blocks_reads_the_query_like_search_does() {
        let blocks = [
            block("Konica bizhub C360 toner low", 0.0),
            block("order 48 cartridges", 0.0),
            block("quarterly budget forecast", 0.0),
            block("KONICA-printer", 0.0),
            block("budgetary", 0.0),
        ];
        assert_eq!(
            matching_blocks(&blocks, "konica"),
            vec![0, 3],
            "case and punctuation"
        );
        assert_eq!(
            matching_blocks(&blocks, "budget"),
            vec![2],
            "whole words only"
        );
        assert_eq!(matching_blocks(&blocks, "budget*"), vec![2, 4], "prefix");
        assert_eq!(matching_blocks(&blocks, "\"toner low\""), vec![0], "phrase");
        assert!(
            matching_blocks(&blocks, "\"low toner\"").is_empty(),
            "phrase order matters"
        );
        assert_eq!(
            matching_blocks(&blocks, "order konica"),
            vec![0, 1, 3],
            "each phrase outlined"
        );
        assert!(matching_blocks(&blocks, "").is_empty());
        assert!(matching_blocks(&blocks, "\" *** ").is_empty());
        assert_eq!(
            matching_blocks(&blocks, "\"toner"),
            vec![0],
            "unterminated quote"
        );
    }
}
