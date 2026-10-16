//! Native ScreenCaptureKit / CoreGraphics bridge. All returned native allocations are copied and
//! freed here. The bridge reports raw facts; [`crate::macos_facts`] decides what they mean.
use crate::VisibleWindow;
use crate::macos_facts::{
    self, OwnIdentity, RuleNeeds, SessionFacts, SessionUnknown, WindowList, WindowListError,
    WindowSnapshot,
};
use rsrewind_core::{BgraFrame, MonitorInfo};
use serde::Deserialize;
use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;
unsafe extern "C" {
    fn rs_capture_permission(request: bool) -> bool;
    fn rs_capture_monitors() -> *mut c_char;
    fn rs_capture_windows() -> *mut c_char;
    fn rs_capture_session() -> *mut c_char;
    fn rs_capture_frame(
        id: u32,
        width: *mut u32,
        height: *mut u32,
        length: *mut usize,
        reason: *mut i32,
        os_code: *mut isize,
    ) -> *mut c_void;
    fn rs_capture_free(p: *mut c_void);
}
/// Check authorization; only an explicit interactive doctor request should ask for it.
pub fn screen_recording_permission(request: bool) -> bool {
    // SAFETY: no pointers; native implementation returns a Boolean.
    unsafe { rs_capture_permission(request) }
}
/// Copies and frees a bridge string. `None` when the bridge returned null.
fn take(p: *mut c_char) -> Option<Vec<u8>> {
    if p.is_null() {
        return None;
    }
    // SAFETY: bridge returns a live strdup allocation with a terminating NUL; copy before free.
    let bytes = unsafe { CStr::from_ptr(p).to_bytes().to_vec() };
    // SAFETY: this allocation belongs to the bridge and is released exactly once.
    unsafe { rs_capture_free(p.cast()) };
    Some(bytes)
}
fn decode<T: serde::de::DeserializeOwned>(p: *mut c_char) -> Result<T, String> {
    let Some(bytes) = take(p) else {
        return Err(
            "macOS reported no active displays (CGGetActiveDisplayList failed or listed none)"
                .into(),
        );
    };
    if let Some(error) = macos_facts::bridge_error(&bytes) {
        return Err(error.to_string());
    }
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
fn window_snapshot() -> Result<WindowSnapshot, WindowListError> {
    // SAFETY: no inputs; the returned allocation is copied and freed by `take`.
    let bytes = take(unsafe { rs_capture_windows() }).ok_or(WindowListError::BridgeNoData)?;
    macos_facts::parse_windows(&bytes)
}
/// The visible windows with how they were obtained, or why the list is unknown. `needs` says
/// which rule kinds are configured (see [`macos_facts::classify_windows`]).
pub fn window_list(needs: RuleNeeds) -> Result<WindowList, WindowListError> {
    static OWN: OnceLock<OwnIdentity> = OnceLock::new();
    let own = OWN.get_or_init(OwnIdentity::current);
    macos_facts::classify_windows(&window_snapshot()?, needs, own)
}
pub fn visible_windows(needs: RuleNeeds) -> Result<Vec<VisibleWindow>, WindowListError> {
    window_list(needs).map(|list| list.windows)
}
/// Raw session facts (for `doctor`), or why they could not be read.
pub fn session_facts() -> Result<SessionFacts, SessionUnknown> {
    // SAFETY: no inputs; the returned allocation is copied and freed by `take`.
    let bytes = take(unsafe { rs_capture_session() }).ok_or(SessionUnknown::BridgeNoData)?;
    macos_facts::parse_session(&bytes)
}
/// Milliseconds since the last input, or why the session is unknown (locked, not on the console,
/// login window showing): unknown means do not record.
pub fn session_idle() -> Result<u64, SessionUnknown> {
    macos_facts::classify_session(&session_facts()?)
}
pub fn idle_millis() -> Option<u64> {
    session_idle().ok()
}
/// One frame of display `id`. Refused, before and after the capture, unless the session is
/// active (the lock screen must never be captured, nor a frame taken just as it appeared).
pub fn capture(id: u32) -> Result<BgraFrame, String> {
    session_idle().map_err(|why| format!("not capturing: {why}"))?;
    let (mut width, mut height, mut length, mut reason, mut os_code) = (0, 0, 0, 0, 0);
    // SAFETY: output pointers refer to live locals for the duration of this synchronous call.
    let p = unsafe {
        rs_capture_frame(
            id,
            &mut width,
            &mut height,
            &mut length,
            &mut reason,
            &mut os_code,
        )
    };
    if p.is_null() {
        return Err(macos_facts::frame_failure(reason, os_code as i64));
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
    // The session may have locked while the screenshot was taken: drop the frame, never store it.
    session_idle().map_err(|why| format!("frame discarded: {why}"))?;
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
        assert!(super::take(std::ptr::null_mut()).is_none());
    }
}
