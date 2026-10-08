//! Monitor enumeration and process DPI awareness.

use crate::error::{CaptureError, Result};
use crate::wide::from_wide;
use rsrewind_core::MonitorInfo;
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    GetDpiAwarenessContextForProcess, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use windows::core::BOOL;

/// An `HMONITOR`, stored as an integer so it is `Send`/`Sync`.
///
/// A monitor handle is an opaque identifier, not a pointer we dereference, so moving it between
/// threads is sound. It is only valid until the next display topology change (hotplug, mode
/// change, sleep/wake); persist `MonitorInfo::device_name` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonitorHandle(usize);

impl MonitorHandle {
    pub(crate) fn from_hmonitor(handle: HMONITOR) -> Self {
        Self(handle.0 as usize)
    }

    /// The handle as an opaque integer, for callers that key monitors without naming Windows
    /// types (the recorder's platform seam). Round-trips through [`Self::from_raw`].
    pub fn to_raw(self) -> usize {
        self.0
    }

    /// Rebuilds a handle from [`Self::to_raw`]. Sound for any value: the handle is never
    /// dereferenced, and Windows rejects a stale or invented one with an error.
    pub fn from_raw(raw: usize) -> Self {
        Self(raw)
    }

    pub(crate) fn hmonitor(self) -> HMONITOR {
        HMONITOR(self.0 as *mut core::ffi::c_void)
    }
}

/// Result of [`enable_dpi_awareness`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpiAwareness {
    /// This call made the process per-monitor-v2 aware.
    Enabled,
    /// Awareness had already been set (manifest or an earlier call) to per-monitor-v2.
    AlreadyPerMonitorV2,
    /// Awareness had already been set to something else and cannot be changed any more.
    /// Coordinates from `monitors()` and window rects may then be virtualised (scaled).
    AlreadySetOther,
}

/// Makes the process per-monitor-v2 DPI aware. Call first thing, before any window or capture
/// API, because awareness can only be set once per process. Without it, Windows reports scaled
/// (virtualised) monitor and window rectangles on high-DPI displays and they would not line up
/// with the physical-pixel frames WGC produces.
pub fn enable_dpi_awareness() -> Result<DpiAwareness> {
    // SAFETY: plain Win32 call with a predefined pseudo-handle constant.
    let set = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    match set {
        Ok(()) => Ok(DpiAwareness::Enabled),
        // Documented: ERROR_ACCESS_DENIED when awareness was already set.
        Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => {
            // SAFETY: a null handle means the current process; returns a context value, never fails.
            let current = unsafe { GetDpiAwarenessContextForProcess(HANDLE::default()) };
            // SAFETY: both arguments are valid DPI_AWARENESS_CONTEXT values.
            let same = unsafe {
                AreDpiAwarenessContextsEqual(current, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            };
            if same.as_bool() {
                Ok(DpiAwareness::AlreadyPerMonitorV2)
            } else {
                tracing::warn!(
                    "process DPI awareness was already set to something other than \
                     per-monitor-v2; monitor/window coordinates may be scaled"
                );
                Ok(DpiAwareness::AlreadySetOther)
            }
        }
        Err(error) => Err(CaptureError::win("SetProcessDpiAwarenessContext", error)),
    }
}

/// Every attached monitor, primary included, in `EnumDisplayMonitors` order.
pub fn monitors() -> Result<Vec<(MonitorHandle, MonitorInfo)>> {
    let mut handles: Vec<HMONITOR> = Vec::new();

    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // SAFETY: `data` is the `&mut Vec<HMONITOR>` passed below, which outlives the
        // synchronous EnumDisplayMonitors call and is not otherwise borrowed during it.
        let handles = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        handles.push(monitor);
        BOOL::from(true)
    }

    // SAFETY: the callback only touches the Vec through the pointer we pass, and the call is
    // synchronous, so the pointer stays valid for its whole duration.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        )
    };
    if !ok.as_bool() {
        return Err(CaptureError::win(
            "EnumDisplayMonitors",
            windows::core::Error::from_thread(),
        ));
    }

    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        match monitor_info(handle) {
            Some(info) => out.push((MonitorHandle::from_hmonitor(handle), info)),
            // A monitor can vanish between enumeration and query; skip it rather than fail.
            None => tracing::debug!("GetMonitorInfoW failed for an enumerated monitor; skipped"),
        }
    }
    Ok(out)
}

/// Device name (`\\.\DISPLAYn`) and geometry of one monitor, or `None` if it disappeared.
pub(crate) fn monitor_info(handle: HMONITOR) -> Option<MonitorInfo> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: `info` is a MONITORINFOEXW with cbSize set accordingly, which is how the API is
    // told it may write the trailing device-name field; the pointer is valid for the call.
    let ok = unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo) };
    if !ok.as_bool() {
        return None;
    }
    let rect = info.monitorInfo.rcMonitor;
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: valid out-pointers for the duration of the call.
    let dpi = match unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } {
        Ok(()) if dpi_x > 0 => dpi_x,
        _ => {
            tracing::debug!("GetDpiForMonitor failed; assuming 96 DPI");
            96
        }
    };
    Some(MonitorInfo {
        device_name: from_wide(&info.szDevice),
        left: rect.left,
        top: rect.top,
        width: rect.right.saturating_sub(rect.left).max(0) as u32,
        height: rect.bottom.saturating_sub(rect.top).max(0) as u32,
        dpi,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}
