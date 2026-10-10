//! Windows.Media.Ocr wrapper for rsRewind.
//!
//! See [`OcrEngine`] for the public API and its doc comment for the COM apartment / threading
//! assumptions this crate relies on (short version: no explicit `RoInitialize` is needed; call
//! it from any dedicated worker thread that has not itself called `CoInitializeEx`).
//!
//! **Never log recognized text at `info` level or above** (see `docs/mvp-contract.md`). If you
//! need to see OCR output while debugging, use `tracing::debug!` and say in a comment why that
//! call site is safe (e.g. gated behind a local-only debug build, not the daemon's normal log
//! path).
//!
//! Windows uses Windows.Media.Ocr ([`OcrEngine`]); Linux uses `ocrs` ([`OcrsEngine`]), whose models
//! the user places in the data folder.

#[cfg(windows)]
mod bitmap;
#[cfg(windows)]
mod engine;
#[cfg(windows)]
mod error;
#[cfg(windows)]
mod gdi_render;
#[cfg(windows)]
mod geometry;
#[cfg(target_os = "linux")]
mod ocrs_engine;

#[cfg(windows)]
pub use engine::{OcrEngine, OcrOutput};
#[cfg(windows)]
pub use error::{OcrError, Result};
#[cfg(windows)]
pub use gdi_render::render_text_to_frame;
#[cfg(windows)]
pub use geometry::{downscale_bgra, packed_pixels, scale_rect, union_rect};
#[cfg(target_os = "linux")]
pub use ocrs_engine::{DETECTION_MODEL, ENGINE_NAME, OcrsEngine, OcrsOutput, RECOGNITION_MODEL};
