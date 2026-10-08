//! A tiny software canvas for the fictional screenshots: filled and rounded rectangles, soft
//! shadows, and anti-aliased text through `ab_glyph`. Every line of text drawn with `ocr: true`
//! becomes an [`OcrBlock`] at exactly the rectangle it was drawn in, so search highlights land on
//! the words a viewer can see.

// Drawing primitives take position, size, radius, colour and opacity flat, like a 2D API does.
#![allow(clippy::too_many_arguments)]

use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use rsrewind_core::{BgraFrame, OcrBlock};
use std::path::{Path, PathBuf};

pub type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub const fn hex(v: u32) -> Rgb {
    Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// The three faces the scenes use, loaded from the operating system's own fonts (nothing is
/// bundled): Segoe UI / Consolas on Windows, Noto or DejaVu elsewhere.
pub struct Fonts {
    pub ui: FontVec,
    pub bold: FontVec,
    pub mono: FontVec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    Ui,
    Bold,
    Mono,
}

impl Fonts {
    pub fn load() -> Fallible<Self> {
        let ui = find(&["segoeui.ttf", "NotoSans-Regular.ttf", "DejaVuSans.ttf"])?;
        let bold = find(&[
            "seguisb.ttf",
            "segoeuib.ttf",
            "NotoSans-SemiBold.ttf",
            "NotoSans-Bold.ttf",
            "DejaVuSans-Bold.ttf",
        ])
        .unwrap_or_else(|_| ui.clone());
        let mono = find(&[
            "CascadiaMono.ttf",
            "consola.ttf",
            "NotoSansMono-Regular.ttf",
            "DejaVuSansMono.ttf",
        ])?;
        let load =
            |p: &Path| -> Fallible<FontVec> { Ok(FontVec::try_from_vec(std::fs::read(p)?)?) };
        Ok(Self {
            ui: load(&ui)?,
            bold: load(&bold)?,
            mono: load(&mono)?,
        })
    }

    fn face(&self, face: Face) -> &FontVec {
        match face {
            Face::Ui => &self.ui,
            Face::Bold => &self.bold,
            Face::Mono => &self.mono,
        }
    }
}

/// The first font file with one of these names under the usual font folders.
fn find(names: &[&str]) -> Fallible<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("SYNTH_FONT_DIR") {
        roots.push(dir.into());
    }
    if let Some(windir) = std::env::var_os("WINDIR") {
        roots.push(PathBuf::from(windir).join("Fonts"));
    }
    roots.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".local/share/fonts"));
    }
    for name in names {
        for root in &roots {
            if let Some(found) = search(root, name, 4) {
                return Ok(found);
            }
        }
    }
    Err(format!("no font found among {names:?} (set SYNTH_FONT_DIR)").into())
}

fn search(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Some(candidate);
    }
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .find_map(|e| search(&e.path(), name, depth - 1))
}

pub struct Canvas {
    pub w: i32,
    pub h: i32,
    px: Vec<u8>,
    pub blocks: Vec<OcrBlock>,
}

impl Canvas {
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w: w as i32,
            h: h as i32,
            px: vec![255; (w * h * 4) as usize],
            blocks: Vec::new(),
        }
    }

    pub fn into_frame(self) -> (BgraFrame, Vec<OcrBlock>) {
        (
            BgraFrame {
                width: self.w as u32,
                height: self.h as u32,
                stride: self.w as u32 * 4,
                pixels: self.px,
            },
            self.blocks,
        )
    }

    fn blend(&mut self, x: i32, y: i32, c: Rgb, a: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a <= 0.0 {
            return;
        }
        let i = ((y * self.w + x) * 4) as usize;
        let a = a.min(1.0);
        for (k, v) in [c.2, c.1, c.0].into_iter().enumerate() {
            let old = f32::from(self.px[i + k]);
            self.px[i + k] = (old + (f32::from(v) - old) * a).round() as u8;
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb) {
        self.rect_a(x, y, w, h, c, 1.0);
    }

    pub fn rect_a(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb, a: f32) {
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                self.blend(xx, yy, c, a);
            }
        }
    }

    /// A rectangle with rounded corners, anti-aliased.
    pub fn round(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, c: Rgb, a: f32) {
        let (fx, fy, fw, fh) = (x as f32, y as f32, w as f32, h as f32);
        let r = r.min(fw / 2.0).min(fh / 2.0);
        for yy in y.max(0)..(y + h).min(self.h) {
            for xx in x.max(0)..(x + w).min(self.w) {
                let (px, py) = (xx as f32 + 0.5, yy as f32 + 0.5);
                let cx = px.clamp(fx + r, fx + fw - r);
                let cy = py.clamp(fy + r, fy + fh - r);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - r;
                let cover = (0.5 - d).clamp(0.0, 1.0);
                self.blend(xx, yy, c, a * cover);
            }
        }
    }

    /// A rounded outline of width `t`.
    pub fn outline(&mut self, x: i32, y: i32, w: i32, h: i32, r: f32, t: i32, c: Rgb) {
        self.rect(x + r as i32, y, w - 2 * r as i32, t, c);
        self.rect(x + r as i32, y + h - t, w - 2 * r as i32, t, c);
        self.rect(x, y + r as i32, t, h - 2 * r as i32, c);
        self.rect(x + w - t, y + r as i32, t, h - 2 * r as i32, c);
    }

    /// A soft drop shadow under a window.
    pub fn shadow(&mut self, x: i32, y: i32, w: i32, h: i32) {
        for k in (1..=14).rev() {
            let a = 0.018 * (15 - k) as f32 / 14.0;
            self.round(
                x - k,
                y - k + 6,
                w + 2 * k,
                h + 2 * k,
                10.0 + k as f32,
                Rgb(0, 0, 0),
                a,
            );
        }
    }

    pub fn gradient(&mut self, top: Rgb, bottom: Rgb, glow: Rgb, glow_at: (f32, f32)) {
        let (w, h) = (self.w as f32, self.h as f32);
        for y in 0..self.h {
            let t = y as f32 / h;
            let mix = |a: u8, b: u8| f32::from(a) + (f32::from(b) - f32::from(a)) * t;
            for x in 0..self.w {
                let dx = (x as f32 - glow_at.0 * w) / w;
                let dy = (y as f32 - glow_at.1 * h) / h;
                let g = (1.0 - (dx * dx + dy * dy).sqrt() * 1.6).clamp(0.0, 1.0) * 0.55;
                let c = |a: u8, b: u8, gl: u8| (mix(a, b) + (f32::from(gl) - mix(a, b)) * g) as u8;
                let i = ((y * self.w + x) * 4) as usize;
                self.px[i] = c(top.2, bottom.2, glow.2);
                self.px[i + 1] = c(top.1, bottom.1, glow.1);
                self.px[i + 2] = c(top.0, bottom.0, glow.0);
                self.px[i + 3] = 255;
            }
        }
    }

    /// Width a line of text takes.
    pub fn measure(&self, fonts: &Fonts, face: Face, size: f32, text: &str) -> f32 {
        let font = fonts.face(face);
        let scaled = font.as_scaled(PxScale::from(size));
        let mut caret = 0.0;
        let mut prev = None;
        for ch in text.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                caret += scaled.kern(p, id);
            }
            caret += scaled.h_advance(id);
            prev = Some(id);
        }
        caret
    }

    /// Draws one line with its top-left at `(x, y)`; returns its width. Recorded as an OCR line
    /// when `ocr` is set (that is what the recognizer would have read there).
    pub fn text(
        &mut self,
        fonts: &Fonts,
        face: Face,
        size: f32,
        x: f32,
        y: f32,
        text: &str,
        c: Rgb,
        ocr: bool,
    ) -> f32 {
        let font = fonts.face(face);
        let scale = PxScale::from(size);
        let scaled = font.as_scaled(scale);
        let ascent = scaled.ascent();
        let mut caret = x;
        let mut prev = None;
        let mut coverage: Vec<(i32, i32, f32)> = Vec::new();
        for ch in text.chars() {
            let id = font.glyph_id(ch);
            if let Some(p) = prev {
                caret += scaled.kern(p, id);
            }
            let glyph = id.with_scale_and_position(scale, point(caret, y + ascent));
            caret += scaled.h_advance(id);
            prev = Some(id);
            if let Some(outline) = font.outline_glyph(glyph) {
                let b = outline.px_bounds();
                outline.draw(|gx, gy, v| {
                    coverage.push((b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, v));
                });
            }
        }
        for (px, py, v) in coverage {
            self.blend(px, py, c, v);
        }
        let width = caret - x;
        if ocr && !text.trim().is_empty() {
            let height = ascent - scaled.descent();
            self.blocks.push(OcrBlock {
                text: text.to_owned(),
                x,
                y,
                width,
                height,
                confidence: None,
                line_index: self.blocks.len() as u32,
            });
        }
        width
    }
}
