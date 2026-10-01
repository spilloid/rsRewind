//! Builds a `Windows.Graphics.Imaging.SoftwareBitmap` from a packed BGRA8 pixel buffer.
//!
//! `SoftwareBitmap` has no "wrap this slice, don't copy" constructor from managed code; the
//! WinRT-blessed path is `IBuffer` + `SoftwareBitmap::CreateCopyWithAlphaFromBuffer`, and the
//! only way to fill an `IBuffer` with our own bytes from Rust is `IBufferByteAccess`, a plain
//! COM interface (not WinRT metadata) that `windows` 0.62 generates under
//! `Win32::System::WinRT`. This is the standard C++/WinRT pattern for this exact problem.

use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
use windows::Storage::Streams::Buffer;
use windows::Win32::System::WinRT::IBufferByteAccess;
use windows::core::Interface;

use crate::error::{OcrError, Result};

/// `pixels` must be a tightly packed (`stride == width * 4`) BGRA8 buffer of exactly
/// `width * height * 4` bytes.
pub(crate) fn software_bitmap_from_bgra(
    pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<SoftwareBitmap> {
    let len = u32::try_from(pixels.len()).map_err(|_| OcrError::FrameTooLarge {
        byte_len: pixels.len(),
    })?;

    let buffer = Buffer::Create(len)?;
    buffer.SetLength(len)?;

    let byte_access: IBufferByteAccess = buffer.cast()?;
    // SAFETY: `IBufferByteAccess::Buffer` returns a pointer into storage owned by `buffer`,
    // valid for `buffer.Capacity()` bytes. We just created `buffer` with capacity `len` (via
    // `Buffer::Create(len)`) and `pixels.len() == len`, so writing `pixels.len()` bytes starting
    // at that pointer stays within the allocation. `buffer` is kept alive on this stack frame
    // for the whole call and the raw pointer is not retained past it.
    unsafe {
        let dest = byte_access.Buffer()?;
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), dest, pixels.len());
    }

    let bitmap = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        width as i32,
        height as i32,
        BitmapAlphaMode::Premultiplied,
    )?;
    Ok(bitmap)
}
