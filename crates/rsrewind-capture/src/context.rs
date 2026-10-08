//! Pure, platform-independent shapes of desktop context: window rectangles and the visible
//! windows a privacy decision is made over. The Windows code that fills them lives in `window.rs`;
//! keeping the shapes here lets the recorder loop (and its tests) build on every platform.

use rsrewind_core::{ApplicationContext, MonitorInfo, WindowContext};

/// Process name used when neither the image path nor a process snapshot names the process
/// (in practice: it exited between the window query and the process query).
pub const UNKNOWN_PROCESS: &str = "unknown";

/// A window rectangle in virtual-desktop physical pixels (requires per-monitor-v2 awareness,
/// on Windows, see `enable_dpi_awareness`). `right`/`bottom` are exclusive.
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
}
