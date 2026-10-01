//! Screenshot files: lossy WebP encode/decode and crash-safe, never-overwriting writes.

use crate::{Result, StorageError};
use rsrewind_core::{BgraFrame, DataDir};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// libwebp's hard limit on either dimension.
pub const MAX_WEBP_DIMENSION: u32 = 16_383;

/// Encodes a BGRA frame (packed or strided) as lossy WebP.
///
/// The alpha channel is dropped: captured desktop alpha carries no information (and some capture
/// paths leave it 0, which would make the image invisible), and an RGB WebP is smaller.
pub fn encode_webp(frame: &BgraFrame, quality: u8) -> Result<Vec<u8>> {
    if frame.width == 0 || frame.height == 0 {
        return Err(StorageError::Image("cannot encode an empty frame".into()));
    }
    if frame.width > MAX_WEBP_DIMENSION || frame.height > MAX_WEBP_DIMENSION {
        return Err(StorageError::Image(format!(
            "{}x{} exceeds the WebP limit of {MAX_WEBP_DIMENSION} pixels per side",
            frame.width, frame.height
        )));
    }
    if !frame.is_well_formed() {
        return Err(StorageError::Image(format!(
            "malformed frame: {frame:?}, expected at least {} bytes",
            frame.expected_len()
        )));
    }
    let width = frame.width as usize;
    let stride = frame.stride as usize;
    let mut rgb = Vec::with_capacity(width * frame.height as usize * 3);
    // Walk rows by stride so padding bytes at the end of each row are never read as pixels.
    for y in 0..frame.height as usize {
        let start = y * stride;
        let row = frame
            .pixels
            .get(start..start + width * 4)
            .ok_or_else(|| StorageError::Image("frame row out of bounds".into()))?;
        for &[b, g, r, _] in row.as_chunks::<4>().0 {
            rgb.extend_from_slice(&[r, g, b]);
        }
    }
    let encoder = webp::Encoder::from_rgb(&rgb, frame.width, frame.height);
    let memory = encoder
        .encode_simple(false, f32::from(quality.min(100)))
        .map_err(|e| StorageError::Image(format!("WebP encoding failed: {e:?}")))?;
    Ok(memory.to_vec())
}

/// Decodes a still WebP image into a packed BGRA frame (alpha 255 when the image has none).
pub fn decode_webp(bytes: &[u8]) -> Result<BgraFrame> {
    let image = webp::Decoder::new(bytes)
        .decode()
        .ok_or_else(|| StorageError::Image("not a decodable still WebP image".into()))?;
    let (width, height) = (image.width(), image.height());
    let source: &[u8] = &image;
    let channels = if image.is_alpha() { 4 } else { 3 };
    let pixel_count = width as usize * height as usize;
    if source.len() < pixel_count * channels {
        return Err(StorageError::Image("decoded WebP buffer is short".into()));
    }
    let mut pixels = Vec::with_capacity(pixel_count * 4);
    for px in source.chunks_exact(channels).take(pixel_count) {
        let alpha = if channels == 4 { px[3] } else { 255 };
        pixels.extend_from_slice(&[px[2], px[1], px[0], alpha]);
    }
    Ok(BgraFrame {
        width,
        height,
        stride: width * 4,
        pixels,
    })
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `bytes` to the media file at `relative` (forward slashes, inside the data root).
///
/// The bytes go to a temporary file in the destination directory first and are flushed to disk,
/// then published under the final name *without ever replacing* an existing file: an existing
/// destination is an error. A crash leaves at worst a stray temp file, never a torn image.
pub fn write_webp_exclusive(data: &DataDir, relative: &str, bytes: &[u8]) -> Result<PathBuf> {
    let destination = resolve_media_path(data, relative)?;
    let parent = destination
        .parent()
        .ok_or_else(|| StorageError::InvalidMediaPath(relative.to_owned()))?;
    std::fs::create_dir_all(parent).map_err(|e| StorageError::io(parent, e))?;
    if destination.exists() {
        return Err(StorageError::MediaExists(destination));
    }

    let temp = temp_path(parent, &destination);
    let result = write_temp(&temp, bytes).and_then(|()| publish(&temp, &destination));
    // After a successful hard link the temp name is a second link to the same file; after a
    // failure it is garbage. Either way it goes.
    if temp.exists() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map(|()| destination)
}

/// Maps a stored relative media path to an absolute path, rejecting anything unsafe.
pub fn resolve_media_path(data: &DataDir, relative: &str) -> Result<PathBuf> {
    // Stored paths use forward slashes only; a backslash means someone else wrote this value.
    if relative.contains('\\') {
        return Err(StorageError::InvalidMediaPath(relative.to_owned()));
    }
    data.resolve_media(relative)
        .ok_or_else(|| StorageError::InvalidMediaPath(relative.to_owned()))
}

fn temp_path(parent: &Path, destination: &Path) -> PathBuf {
    let name = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".{name}.{}.{counter}.tmp", std::process::id()))
}

fn write_temp(temp: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|e| StorageError::io(temp, e))?;
    file.write_all(bytes)
        .map_err(|e| StorageError::io(temp, e))?;
    file.sync_all().map_err(|e| StorageError::io(temp, e))
}

/// Gives the finished temp file its final name, failing if that name is taken.
fn publish(temp: &Path, destination: &Path) -> Result<()> {
    // `std::fs::rename` on Windows passes MOVEFILE_REPLACE_EXISTING and would silently overwrite.
    // A hard link is atomic and fails if the destination exists, which is exactly the semantics
    // we want; the temp name is removed by the caller afterwards.
    match std::fs::hard_link(temp, destination) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists || destination.exists() => {
            Err(StorageError::MediaExists(destination.to_path_buf()))
        }
        Err(link_error) => {
            // Filesystems without hard links (FAT/exFAT data drives). The existence check above
            // and this rename are not atomic together, but media names are unique per monitor and
            // millisecond and only the persist thread creates them.
            tracing::debug!(error = %link_error, "hard link unavailable, falling back to rename");
            std::fs::rename(temp, destination).map_err(|e| StorageError::io(destination, e))
        }
    }
}
