//! Native ScreenCaptureKit bridge. All returned native allocations are copied and freed here.
use crate::{ScreenRect, VisibleWindow};
use rsrewind_core::{BgraFrame, MonitorInfo};
use serde::Deserialize;
use std::ffi::{CStr, c_char, c_void};
unsafe extern "C" {
    fn rs_capture_permission(request: bool) -> bool;
    fn rs_capture_monitors() -> *mut c_char;
    fn rs_capture_windows() -> *mut c_char;
    fn rs_capture_idle() -> f64;
    fn rs_capture_frame(
        id: u32,
        width: *mut u32,
        height: *mut u32,
        length: *mut usize,
    ) -> *mut c_void;
    fn rs_capture_free(p: *mut c_void);
}
/// Check authorization; only an explicit interactive doctor request should ask for it.
pub fn screen_recording_permission(request: bool) -> bool {
    // SAFETY: no pointers; native implementation returns a Boolean.
    unsafe { rs_capture_permission(request) }
}
fn decode<T: serde::de::DeserializeOwned>(p: *mut c_char) -> Result<T, String> {
    if p.is_null() {
        return Err("macOS desktop context unavailable (check Screen Recording permission and unlocked console session)".into());
    }
    // SAFETY: bridge returns a live strdup allocation with a terminating NUL; copy before free.
    let bytes = unsafe { CStr::from_ptr(p).to_bytes().to_vec() };
    // SAFETY: this allocation belongs to the bridge and is released exactly once.
    unsafe { rs_capture_free(p.cast()) };
    serde_json::from_slice(&bytes).map_err(|_| "invalid native desktop context".into())
}
#[derive(Deserialize)]
pub struct Display {
    pub id: u32,
    pub info: MonitorInfo,
}
pub fn monitors() -> Result<Vec<Display>, String> {
    // SAFETY: no inputs; returned allocation is handled by decode.
    decode(unsafe { rs_capture_monitors() })
}
#[derive(Deserialize)]
struct Window {
    process_name: String,
    title: String,
    pid: u32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}
pub fn visible_windows() -> Result<Vec<VisibleWindow>, String> {
    // SAFETY: no inputs; returned allocation is handled by decode.
    let windows: Vec<Window> = decode(unsafe { rs_capture_windows() })?;
    Ok(windows
        .into_iter()
        .map(|w| VisibleWindow {
            process_name: w.process_name,
            title: w.title,
            pid: w.pid,
            monitor: String::new(),
            rect: ScreenRect {
                left: w.left,
                top: w.top,
                right: w.right,
                bottom: w.bottom,
            },
        })
        .collect())
}
pub fn idle_millis() -> Option<u64> {
    // SAFETY: no pointers; unknown/locked sessions return a negative sentinel.
    let millis = unsafe { rs_capture_idle() };
    (millis.is_finite() && millis >= 0.0).then_some(millis as u64)
}
pub fn capture(id: u32) -> Result<BgraFrame, String> {
    let (mut width, mut height, mut length) = (0, 0, 0);
    // SAFETY: output pointers refer to live locals for the duration of this synchronous call.
    let p = unsafe { rs_capture_frame(id, &mut width, &mut height, &mut length) };
    if p.is_null() {
        return Err("ScreenCaptureKit capture failed or timed out; check Screen Recording authorization and console session".into());
    }
    let valid = width > 0
        && height > 0
        && width.checked_mul(4).is_some()
        && u64::from(width) * u64::from(height) * 4 == length as u64
        && length <= 512 * 1024 * 1024;
    let pixels = if valid {
        // SAFETY: native bridge allocated exactly length bytes and does not retain this pointer.
        unsafe { std::slice::from_raw_parts(p.cast::<u8>(), length).to_vec() }
    } else {
        Vec::new()
    };
    // SAFETY: allocation belongs to the native bridge, freed exactly once on every path.
    unsafe { rs_capture_free(p) };
    if !valid {
        return Err("invalid ScreenCaptureKit frame geometry".into());
    }
    Ok(BgraFrame {
        width,
        height,
        stride: width * 4,
        pixels,
    })
}
#[cfg(test)]
mod tests {
    #[test]
    fn null_context_fails_closed() {
        assert!(super::decode::<Vec<super::Display>>(std::ptr::null_mut()).is_err());
    }
}
