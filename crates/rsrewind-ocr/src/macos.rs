//! In-memory Apple Vision OCR; recognized content never appears in errors.
use rsrewind_core::{BgraFrame, OcrBlock};
use std::ffi::{CStr, CString, c_char, c_void};
use std::time::Instant;
pub const ENGINE_NAME: &str = "apple-vision";
unsafe extern "C" {
    fn rs_ocr_recognize(
        pixels: *const u8,
        width: u32,
        height: u32,
        stride: u32,
        language: *const c_char,
    ) -> *mut c_char;
    fn rs_ocr_free(p: *mut c_void);
}
pub struct VisionEngine {
    language: CString,
}
pub struct VisionOutput {
    pub blocks: Vec<OcrBlock>,
    pub elapsed_ms: u64,
}
impl VisionEngine {
    pub fn new(language: Option<&str>) -> Result<Self, String> {
        Ok(Self {
            language: CString::new(language.unwrap_or(""))
                .map_err(|_| "invalid OCR language".to_string())?,
        })
    }
    pub fn recognize(&self, frame: &BgraFrame) -> Result<VisionOutput, String> {
        if frame.width == 0 || frame.height == 0 || !frame.is_well_formed() {
            return Err("invalid OCR frame geometry".into());
        }
        // Vision copies stride * height bytes; into_packed eliminates a possibly unpadded last row.
        let packed = frame.clone().into_packed();
        let started = Instant::now();
        // SAFETY: packed pixels and NUL-terminated language outlive the synchronous native call.
        let p = unsafe {
            rs_ocr_recognize(
                packed.pixels.as_ptr(),
                packed.width,
                packed.height,
                packed.stride,
                self.language.as_ptr(),
            )
        };
        if p.is_null() {
            return Err("Apple Vision text recognition failed (check configured language)".into());
        }
        // SAFETY: bridge returns a live strdup allocation terminated with NUL.
        let bytes = unsafe { CStr::from_ptr(p).to_bytes().to_vec() };
        // SAFETY: native allocation is released once, after copying.
        unsafe { rs_ocr_free(p.cast()) };
        let blocks = serde_json::from_slice(&bytes)
            .map_err(|_| "invalid Apple Vision result".to_string())?;
        Ok(VisionOutput {
            blocks,
            elapsed_ms: started.elapsed().as_millis() as u64,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_frame_is_rejected_before_native_read() -> Result<(), String> {
        let engine = VisionEngine::new(None)?;
        assert!(
            engine
                .recognize(&BgraFrame {
                    width: 10,
                    height: 10,
                    stride: 40,
                    pixels: vec![0; 3]
                })
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn blank_frame_returns_no_text() -> Result<(), String> {
        let engine = VisionEngine::new(None)?;
        let out = engine.recognize(&BgraFrame {
            width: 64,
            height: 64,
            stride: 256,
            pixels: vec![255; 64 * 256],
        })?;
        assert!(out.blocks.is_empty());
        Ok(())
    }
}
