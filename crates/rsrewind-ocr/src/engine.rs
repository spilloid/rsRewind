//! `OcrEngine`: a thin, typed wrapper around `Windows.Media.Ocr.OcrEngine`.

use std::time::Instant;

use rsrewind_core::{BgraFrame, OcrBlock};
use windows::Foundation::Rect;
use windows::Globalization::Language;
use windows::Media::Ocr::OcrEngine as WinOcrEngine;
use windows::core::HSTRING;

use crate::bitmap::software_bitmap_from_bgra;
use crate::error::{OcrError, Result};
use crate::geometry::{downscale_bgra, packed_pixels, scale_rect, union_rect};

/// The result of recognizing one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrOutput {
    /// One block per recognized line, in reading order.
    pub blocks: Vec<OcrBlock>,
    /// All block texts joined by `\n`, in the same order as `blocks`.
    pub text: String,
    /// Wall-clock time spent inside `RecognizeAsync` (plus bitmap construction/downscale).
    pub elapsed_ms: u64,
}

/// A loaded `Windows.Media.Ocr.OcrEngine`.
///
/// # Apartment / threading
///
/// Every call here that touches WinRT (`TryCreateFromLanguage`, `RecognizeAsync`, ...) goes
/// through `windows-core`'s `FactoryCache` / `RoGetActivationFactory` path. That path calls
/// `RoGetActivationFactory` and, if it returns `CO_E_NOTINITIALIZED` (the calling thread has
/// never called `CoInitializeEx`/`RoInitialize`), falls back to `CoIncrementMTAUsage` to join
/// the process's implicit multi-threaded apartment, then retries
/// (`windows-core-0.62.2/src/imp/factory_cache.rs::load_factory`). In other words: **no
/// explicit `RoInitialize` call is required**. A thread that has not initialized any apartment
/// of its own transparently joins the implicit MTA on first use. `OcrEngine` and `OcrResult`
/// (etc.) are also marked `unsafe impl Send + Sync` in `windows` 0.62, consistent with the
/// WinRT OCR APIs being free-threaded/agile.
///
/// This crate therefore provides no `init_thread()` helper: calling `OcrEngine::new` (or
/// anything else here) on a dedicated worker thread that has not called `CoInitializeEx` itself
/// "just works". The one thing callers must still get right themselves is: don't call
/// `CoInitializeEx(COINIT_APARTMENTTHREADED)` on that thread first, or `RoGetActivationFactory`
/// will run on an STA instead of the implicit MTA (still correct, just loses the "any thread"
/// property this module was written to rely on).
///
/// `RecognizeAsync`'s `IAsyncOperation<OcrResult>` is awaited with `.join()` (the `windows`
/// 0.62 / `windows-future` 0.3 name for what used to be `.get()`): it blocks the calling thread
/// on a `CreateEventW` handle that the WinRT completion callback signals
/// (`windows-future-0.3.2/src/join.rs` + `src/waiter.rs`) — a plain kernel wait, not a message
/// pump, so it is safe to call from a background worker thread with no window and no message
/// loop.
pub struct OcrEngine {
    inner: WinOcrEngine,
}

impl OcrEngine {
    /// Creates an engine for `language` (a BCP-47 tag such as `"en-US"`), or for the user's
    /// profile languages when `None`. Windows reports "no OCR language installed" by handing
    /// back a null engine rather than an error, so a null result here is folded into
    /// [`OcrError::NoLanguage`] either way.
    pub fn new(language: Option<&str>) -> Result<Self> {
        let inner = match language {
            Some(tag) => {
                let hstring = HSTRING::from(tag);
                let lang = Language::CreateLanguage(&hstring).map_err(|source| {
                    OcrError::InvalidLanguageTag {
                        tag: tag.to_string(),
                        source,
                    }
                })?;
                WinOcrEngine::TryCreateFromLanguage(&lang)
            }
            None => WinOcrEngine::TryCreateFromUserProfileLanguages(),
        }
        .map_err(|_| OcrError::no_language(language))?;

        Ok(Self { inner })
    }

    /// BCP-47 tags of every OCR-capable language installed on this machine.
    pub fn available_languages() -> Vec<String> {
        let Ok(view) = WinOcrEngine::AvailableRecognizerLanguages() else {
            return Vec::new();
        };
        let Ok(size) = view.Size() else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(size as usize);
        for i in 0..size {
            let Ok(lang) = view.GetAt(i) else { continue };
            let Ok(tag) = lang.LanguageTag() else {
                continue;
            };
            out.push(tag.to_string_lossy());
        }
        out
    }

    /// The largest width or height `Windows.Media.Ocr` will accept; larger frames must be
    /// downscaled first.
    pub fn max_image_dimension() -> Result<u32> {
        Ok(WinOcrEngine::MaxImageDimension()?)
    }

    /// Recognizes text in `frame`. Rejects malformed or zero-size frames with a typed error
    /// instead of panicking; everything else (bitmap construction, optional downscale,
    /// `RecognizeAsync`) is infallible from the caller's point of view except for typed
    /// `OcrError`s.
    pub fn recognize(&self, frame: &BgraFrame) -> Result<OcrOutput> {
        if frame.width == 0 || frame.height == 0 {
            return Err(OcrError::EmptyFrame);
        }
        if !frame.is_well_formed() {
            return Err(OcrError::MalformedFrame);
        }

        let start = Instant::now();

        let packed = packed_pixels(frame);
        let max_dim = Self::max_image_dimension()?;
        let (scaled_pixels, scaled_width, scaled_height) =
            downscale_bgra(&packed, frame.width, frame.height, max_dim);

        // Scale factor to map rectangles in `scaled_*` pixel space back to the caller's
        // original frame pixel space. 1.0 when no downscale happened.
        let scale_x = frame.width as f32 / scaled_width as f32;
        let scale_y = frame.height as f32 / scaled_height as f32;

        let bitmap = software_bitmap_from_bgra(&scaled_pixels, scaled_width, scaled_height)?;
        let result = self.inner.RecognizeAsync(&bitmap)?.join()?;

        let mut blocks = Vec::new();
        let lines = result.Lines()?;
        let line_count = lines.Size()?;
        for line_index in 0..line_count {
            let line = lines.GetAt(line_index)?;
            let text = line.Text()?.to_string_lossy();

            let words = line.Words()?;
            let word_count = words.Size()?;
            let mut word_rects: Vec<Rect> = Vec::with_capacity(word_count as usize);
            for word_index in 0..word_count {
                let word = words.GetAt(word_index)?;
                word_rects.push(word.BoundingRect()?);
            }

            // A line with no recognized words (shouldn't happen, but the API shape allows it)
            // contributes no block rather than a zero rect that would be misleading.
            let Some(rect) = union_rect(&word_rects) else {
                continue;
            };
            let rect = scale_rect(rect, scale_x, scale_y);

            blocks.push(OcrBlock {
                text,
                x: rect.X,
                y: rect.Y,
                width: rect.Width,
                height: rect.Height,
                confidence: None,
                line_index,
            });
        }

        let text = blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(OcrOutput {
            blocks,
            text,
            elapsed_ms,
        })
    }
}
