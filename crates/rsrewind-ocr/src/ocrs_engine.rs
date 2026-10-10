//! Text recognition on Linux with `ocrs` (pure Rust, models run by `rten`).
//!
//! The two models (`text-detection.rten`, `text-recognition.rten`, about 12 MB together; CC-BY-SA-4.0,
//! trained on HierText and synthetic data) are read from the data folder's `models/` directory. rsRewind
//! never downloads them: no network code. If they are missing, construction fails with a message that
//! says where to put them, the recorder keeps recording, and moments wait as `pending` until they exist.
//!
//! Output matches the Windows engine: one block per recognized line, rectangle in the frame's own pixel
//! coordinates, reading order in `line_index`. Recognized text is never logged.

use ocrs::{ImageSource, OcrEngineParams, TextItem};
use rsrewind_core::{BgraFrame, OcrBlock};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const DETECTION_MODEL: &str = "text-detection.rten";
pub const RECOGNITION_MODEL: &str = "text-recognition.rten";
/// Recorded with each result (`visual_states.ocr_engine`).
pub const ENGINE_NAME: &str = "ocrs";

pub struct OcrsEngine {
    engine: ocrs::OcrEngine,
}

/// What [`OcrsEngine::recognize`] found.
pub struct OcrsOutput {
    pub blocks: Vec<OcrBlock>,
    pub elapsed_ms: u64,
}

impl OcrsEngine {
    /// Loads both models from `models_dir`. The error names the missing file and the folder, never
    /// screen content.
    pub fn load(models_dir: &Path) -> Result<Self, String> {
        let load = |name: &str| -> Result<rten::Model, String> {
            let path: PathBuf = models_dir.join(name);
            if !path.is_file() {
                return Err(format!(
                    "text recognition needs {name} in {} (see the install page's Linux section)",
                    models_dir.display()
                ));
            }
            rten::Model::load_file(&path)
                .map_err(|e| format!("could not load {}: {e}", path.display()))
        };
        let engine = ocrs::OcrEngine::new(OcrEngineParams {
            detection_model: Some(load(DETECTION_MODEL)?),
            recognition_model: Some(load(RECOGNITION_MODEL)?),
            ..Default::default()
        })
        .map_err(|e| format!("could not start text recognition: {e}"))?;
        Ok(Self { engine })
    }

    pub fn recognize(&self, frame: &BgraFrame) -> Result<OcrsOutput, String> {
        let started = Instant::now();
        let rgb = bgra_to_rgb(frame);
        let source = ImageSource::from_bytes(&rgb, (frame.width, frame.height))
            .map_err(|e| format!("frame not usable for text recognition: {e:?}"))?;
        let input = self
            .engine
            .prepare_input(source)
            .map_err(|e| e.to_string())?;
        let words = self
            .engine
            .detect_words(&input)
            .map_err(|e| e.to_string())?;
        let lines = self.engine.find_text_lines(&input, &words);
        let recognized = self
            .engine
            .recognize_text(&input, &lines)
            .map_err(|e| e.to_string())?;
        let mut blocks = Vec::new();
        for line in recognized.into_iter().flatten() {
            // `bounding_rect` needs at least one character.
            if line.chars().is_empty() {
                continue;
            }
            let text = line.to_string();
            if text.trim().is_empty() {
                continue;
            }
            let rect = line.bounding_rect();
            blocks.push(OcrBlock {
                text,
                x: rect.left() as f32,
                y: rect.top() as f32,
                width: rect.width() as f32,
                height: rect.height() as f32,
                confidence: None,
                line_index: blocks.len() as u32,
            });
        }
        Ok(OcrsOutput {
            blocks,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }
}

/// Packed RGB8 from a (possibly strided) BGRA frame.
fn bgra_to_rgb(frame: &BgraFrame) -> Vec<u8> {
    let (w, h, stride) = (
        frame.width as usize,
        frame.height as usize,
        frame.stride as usize,
    );
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        let Some(row) = frame.pixels.get(y * stride..y * stride + w * 4) else {
            break;
        };
        for px in row.as_chunks::<4>().0 {
            out.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_with_row_padding_becomes_packed_rgb() {
        // 2x2, stride 12 (4 bytes of padding per row).
        let frame = BgraFrame {
            width: 2,
            height: 2,
            stride: 12,
            pixels: vec![
                1, 2, 3, 255, 4, 5, 6, 255, 0, 0, 0, 0, //
                7, 8, 9, 255, 10, 11, 12, 255, 0, 0, 0, 0,
            ],
        };
        assert_eq!(
            bgra_to_rgb(&frame),
            vec![3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]
        );
    }

    #[test]
    fn missing_models_say_where_to_put_them() {
        let dir = std::env::temp_dir().join("rsrewind-ocrs-no-models");
        let Err(message) = OcrsEngine::load(&dir) else {
            panic!("loading from an empty folder must fail");
        };
        assert!(message.contains(DETECTION_MODEL), "{message}");
        assert!(message.contains("rsrewind-ocrs-no-models"), "{message}");
    }

    /// Real models and a real picture: run with `RSREWIND_OCRS_MODELS=<dir> cargo test -p rsrewind-ocr
    /// -- --ignored`. Renders known text with the system font is not available here, so it reads a
    /// WebP given by `RSREWIND_OCRS_SAMPLE` and just checks that some text comes back.
    #[test]
    #[ignore]
    fn recognizes_text_with_real_models() -> Result<(), Box<dyn std::error::Error>> {
        let models = std::env::var_os("RSREWIND_OCRS_MODELS").ok_or("set RSREWIND_OCRS_MODELS")?;
        let engine = OcrsEngine::load(Path::new(&models))?;
        let sample = std::env::var_os("RSREWIND_OCRS_SAMPLE").ok_or("set RSREWIND_OCRS_SAMPLE")?;
        let bytes = std::fs::read(sample)?;
        let image = webp::Decoder::new(&bytes).decode().ok_or("not a WebP")?;
        let (w, h) = (image.width(), image.height());
        let channels = if image.is_alpha() { 4 } else { 3 };
        let pixels: Vec<u8> = image
            .chunks_exact(channels)
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect();
        let out = engine.recognize(&BgraFrame {
            width: w,
            height: h,
            stride: w * 4,
            pixels,
        })?;
        println!("{} lines in {} ms", out.blocks.len(), out.elapsed_ms);
        assert!(!out.blocks.is_empty());
        Ok(())
    }
}
