//! A dependency-free (no `image` crate) GDI text renderer, used only by the ignored integration
//! test and the `ocr_probe` example to produce a `BgraFrame` with known text in it, without
//! needing a checked-in fixture image. Never called by `recognize` or anything on the daemon's
//! capture/OCR path.

use rsrewind_core::BgraFrame;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateCompatibleDC,
    CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH, DEFAULT_QUALITY, DIB_RGB_COLORS,
    DT_LEFT, DT_NOCLIP, DT_TOP, DT_WORDBREAK, DeleteDC, DeleteObject, DrawTextW, FW_NORMAL,
    OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::core::w;

use crate::error::{OcrError, Result};

/// Renders `text` into a `width x height` `BgraFrame`: opaque white background, black text,
/// word-wrapped with an 8px margin, using a font sized relative to `height` so short test
/// strings are large enough for the OCR engine to read back reliably.
pub fn render_text_to_frame(text: &str, width: u32, height: u32) -> Result<BgraFrame> {
    if width == 0 || height == 0 {
        return Err(OcrError::EmptyFrame);
    }

    // SAFETY: every GDI handle created below (`hdc`, `hbitmap`, `hfont`) is released on every
    // path out of this function: each `Create*`/`Select*` is paired with a matching
    // `Delete*`/restore before return, including on the early-return error paths. The DIB
    // section's pixel memory (`bits`) is owned by `hbitmap` and is only read or written while
    // `hbitmap` is alive and still the DC's selected bitmap.
    unsafe {
        let hdc = CreateCompatibleDC(None);
        if hdc.is_invalid() {
            return Err(OcrError::GdiFailure("CreateCompatibleDC"));
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // Negative height = top-down DIB rows, matching `BgraFrame`'s documented
                // top-down row order so no flip is needed when copying pixels out.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let hbitmap =
            match CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) {
                Ok(h) => h,
                Err(source) => {
                    let _ = DeleteDC(hdc);
                    return Err(OcrError::from(source));
                }
            };
        if hbitmap.is_invalid() || bits_ptr.is_null() {
            let _ = DeleteDC(hdc);
            return Err(OcrError::GdiFailure("CreateDIBSection"));
        }

        let byte_len = (width as usize) * (height as usize) * 4;
        // SAFETY: `CreateDIBSection` succeeded with `biBitCount = 32` and no `biSizeImage`
        // override, so GDI allocated exactly `width * height * 4` bytes at `bits_ptr`, and
        // `hbitmap` (which owns that allocation) is kept alive until after the last use below.
        let bits: &mut [u8] = std::slice::from_raw_parts_mut(bits_ptr.cast::<u8>(), byte_len);
        bits.fill(0xFF); // opaque white: B = G = R = A = 0xFF

        let previous_bitmap = SelectObject(hdc, hbitmap.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x0000_0000)); // black

        // Negative cHeight = character height in logical units (not cell height), the usual
        // convention for "give me a font at least this tall". A fraction of the frame height
        // keeps short test strings legible without the test/example having to pick a size.
        let font_height = -((height as i32) / 6).max(14);
        let hfont = CreateFontW(
            font_height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            DEFAULT_QUALITY,
            // DEFAULT_PITCH (0) | FF_DONTCARE (0): no pitch/family preference.
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        );
        if hfont.is_invalid() {
            SelectObject(hdc, previous_bitmap);
            let _ = DeleteObject(hbitmap.into());
            let _ = DeleteDC(hdc);
            return Err(OcrError::GdiFailure("CreateFontW"));
        }
        let previous_font = SelectObject(hdc, hfont.into());

        let mut rect = RECT {
            left: 8,
            top: 8,
            right: (width as i32) - 8,
            bottom: (height as i32) - 8,
        };
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(
            hdc,
            &mut wide,
            &mut rect,
            DT_LEFT | DT_TOP | DT_WORDBREAK | DT_NOCLIP,
        );

        // Copy the rendered pixels out before tearing down the GDI objects that own them.
        let mut pixels = vec![0u8; byte_len];
        pixels.copy_from_slice(bits);

        SelectObject(hdc, previous_font);
        SelectObject(hdc, previous_bitmap);
        let _ = DeleteObject(hfont.into());
        let _ = DeleteObject(hbitmap.into());
        let _ = DeleteDC(hdc);

        Ok(BgraFrame {
            width,
            height,
            stride: width * 4,
            pixels,
        })
    }
}
