//! Text recognition on Linux with Tesseract, run as a child process (`tesseract - - tsv`): the frame
//! goes in as a PNG on stdin, word boxes come back as TSV on stdout. rsRewind links no OCR library and
//! downloads nothing; Tesseract comes from the system (`dnf`/`rpm-ostree install tesseract`, `apt
//! install tesseract-ocr`, `brew install tesseract`). If it is missing, construction fails with a message
//! saying how to install it, the recorder keeps recording, and moments wait as `pending`.
//!
//! Chosen over `ocrs` by measurement (2026-10-10, a real dark-mode 2496x1664 screen): Tesseract found 5
//! of 8 visible phrases in ~1.5 s, ocrs 1 of 8 in ~16 s even with the image inverted.
//!
//! Output matches the Windows engine: one block per text line, rectangle in the frame's own pixel
//! coordinates, reading order in `line_index`. Recognized text is never logged.

use rsrewind_core::{BgraFrame, OcrBlock};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

/// Recorded with each result (`visual_states.ocr_engine`).
pub const ENGINE_NAME: &str = "tesseract";

/// Words Tesseract is less sure of than this (0-100) are dropped: they are mostly UI chrome noise.
const MIN_WORD_CONFIDENCE: f32 = 30.0;

pub struct TesseractEngine {
    program: String,
    language: String,
}

pub struct TesseractOutput {
    pub blocks: Vec<OcrBlock>,
    pub elapsed_ms: u64,
}

impl TesseractEngine {
    /// Checks that `tesseract` runs and knows `language` (Tesseract code, default `eng`).
    pub fn new(language: Option<&str>) -> Result<Self, String> {
        let language = language
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(tesseract_language)
            .unwrap_or_else(|| "eng".to_string());
        let program =
            std::env::var("RSREWIND_TESSERACT").unwrap_or_else(|_| "tesseract".to_string());
        let langs = Command::new(&program)
            .arg("--list-langs")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| {
                "text recognition needs Tesseract: install it (Fedora/Bazzite: `rpm-ostree install tesseract` \
                 or `brew install tesseract`; Debian/Ubuntu: `apt install tesseract-ocr`) and restart the recorder"
                    .to_string()
            })?;
        let listed = String::from_utf8_lossy(&langs.stdout);
        if !listed.lines().skip(1).any(|l| l.trim() == language) {
            return Err(format!(
                "Tesseract has no '{language}' language data installed (e.g. the tesseract-langpack-{language} package)"
            ));
        }
        Ok(Self { program, language })
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn recognize(&self, frame: &BgraFrame) -> Result<TesseractOutput, String> {
        let started = Instant::now();
        let png = encode_png(frame)?;
        let mut child = Command::new(&self.program)
            .args(["-", "-", "-l", &self.language, "--psm", "3", "tsv"])
            // Tesseract uses every core through OpenMP unless told otherwise.
            .env(
                "OMP_THREAD_LIMIT",
                std::env::var("OMP_THREAD_LIMIT").unwrap_or_else(|_| "2".into()),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("could not run tesseract: {e}"))?;
        // Write on a thread: a large frame fills the pipe before Tesseract starts printing.
        let mut stdin = child.stdin.take().ok_or("tesseract stdin unavailable")?;
        let writer = std::thread::spawn(move || stdin.write_all(&png));
        let output = child
            .wait_with_output()
            .map_err(|e| format!("tesseract failed: {e}"))?;
        let _ = writer.join();
        if !output.status.success() {
            return Err(format!("tesseract exited with {}", output.status));
        }
        Ok(TesseractOutput {
            blocks: lines_from_tsv(&String::from_utf8_lossy(&output.stdout)),
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// Windows-style language tags (`en-US`) to Tesseract codes (`eng`) for the few common ones; anything
/// else is passed through, so a Tesseract code in `config.toml` works as is.
fn tesseract_language(tag: &str) -> String {
    let primary = tag
        .split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase();
    match primary.as_str() {
        "en" => "eng",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "nl" => "nld",
        "pt" => "por",
        _ => return tag.to_string(),
    }
    .to_string()
}

/// One block per Tesseract line (`block, par, line`), words joined by spaces, rectangle = union of its
/// words. Low-confidence words and empty lines are dropped. Reading order is Tesseract's.
pub fn lines_from_tsv(tsv: &str) -> Vec<OcrBlock> {
    struct Line {
        key: (u32, u32, u32),
        words: Vec<String>,
        rect: (i64, i64, i64, i64),
        conf: f32,
    }
    let mut lines: Vec<Line> = Vec::new();
    for row in tsv.lines().skip(1) {
        let f: Vec<&str> = row.split('\t').collect();
        if f.len() < 12 || f[0] != "5" {
            continue;
        }
        let num = |i: usize| f[i].trim().parse::<i64>().ok();
        let (Some(block), Some(par), Some(line), Some(l), Some(t), Some(w), Some(h)) =
            (num(2), num(3), num(4), num(6), num(7), num(8), num(9))
        else {
            continue;
        };
        let conf: f32 = f[10].trim().parse().unwrap_or(-1.0);
        let text = f[11..].join("\t").trim().to_string();
        if text.is_empty() || conf < MIN_WORD_CONFIDENCE || w <= 0 || h <= 0 {
            continue;
        }
        let key = (block as u32, par as u32, line as u32);
        let (r, b) = (l + w, t + h);
        match lines.last_mut() {
            Some(cur) if cur.key == key => {
                cur.words.push(text);
                cur.rect = (
                    cur.rect.0.min(l),
                    cur.rect.1.min(t),
                    cur.rect.2.max(r),
                    cur.rect.3.max(b),
                );
                cur.conf += conf;
            }
            _ => lines.push(Line {
                key,
                words: vec![text],
                rect: (l, t, r, b),
                conf,
            }),
        }
    }
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let n = line.words.len() as f32;
            OcrBlock {
                text: line.words.join(" "),
                x: line.rect.0 as f32,
                y: line.rect.1 as f32,
                width: (line.rect.2 - line.rect.0) as f32,
                height: (line.rect.3 - line.rect.1) as f32,
                confidence: Some(line.conf / n / 100.0),
                line_index: i as u32,
            }
        })
        .collect()
}

/// RGB PNG of a (possibly strided) BGRA frame; fast compression, it only lives in a pipe.
fn encode_png(frame: &BgraFrame) -> Result<Vec<u8>, String> {
    let (w, h, stride) = (
        frame.width as usize,
        frame.height as usize,
        frame.stride as usize,
    );
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let row = frame
            .pixels
            .get(y * stride..y * stride + w * 4)
            .ok_or("frame shorter than its size")?;
        for px in row.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, frame.width, frame.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSV: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext
1\t1\t0\t0\t0\t0\t0\t0\t800\t600\t-1\t
4\t1\t1\t1\t1\t0\t10\t20\t300\t30\t-1\t
5\t1\t1\t1\t1\t1\t10\t20\t100\t30\t96.5\tBlue
5\t1\t1\t1\t1\t2\t120\t22\t110\t28\t91.0\tScreen
5\t1\t1\t1\t1\t3\t240\t20\t70\t32\t12.0\t~|
5\t1\t2\t1\t1\t1\t400\t300\t50\t20\t88.0\t$40
5\t1\t2\t1\t2\t1\t400\t330\t60\t20\t95.0\t
5\t1\t3\t1\t1\t1\t5\t5\t0\t10\t99.0\tzero-width";

    #[test]
    fn tsv_words_become_one_block_per_line() {
        let blocks = lines_from_tsv(TSV);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        assert_eq!(
            blocks[0].text, "Blue Screen",
            "the low-confidence word is dropped"
        );
        assert_eq!(
            (blocks[0].x, blocks[0].y, blocks[0].width, blocks[0].height),
            (10.0, 20.0, 220.0, 30.0)
        );
        assert_eq!(blocks[0].line_index, 0);
        assert!((blocks[0].confidence.unwrap_or(0.0) - 0.9375).abs() < 1e-4);
        assert_eq!(blocks[1].text, "$40");
        assert_eq!(blocks[1].line_index, 1);
        assert!(lines_from_tsv("").is_empty());
        assert!(lines_from_tsv("header only\n5\tbroken").is_empty());
    }

    #[test]
    fn language_tags_map_to_tesseract_codes() {
        assert_eq!(tesseract_language("en-US"), "eng");
        assert_eq!(tesseract_language("de_DE"), "deu");
        assert_eq!(tesseract_language("chi_sim"), "chi_sim");
    }

    #[test]
    fn frames_encode_to_a_png_of_the_right_size() -> Result<(), String> {
        let frame = BgraFrame {
            width: 3,
            height: 2,
            stride: 16,
            pixels: vec![255; 32],
        };
        let png = encode_png(&frame)?;
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let short = BgraFrame {
            width: 3,
            height: 2,
            stride: 16,
            pixels: vec![0; 10],
        };
        assert!(encode_png(&short).is_err());
        Ok(())
    }

    /// Real Tesseract on real stored frames: `RSREWIND_OCR_SAMPLE=<frame.webp> RSREWIND_OCR_EXPECT="phrase|phrase"
    /// cargo test -p rsrewind-ocr -- --ignored --nocapture` (`RSREWIND_TESSERACT` may point at a wrapper). Prints
    /// counts and which expected phrases were found, never the recognized text.
    #[test]
    #[ignore]
    fn recognizes_a_real_frame() -> Result<(), Box<dyn std::error::Error>> {
        let sample = std::env::var_os("RSREWIND_OCR_SAMPLE").ok_or("set RSREWIND_OCR_SAMPLE")?;
        let bytes = std::fs::read(sample)?;
        let image = webp::Decoder::new(&bytes).decode().ok_or("not a WebP")?;
        let (w, h) = (image.width(), image.height());
        let ch = if image.is_alpha() { 4 } else { 3 };
        let pixels: Vec<u8> = image
            .chunks_exact(ch)
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect();
        let engine = TesseractEngine::new(None)?;
        let out = engine.recognize(&BgraFrame {
            width: w,
            height: h,
            stride: w * 4,
            pixels,
        })?;
        let text: String = out
            .blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        println!("{} lines in {} ms", out.blocks.len(), out.elapsed_ms);
        let expect = std::env::var("RSREWIND_OCR_EXPECT").unwrap_or_default();
        let mut found = 0;
        for phrase in expect.split('|').filter(|p| !p.is_empty()) {
            let hit = text.to_lowercase().contains(&phrase.to_lowercase());
            found += usize::from(hit);
            println!("  {}: {phrase}", if hit { "found" } else { "missed" });
        }
        assert!(!out.blocks.is_empty());
        assert!(expect.is_empty() || found > 0);
        Ok(())
    }
}
