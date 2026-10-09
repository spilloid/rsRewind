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
#![cfg(windows)]

mod bitmap;
mod engine;
mod error;
mod gdi_render;
mod geometry;

pub use engine::{OcrEngine, OcrOutput};
pub use error::{OcrError, Result};
pub use gdi_render::render_text_to_frame;
pub use geometry::{downscale_bgra, packed_pixels, scale_rect, union_rect};
