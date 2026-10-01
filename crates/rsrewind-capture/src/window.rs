//! Foreground focus, visible top-level windows and the processes behind them.
//!
//! Nothing here can fail a capture tick: every Windows call degrades to "less information"
//! (an `exe_path` of `None`, a process name from a fallback, an empty title) rather than an error.

use crate::monitor::monitor_info;
use crate::wide::{file_name, from_wide};
use rsrewind_core::{ApplicationContext, FocusContext, MonitorInfo, WindowContext};
use std::collections::HashMap;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, HWND, LPARAM, RECT,
};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
};
use windows::core::{BOOL, PWSTR};

/// Process name used when neither the image path nor a process snapshot names the process
/// (in practice: it exited between the window query and the process query).
pub const UNKNOWN_PROCESS: &str = "unknown";

/// A window rectangle in virtual-desktop physical pixels (requires per-monitor-v2 awareness,
/// see [`crate::enable_dpi_awareness`]). `right`/`bottom` are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRect {
    pub fn width(&self) -> u32 {
        (i64::from(self.right) - i64::from(self.left)).clamp(0, i64::from(u32::MAX)) as u32
    }

    pub fn height(&self) -> u32 {
        (i64::from(self.bottom) - i64::from(self.top)).clamp(0, i64::from(u32::MAX)) as u32
    }

    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// Area of overlap with `monitor`, in pixels. A window spanning two monitors overlaps both;
    /// privacy checks should use this rather than only [`VisibleWindow::monitor`].
    pub fn overlap_area(&self, monitor: &MonitorInfo) -> u64 {
        let m_left = i64::from(monitor.left);
        let m_top = i64::from(monitor.top);
        let m_right = m_left + i64::from(monitor.width);
        let m_bottom = m_top + i64::from(monitor.height);
        let w = (i64::from(self.right).min(m_right) - i64::from(self.left).max(m_left)).max(0);
        let h = (i64::from(self.bottom).min(m_bottom) - i64::from(self.top).max(m_top)).max(0);
        (w * h) as u64
    }

    pub fn intersects(&self, monitor: &MonitorInfo) -> bool {
        self.overlap_area(monitor) > 0
    }

    fn from_rect(rect: RECT) -> Self {
        Self {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }
}

/// A top-level window a user could currently see (visible, not minimised, not cloaked, non-zero
/// area). Used for privacy: a monitor is excluded if *any* visible window on it matches a rule.
#[derive(Clone, PartialEq, Eq)]
pub struct VisibleWindow {
    /// Executable file name, e.g. `1Password.exe`, or [`UNKNOWN_PROCESS`].
    pub process_name: String,
    pub title: String,
    pub rect: ScreenRect,
    /// Device name (`\\.\DISPLAYn`) of the monitor the window mostly lies on
    /// (`MonitorFromWindow(MONITOR_DEFAULTTONEAREST)`), empty if it could not be resolved.
    pub monitor: String,
    pub pid: u32,
}

impl VisibleWindow {
    /// The shapes `PrivacyPolicy::evaluate` takes.
    pub fn application_context(&self) -> ApplicationContext {
        ApplicationContext {
            process_name: self.process_name.clone(),
            exe_path: None,
        }
    }

    pub fn window_context(&self) -> WindowContext {
        WindowContext {
            title: self.title.clone(),
            class_name: None,
        }
    }
}

impl std::fmt::Debug for VisibleWindow {
    // Titles are captured content: never let a `{:?}` put one into a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VisibleWindow")
            .field("process_name", &self.process_name)
            .field(
                "title",
                &format_args!("<{} chars>", self.title.chars().count()),
            )
            .field("rect", &self.rect)
            .field("monitor", &self.monitor)
            .field("pid", &self.pid)
            .finish()
    }
}

/// Who has focus right now. `None` when there is no foreground window (desktop switching, the
/// secure desktop / UAC prompt, a locked workstation) or it vanished mid-query.
pub fn foreground() -> Option<FocusContext> {
    // SAFETY: no arguments; returns a possibly-null handle.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return None;
    }
    let pid = window_pid(hwnd)?;
    let mut resolver = ProcessResolver::default();
    let application = resolver.resolve(pid);
    Some(FocusContext {
        application,
        window: WindowContext {
            title: window_title(hwnd),
            class_name: window_class(hwnd),
        },
        pid,
    })
}

/// Visible, uncloaked, non-minimised top-level windows with non-zero area, in Z order (topmost
/// first), excluding this process's own windows.
pub fn visible_windows() -> Vec<VisibleWindow> {
    let own_pid = std::process::id();
    let mut resolver = ProcessResolver::default();
    let mut monitor_names: HashMap<usize, String> = HashMap::new();
    let mut out = Vec::new();

    for hwnd in top_level_windows() {
        // SAFETY: plain query on a window handle; a stale handle just returns FALSE.
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            continue;
        }
        // SAFETY: as above.
        if unsafe { IsIconic(hwnd) }.as_bool() {
            continue;
        }
        if is_cloaked(hwnd) {
            continue;
        }
        let Some(pid) = window_pid(hwnd) else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let Some(rect) = window_rect(hwnd) else {
            continue;
        };
        if rect.is_empty() {
            continue;
        }
        // SAFETY: plain query; DEFAULTTONEAREST never returns null for a valid window.
        let hmonitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        let monitor = monitor_names
            .entry(hmonitor.0 as usize)
            .or_insert_with(|| {
                monitor_info(hmonitor)
                    .map(|info| info.device_name)
                    .unwrap_or_default()
            })
            .clone();
        out.push(VisibleWindow {
            process_name: resolver.resolve(pid).process_name,
            title: window_title(hwnd),
            rect,
            monitor,
            pid,
        });
    }
    out
}

fn top_level_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `&mut Vec<HWND>` passed below; EnumWindows is synchronous and
        // nothing else borrows the Vec while it runs.
        let handles = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        handles.push(hwnd);
        BOOL::from(true)
    }
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the pointer refers to a live local for the duration of this synchronous call.
    let result = unsafe {
        EnumWindows(
            Some(collect),
            LPARAM(&mut handles as *mut Vec<HWND> as isize),
        )
    };
    if let Err(error) = result {
        // Partial list is still useful; privacy errs towards "more windows", which we have.
        tracing::debug!(%error, collected = handles.len(), "EnumWindows failed");
    }
    handles
}

fn window_pid(hwnd: HWND) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: valid out-pointer for the call; a stale handle returns 0 and leaves pid at 0.
    let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (thread != 0 && pid != 0).then_some(pid)
}

fn window_title(hwnd: HWND) -> String {
    // For windows of *other* processes GetWindowTextW reads the cached caption and never sends
    // WM_GETTEXT, so a hung application cannot block the capture tick.
    // SAFETY: plain query.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; len as usize + 1];
    // SAFETY: the slice is valid and writable; the API bounds writes by its length.
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    buffer.truncate(copied.max(0) as usize);
    from_wide(&buffer)
}

fn window_class(hwnd: HWND) -> Option<String> {
    // Window class names are limited to 256 characters.
    let mut buffer = [0u16; 257];
    // SAFETY: the slice is valid and writable; the API bounds writes by its length.
    let copied = unsafe { GetClassNameW(hwnd, &mut buffer) };
    (copied > 0).then(|| from_wide(&buffer[..copied as usize]))
}

fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: DWMWA_CLOAKED writes a DWORD; we pass a u32 and its exact size.
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    // On failure treat as visible: for privacy it is safer to over-report windows.
    result.is_ok() && cloaked != 0
}

fn window_rect(hwnd: HWND) -> Option<ScreenRect> {
    let mut rect = RECT::default();
    // The extended frame bounds exclude the invisible resize borders Windows 10+ adds around
    // windows, so they reflect what is actually painted. Fall back to the window rect.
    // SAFETY: DWMWA_EXTENDED_FRAME_BOUNDS writes a RECT; we pass a RECT and its exact size.
    let dwm = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
    };
    if dwm.is_ok() {
        return Some(ScreenRect::from_rect(rect));
    }
    // SAFETY: valid out-pointer for the call.
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some(ScreenRect::from_rect(rect))
}

/// Closes a process handle on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own this handle (returned by OpenProcess / CreateToolhelp32Snapshot) and
        // close it exactly once.
        if let Err(error) = unsafe { CloseHandle(self.0) } {
            tracing::debug!(%error, "CloseHandle failed");
        }
    }
}

/// Resolves pid → process name with per-call caching.
///
/// Order: `QueryFullProcessImageNameW` (gives the full path) → a Toolhelp process snapshot (gives
/// the exe name for *any* process, including elevated/protected ones we cannot open, but no
/// path) → [`UNKNOWN_PROCESS`]. The snapshot is taken at most once per resolver and only when
/// needed. We prefer a real name over "unknown" because process-name privacy rules
/// (`1Password.exe`, `consent.exe`) must still match elevated windows.
#[derive(Default)]
struct ProcessResolver {
    cache: HashMap<u32, ApplicationContext>,
    snapshot: Option<HashMap<u32, String>>,
}

impl ProcessResolver {
    fn resolve(&mut self, pid: u32) -> ApplicationContext {
        if let Some(hit) = self.cache.get(&pid) {
            return hit.clone();
        }
        let context = match image_path(pid) {
            Some(path) => ApplicationContext {
                process_name: file_name(&path).to_string(),
                exe_path: Some(path),
            },
            None => {
                let snapshot = self.snapshot.get_or_insert_with(process_snapshot);
                ApplicationContext {
                    process_name: snapshot
                        .get(&pid)
                        .cloned()
                        .unwrap_or_else(|| UNKNOWN_PROCESS.to_string()),
                    exe_path: None,
                }
            }
        };
        self.cache.insert(pid, context.clone());
        context
    }
}

fn image_path(pid: u32) -> Option<String> {
    // SAFETY: plain call; failure (access denied, exited) is reported via Result.
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => OwnedHandle(handle),
        Err(error) => {
            tracing::debug!(pid, %error, "OpenProcess failed; falling back to process snapshot");
            return None;
        }
    };
    // MAX_PATH covers nearly everything; retry once at the NT path limit for long paths.
    for capacity in [512usize, 32_768] {
        let mut buffer = vec![0u16; capacity];
        let mut size = capacity as u32;
        // SAFETY: `buffer` is writable for `size` u16s; `size` is updated to the written length.
        let result = unsafe {
            QueryFullProcessImageNameW(
                handle.0,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut size,
            )
        };
        match result {
            Ok(()) => return Some(from_wide(&buffer[..size as usize])),
            Err(error) if error.code() == ERROR_INSUFFICIENT_BUFFER.to_hresult() => continue,
            Err(error) => {
                tracing::debug!(pid, %error, "QueryFullProcessImageNameW failed");
                return None;
            }
        }
    }
    None
}

fn process_snapshot() -> HashMap<u32, String> {
    let mut names = HashMap::new();
    // SAFETY: plain call; returns an owned handle or an error.
    let snapshot = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) } {
        Ok(handle) => OwnedHandle(handle),
        Err(error) => {
            tracing::debug!(%error, "CreateToolhelp32Snapshot failed");
            return names;
        }
    };
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: `entry` is a PROCESSENTRY32W with dwSize set, valid for the call.
    let mut next = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    while next.is_ok() {
        names.insert(entry.th32ProcessID, from_wide(&entry.szExeFile));
        // SAFETY: as above.
        next = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(left: i32, top: i32, width: u32, height: u32) -> MonitorInfo {
        MonitorInfo {
            device_name: r"\\.\DISPLAY1".into(),
            left,
            top,
            width,
            height,
            dpi: 96,
            primary: true,
        }
    }

    #[test]
    fn rect_geometry() {
        let r = ScreenRect {
            left: -10,
            top: 0,
            right: 10,
            bottom: 5,
        };
        assert_eq!((r.width(), r.height()), (20, 5));
        assert!(!r.is_empty());
        let inverted = ScreenRect {
            left: 10,
            top: 0,
            right: -10,
            bottom: 5,
        };
        assert!(inverted.is_empty());
    }

    #[test]
    fn overlap_spans_monitors() {
        let left = monitor(0, 0, 1920, 1080);
        let right = monitor(1920, 0, 2560, 1440);
        let window = ScreenRect {
            left: 1820,
            top: 100,
            right: 2020,
            bottom: 200,
        };
        assert_eq!(window.overlap_area(&left), 100 * 100);
        assert_eq!(window.overlap_area(&right), 100 * 100);
        let off = ScreenRect {
            left: 5000,
            top: 0,
            right: 5100,
            bottom: 100,
        };
        assert!(!off.intersects(&left));
        // Touching edges is not overlap.
        let touching = ScreenRect {
            left: 1920,
            top: 0,
            right: 2000,
            bottom: 10,
        };
        assert!(!touching.intersects(&left));
    }

    #[test]
    fn debug_hides_title() {
        let window = VisibleWindow {
            process_name: "bank.exe".into(),
            title: "Account 12345".into(),
            rect: ScreenRect {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
            monitor: String::new(),
            pid: 1,
        };
        let text = format!("{window:?}");
        assert!(!text.contains("12345"));
        assert!(text.contains("bank.exe"));
    }

    // These touch the live desktop but only read; they hold on any interactive Windows session.
    #[test]
    fn own_process_resolves_with_path() {
        let mut resolver = ProcessResolver::default();
        let app = resolver.resolve(std::process::id());
        assert!(app.exe_path.is_some());
        assert!(app.process_name.to_ascii_lowercase().ends_with(".exe"));
    }

    #[test]
    fn system_process_falls_back_to_snapshot() {
        // PID 4 ("System") cannot be opened for an image path but is in every snapshot.
        let mut resolver = ProcessResolver::default();
        let app = resolver.resolve(4);
        assert_ne!(app.process_name, UNKNOWN_PROCESS);
    }

    #[test]
    fn visible_windows_exclude_own_process() {
        let own = std::process::id();
        assert!(visible_windows().iter().all(|w| w.pid != own));
    }
}
