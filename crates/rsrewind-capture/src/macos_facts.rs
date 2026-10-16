//! What the macOS bridge (`macos.swift`) observes, and what rsRewind concludes from it.
//!
//! The Swift side only reports raw CoreGraphics facts as JSON (is there a title, could the owner be
//! resolved, what layer, what bounds). Every decision about those facts lives here, in portable
//! Rust, so it is unit-tested on every platform even though only a Mac produces the input:
//!
//! - which windows count for the any-visible-window privacy rule, and on which display;
//! - when an odd window makes the whole list *unknown* (fail closed) instead of being tolerated;
//! - when the session counts as unknown (locked, not on the console, login window showing);
//! - what a failed frame capture means.
//!
//! Window titles are captured content: no error, `Display` or `Debug` output here contains one.
//! Process names appear only in [`VisibleWindow`]s handed to the privacy policy.

use crate::context::{ScreenRect, UNKNOWN_PROCESS, VisibleWindow};
use rsrewind_core::PrivacyPolicy;
use serde::Deserialize;
use std::fmt;

/// Which kinds of exclusion rule are configured. Some odd windows are tolerated only when no rule
/// of the kind they would defeat exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuleNeeds {
    /// At least one non-blank `excluded_processes` entry.
    pub process_rules: bool,
    /// At least one non-blank `excluded_title_patterns` entry.
    pub title_rules: bool,
}

impl RuleNeeds {
    pub fn of(policy: &PrivacyPolicy) -> Self {
        let any = |rules: &[String]| rules.iter().any(|r| !r.trim().is_empty());
        Self {
            process_rules: any(&policy.excluded_processes),
            title_rules: any(&policy.excluded_title_patterns),
        }
    }
}

// ----- window list ---------------------------------------------------------------------------

/// `rs_capture_windows` output: either `{"error": code}` or the displays and every on-screen
/// window CoreGraphics listed, in its order (frontmost first).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WindowSnapshot {
    pub error: Option<String>,
    pub displays: Vec<RawDisplay>,
    pub windows: Vec<RawWindow>,
}

/// One active display: its rectangle in global points and its size in pixels. `x` is the
/// display's left edge in rsRewind's pixel layout (displays side by side, as `rs_capture_monitors`
/// reports them).
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawDisplay {
    pub x: i64,
    pub min_x: f64,
    pub min_y: f64,
    pub width_pt: f64,
    pub height_pt: f64,
    pub width_px: i64,
    pub height_px: i64,
}

/// A rectangle in global points (`kCGWindowBounds`).
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct RawRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Raw facts about one CoreGraphics window. Every field is optional because CoreGraphics may
/// leave any of them out; this module decides what a missing one means.
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub struct RawWindow {
    /// `kCGWindowOwnerPID`.
    pub pid: Option<i64>,
    /// `NSRunningApplication.executableURL.lastPathComponent`, when the owner resolves.
    pub executable: Option<String>,
    /// The owner resolved and its activation policy is `.regular` (a Dock app with ordinary
    /// windows, which is what title rules are written for). `None` when it did not resolve.
    pub regular: Option<bool>,
    /// `kCGWindowOwnerName`: the owner's display name, present even for processes
    /// `NSRunningApplication` cannot resolve.
    pub owner_name: Option<String>,
    /// `kCGWindowName`. Absent for untitled windows (and for every other process's windows when
    /// Screen Recording is not granted).
    pub title: Option<String>,
    /// `kCGWindowLayer`; absent or non-integer is tolerated.
    pub layer: Option<i64>,
    pub bounds: Option<RawRect>,
}

impl fmt::Debug for RawWindow {
    // Titles are captured content: never let a `{:?}` put one into a log line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawWindow")
            .field("pid", &self.pid)
            .field("executable", &self.executable)
            .field("regular", &self.regular)
            .field("owner_name", &self.owner_name)
            .field(
                "title",
                &self
                    .title
                    .as_ref()
                    .map(|t| format!("<{} chars>", t.chars().count())),
            )
            .field("layer", &self.layer)
            .field("bounds", &self.bounds)
            .finish()
    }
}

/// Why the window list is unknown. Recording stops on every monitor while it is. Every message is
/// a fixed sentence or a count, never captured content, so it is safe to log and to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowListError {
    /// `CGPreflightScreenCaptureAccess` said no.
    NoPermission,
    /// `CGGetActiveDisplayList` failed or listed nothing.
    NoDisplays,
    /// `CGWindowListCopyWindowInfo` returned nothing.
    ListUnavailable,
    /// The bridge returned no data at all (JSON serialization failed in Swift).
    BridgeNoData,
    /// The bridge's data did not parse.
    Malformed,
    /// The bridge reported an error code this build does not know (sanitized).
    Bridge(String),
    /// A display has no usable size, so windows cannot be placed on it.
    BadDisplayGeometry,
    /// A window on screen has neither a resolvable owner nor an owner name while process rules
    /// exist: no process rule could be checked against it.
    OwnerUnknown { windows: usize },
    /// Title rules exist, regular applications have windows on screen, and not one of them reports
    /// a title: titles are evidently being withheld, so no title rule could match.
    TitlesUnavailable { untitled: usize },
}

impl WindowListError {
    /// A short stable identifier, for counters, logs and `doctor`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoPermission => "no-permission",
            Self::NoDisplays => "no-displays",
            Self::ListUnavailable => "list-unavailable",
            Self::BridgeNoData => "bridge-no-data",
            Self::Malformed => "malformed",
            Self::Bridge(_) => "bridge-error",
            Self::BadDisplayGeometry => "bad-display-geometry",
            Self::OwnerUnknown { .. } => "owner-unknown",
            Self::TitlesUnavailable { .. } => "titles-unavailable",
        }
    }
}

impl fmt::Display for WindowListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "window list unknown ({}): ", self.code())?;
        match self {
            Self::NoPermission => {
                f.write_str("Screen Recording permission is not granted to this process")
            }
            Self::NoDisplays => f.write_str("macOS listed no active displays"),
            Self::ListUnavailable => f.write_str("CGWindowListCopyWindowInfo returned nothing"),
            Self::BridgeNoData => f.write_str("the native bridge returned no data"),
            Self::Malformed => f.write_str("the native bridge returned unreadable data"),
            Self::Bridge(code) => write!(f, "the native bridge reported {code}"),
            Self::BadDisplayGeometry => f.write_str("a display reported no usable size"),
            Self::OwnerUnknown { windows } => write!(
                f,
                "{windows} on-screen window(s) have no identifiable owner, so application exclusion rules cannot be checked"
            ),
            Self::TitlesUnavailable { untitled } => write!(
                f,
                "none of the {untitled} on-screen application window(s) reports a title, so window-title exclusion rules cannot be checked"
            ),
        }
    }
}

impl std::error::Error for WindowListError {}

/// Counts describing one enumeration, for `doctor` and debug logs. Counts only, no content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowStats {
    /// Windows CoreGraphics listed.
    pub listed: usize,
    /// Listed windows with usable bounds overlapping at least one display: the ones that count.
    pub on_screen: usize,
    /// On-screen windows without a (non-empty) title. Title rules cannot match these.
    pub untitled: usize,
    /// ... of which belong to regular applications (other than this process).
    pub untitled_regular: usize,
    /// On-screen windows of regular applications (other than this process) with a title.
    pub titled_regular: usize,
    /// On-screen windows whose owner `NSRunningApplication` could not resolve; their process
    /// name is `kCGWindowOwnerName`.
    pub owner_fallback: usize,
    /// On-screen windows with neither a resolved owner nor an owner name ([`UNKNOWN_PROCESS`]).
    pub owner_unknown: usize,
    /// On-screen windows without an integer layer (kept).
    pub layer_unknown: usize,
    /// Listed windows skipped because they have no usable bounds (they cannot be placed on any
    /// display).
    pub no_bounds: usize,
    /// Listed windows with bounds that touch no active display.
    pub off_display: usize,
}

/// The windows the privacy rules are evaluated over, with how they were obtained.
#[derive(Debug, Clone, Default)]
pub struct WindowList {
    /// One entry per (window, display it touches), frontmost first. A window whose resolved
    /// executable name differs from its owner name appears a second time, right after the first,
    /// under the owner name, so an application rule written either way still matches.
    pub windows: Vec<VisibleWindow>,
    pub stats: WindowStats,
}

pub fn parse_windows(json: &[u8]) -> Result<WindowSnapshot, WindowListError> {
    serde_json::from_slice(json).map_err(|_| WindowListError::Malformed)
}

fn known_bridge_error(code: &str) -> WindowListError {
    match code {
        "no-permission" => WindowListError::NoPermission,
        "no-displays" => WindowListError::NoDisplays,
        "list-unavailable" => WindowListError::ListUnavailable,
        other => WindowListError::Bridge(
            other
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .take(40)
                .collect(),
        ),
    }
}

fn usable(rect: &RawRect) -> bool {
    [rect.x, rect.y, rect.width, rect.height]
        .iter()
        .all(|v| v.is_finite())
        && rect.width > 0.0
        && rect.height > 0.0
}

fn display_usable(d: &RawDisplay) -> bool {
    [d.min_x, d.min_y, d.width_pt, d.height_pt]
        .iter()
        .all(|v| v.is_finite())
        && d.width_pt > 0.0
        && d.height_pt > 0.0
        && d.width_px > 0
        && d.height_px > 0
}

fn to_i32(v: f64) -> i32 {
    // `as` saturates and maps NaN to 0; inputs are finite here.
    v as i32
}

/// The part of `window` on `display`, in rsRewind's pixel layout, or `None` if they do not
/// overlap. Same arithmetic the Swift bridge used before (floor the near edges, ceil the far
/// ones), so a window is never shrunk off a monitor it touches.
pub fn clip_to_display(window: &RawRect, display: &RawDisplay) -> Option<ScreenRect> {
    let (dx0, dy0) = (display.min_x, display.min_y);
    let (dx1, dy1) = (dx0 + display.width_pt, dy0 + display.height_pt);
    let x0 = window.x.max(dx0);
    let y0 = window.y.max(dy0);
    let x1 = (window.x + window.width).min(dx1);
    let y1 = (window.y + window.height).min(dy1);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let sx = display.width_px as f64 / display.width_pt;
    let sy = display.height_px as f64 / display.height_pt;
    let left = display.x as f64;
    Some(ScreenRect {
        left: to_i32(left + ((x0 - dx0) * sx).floor()),
        top: to_i32(((y0 - dy0) * sy).floor()),
        right: to_i32(left + ((x1 - dx0) * sx).ceil()),
        bottom: to_i32(((y1 - dy0) * sy).ceil()),
    })
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// Decides which windows count, tolerating the odd windows every real Mac has (untitled
/// overlays, system processes `NSRunningApplication` cannot resolve, missing layers) and failing
/// closed only where a configured rule could otherwise be defeated:
///
/// - no title: kept with an empty title (as on Windows, where an untitled window has title `""`);
///   but if title rules exist and *no* regular application's window on screen has a title, titles
///   are being withheld and the whole list is unknown ([`WindowListError::TitlesUnavailable`]);
/// - owner not resolvable: process name falls back to `kCGWindowOwnerName`; with neither, the
///   name is [`UNKNOWN_PROCESS`], and if process rules exist the whole list is unknown
///   ([`WindowListError::OwnerUnknown`]);
/// - layer missing or not an integer: kept;
/// - no usable bounds: skipped and counted (it cannot be placed on any display).
///
/// `own_pid` (this process) is left out of the title-availability evidence: a process always sees
/// its own titles, so they prove nothing about everyone else's.
pub fn classify_windows(
    snapshot: &WindowSnapshot,
    needs: RuleNeeds,
    own_pid: u32,
) -> Result<WindowList, WindowListError> {
    if let Some(code) = &snapshot.error {
        return Err(known_bridge_error(code));
    }
    if snapshot.displays.is_empty() {
        return Err(WindowListError::NoDisplays);
    }
    if !snapshot.displays.iter().all(display_usable) {
        return Err(WindowListError::BadDisplayGeometry);
    }
    let mut stats = WindowStats {
        listed: snapshot.windows.len(),
        ..WindowStats::default()
    };
    let mut windows = Vec::new();
    for raw in &snapshot.windows {
        let Some(bounds) = raw.bounds.filter(usable) else {
            stats.no_bounds += 1;
            continue;
        };
        let rects: Vec<ScreenRect> = snapshot
            .displays
            .iter()
            .filter_map(|d| clip_to_display(&bounds, d))
            .collect();
        if rects.is_empty() {
            stats.off_display += 1;
            continue;
        }
        stats.on_screen += 1;

        let executable = non_blank(raw.executable.as_deref());
        let owner = non_blank(raw.owner_name.as_deref());
        let process = match (executable, owner) {
            (Some(exe), _) => exe,
            (None, Some(owner)) => {
                stats.owner_fallback += 1;
                owner
            }
            (None, None) => {
                stats.owner_unknown += 1;
                UNKNOWN_PROCESS
            }
        };
        // Second name to check rules against: the owner's display name, when it differs from the
        // executable (e.g. an Electron app whose executable is not its name).
        let alias = match (executable, owner) {
            (Some(exe), Some(owner)) if !exe.eq_ignore_ascii_case(owner) => Some(owner),
            _ => None,
        };
        let title = raw.title.as_deref().unwrap_or("");
        let titled = !title.trim().is_empty();
        let pid = raw.pid.and_then(|p| u32::try_from(p).ok()).unwrap_or(0);
        let counts_as_evidence = raw.regular == Some(true) && pid != own_pid;
        if !titled {
            stats.untitled += 1;
            if counts_as_evidence {
                stats.untitled_regular += 1;
            }
        } else if counts_as_evidence {
            stats.titled_regular += 1;
        }
        if raw.layer.is_none() {
            stats.layer_unknown += 1;
        }

        for rect in rects {
            for name in std::iter::once(process).chain(alias) {
                windows.push(VisibleWindow {
                    process_name: name.to_string(),
                    title: title.to_string(),
                    rect,
                    monitor: String::new(),
                    pid,
                });
            }
        }
    }
    if needs.process_rules && stats.owner_unknown > 0 {
        return Err(WindowListError::OwnerUnknown {
            windows: stats.owner_unknown,
        });
    }
    if needs.title_rules && stats.untitled_regular > 0 && stats.titled_regular == 0 {
        return Err(WindowListError::TitlesUnavailable {
            untitled: stats.untitled_regular,
        });
    }
    Ok(WindowList { windows, stats })
}

// ----- session and idle ----------------------------------------------------------------------

/// `rs_capture_session` output.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SessionFacts {
    /// `CGSessionCopyCurrentDictionary` returned a dictionary.
    pub have_session: bool,
    /// `kCGSessionOnConsoleKey`.
    pub on_console: Option<bool>,
    /// `kCGSessionLoginDoneKey`.
    pub login_done: Option<bool>,
    /// `CGSSessionScreenIsLocked` (undocumented key; present and true while the screen is locked).
    pub screen_locked: Option<bool>,
    /// The on-screen window list (all layers) could be read.
    pub windows_listed: bool,
    /// On-screen windows owned by `loginwindow` (lock screen, login and fast-user-switch UI).
    pub loginwindow: Vec<LoginWindowFact>,
    /// On-screen windows whose owner `NSRunningApplication` could not resolve. Informational: an
    /// unresolvable owner no longer makes the session unknown.
    pub unresolved_owners: u32,
    /// `CGEventSource.secondsSinceLastEventType`, when finite and non-negative.
    pub idle_seconds: Option<f64>,
}

/// Shape of one `loginwindow` window, for `doctor` (no content).
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct LoginWindowFact {
    pub layer: Option<i64>,
    pub alpha: Option<f64>,
    pub width: Option<f64>,
    pub height: Option<f64>,
}

/// Why idle time / session state is unknown. Unknown means nothing is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionUnknown {
    BridgeNoData,
    Malformed,
    /// `CGSessionCopyCurrentDictionary` returned nothing (no window-server session).
    NoSessionInfo,
    /// The session is not the one on the console (fast user switching, remote login).
    NotOnConsole,
    /// Login has not finished.
    LoginNotDone,
    /// The screen is locked.
    ScreenLocked,
    /// The window list could not be read, so the lock screen cannot be ruled out.
    WindowListUnavailable,
    /// `loginwindow` has windows on screen (lock screen, login or user-switch UI).
    LoginWindowVisible {
        windows: usize,
    },
    /// No idle time from `CGEventSource`.
    IdleUnavailable,
}

impl SessionUnknown {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BridgeNoData => "bridge-no-data",
            Self::Malformed => "malformed",
            Self::NoSessionInfo => "no-session",
            Self::NotOnConsole => "not-on-console",
            Self::LoginNotDone => "login-not-done",
            Self::ScreenLocked => "screen-locked",
            Self::WindowListUnavailable => "list-unavailable",
            Self::LoginWindowVisible { .. } => "loginwindow-visible",
            Self::IdleUnavailable => "idle-unavailable",
        }
    }
}

impl fmt::Display for SessionUnknown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session unknown ({}): ", self.code())?;
        match self {
            Self::BridgeNoData => f.write_str("the native bridge returned no data"),
            Self::Malformed => f.write_str("the native bridge returned unreadable data"),
            Self::NoSessionInfo => {
                f.write_str("macOS returned no window-server session for this process")
            }
            Self::NotOnConsole => f.write_str(
                "this login session is not the one on the console (switched away, or remote)",
            ),
            Self::LoginNotDone => f.write_str("login has not finished"),
            Self::ScreenLocked => f.write_str("the screen is locked"),
            Self::WindowListUnavailable => f.write_str(
                "the window list could not be read, so a lock screen cannot be ruled out",
            ),
            Self::LoginWindowVisible { windows } => write!(
                f,
                "{windows} loginwindow window(s) are on screen (lock, login or user-switch screen)"
            ),
            Self::IdleUnavailable => f.write_str("macOS did not report time since the last input"),
        }
    }
}

impl std::error::Error for SessionUnknown {}

pub fn parse_session(json: &[u8]) -> Result<SessionFacts, SessionUnknown> {
    serde_json::from_slice(json).map_err(|_| SessionUnknown::Malformed)
}

/// Milliseconds since the last input, or why that is unknown. Fails closed on everything that
/// could mean a locked or foreign session; an unresolvable window owner is no longer one of them.
pub fn classify_session(facts: &SessionFacts) -> Result<u64, SessionUnknown> {
    if !facts.have_session {
        return Err(SessionUnknown::NoSessionInfo);
    }
    if facts.on_console != Some(true) {
        return Err(SessionUnknown::NotOnConsole);
    }
    if facts.login_done != Some(true) {
        return Err(SessionUnknown::LoginNotDone);
    }
    if facts.screen_locked == Some(true) {
        return Err(SessionUnknown::ScreenLocked);
    }
    if !facts.windows_listed {
        return Err(SessionUnknown::WindowListUnavailable);
    }
    if !facts.loginwindow.is_empty() {
        return Err(SessionUnknown::LoginWindowVisible {
            windows: facts.loginwindow.len(),
        });
    }
    match facts.idle_seconds {
        Some(seconds) if seconds.is_finite() && seconds >= 0.0 => Ok((seconds * 1000.0) as u64),
        _ => Err(SessionUnknown::IdleUnavailable),
    }
}

// ----- frames --------------------------------------------------------------------------------

/// `rs_capture_frame` failure codes (its `reason` out-parameter).
pub mod frame_reason {
    pub const NONE: i32 = 0;
    pub const NO_PERMISSION: i32 = 1;
    pub const OS_TOO_OLD: i32 = 2;
    pub const SHAREABLE_CONTENT: i32 = 3;
    pub const DISPLAY_NOT_FOUND: i32 = 4;
    pub const SCREENSHOT: i32 = 5;
    pub const TIMEOUT: i32 = 6;
    pub const NO_IMAGE: i32 = 7;
    pub const BAD_SIZE: i32 = 8;
    pub const DRAW: i32 = 9;
}

/// A content-free sentence for a failed frame capture. `os_code` is the `NSError` code where the
/// bridge had one (0 otherwise).
pub fn frame_failure(reason: i32, os_code: i64) -> String {
    use frame_reason::*;
    let what = match reason {
        NO_PERMISSION => "Screen Recording permission is not granted to this process",
        OS_TOO_OLD => "ScreenCaptureKit screenshots need macOS 14 or newer",
        SHAREABLE_CONTENT => "ScreenCaptureKit could not list shareable content",
        DISPLAY_NOT_FOUND => "the display is not in ScreenCaptureKit's shareable content",
        SCREENSHOT => "ScreenCaptureKit failed to capture the screenshot",
        TIMEOUT => "ScreenCaptureKit did not answer within 15 s",
        NO_IMAGE => "ScreenCaptureKit returned no image",
        BAD_SIZE => "the captured image has an unusable size",
        DRAW => "the captured image could not be converted to BGRA",
        _ => "ScreenCaptureKit capture failed for an unknown reason",
    };
    if os_code != 0 {
        format!("frame capture failed (code {reason}): {what} (error {os_code})")
    } else {
        format!("frame capture failed (code {reason}): {what}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const BOTH: RuleNeeds = RuleNeeds {
        process_rules: true,
        title_rules: true,
    };
    const OWN_PID: u32 = 4242;

    /// A 1440x900-point display at 2x (2880x1800 px), and a second one to its right at 1x.
    fn displays() -> Vec<RawDisplay> {
        vec![
            RawDisplay {
                x: 0,
                min_x: 0.0,
                min_y: 0.0,
                width_pt: 1440.0,
                height_pt: 900.0,
                width_px: 2880,
                height_px: 1800,
            },
            RawDisplay {
                x: 2880,
                min_x: 1440.0,
                min_y: 0.0,
                width_pt: 1920.0,
                height_pt: 1080.0,
                width_px: 1920,
                height_px: 1080,
            },
        ]
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Option<RawRect> {
        Some(RawRect {
            x,
            y,
            width,
            height,
        })
    }

    /// An ordinary titled window of a regular application, on display 1.
    fn app(exe: &str, title: &str, pid: i64) -> RawWindow {
        RawWindow {
            pid: Some(pid),
            executable: Some(exe.into()),
            regular: Some(true),
            owner_name: Some(exe.into()),
            title: Some(title.into()),
            layer: Some(0),
            bounds: rect(100.0, 100.0, 800.0, 600.0),
        }
    }

    /// The kind of window every real Mac has on screen: owned by a process
    /// `NSRunningApplication` cannot resolve, no title, no integer layer (WindowServer's
    /// menu bar, Control Center items, MDM agents' overlays).
    fn overlay(owner: Option<&str>) -> RawWindow {
        RawWindow {
            pid: Some(77),
            executable: None,
            regular: None,
            owner_name: owner.map(String::from),
            title: None,
            layer: None,
            bounds: rect(0.0, 0.0, 1440.0, 24.0),
        }
    }

    fn snapshot(windows: Vec<RawWindow>) -> WindowSnapshot {
        WindowSnapshot {
            error: None,
            displays: displays(),
            windows,
        }
    }

    fn names(list: &WindowList) -> Vec<&str> {
        list.windows
            .iter()
            .map(|w| w.process_name.as_str())
            .collect()
    }

    #[test]
    fn odd_windows_no_longer_blank_the_whole_list() -> TestResult {
        // Before #33 any one of these made the bridge return nil for the whole list, and the
        // recorder then excluded every monitor on every tick.
        let snap = snapshot(vec![
            app("Safari", "Docs", 10),
            overlay(Some("Window Server")),
            RawWindow {
                layer: None,
                ..app("Terminal", "zsh", 11)
            },
        ]);
        let list = classify_windows(&snap, BOTH, OWN_PID)?;
        assert_eq!(names(&list), ["Safari", "Window Server", "Terminal"]);
        assert_eq!(list.stats.listed, 3);
        assert_eq!(list.stats.on_screen, 3);
        assert_eq!(list.stats.untitled, 1);
        assert_eq!(list.stats.owner_fallback, 1);
        assert_eq!(list.stats.layer_unknown, 2);
        // The untitled overlay is kept with an empty title, not dropped.
        assert_eq!(list.windows[1].title, "");
        Ok(())
    }

    #[test]
    fn application_rules_still_match_windows_named_by_the_owner_fallback() -> TestResult {
        let policy = PrivacyPolicy {
            excluded_processes: vec!["1Password".into()],
            ..PrivacyPolicy::default()
        };
        // An owner NSRunningApplication cannot resolve: the name comes from kCGWindowOwnerName.
        let vault = RawWindow {
            title: Some("Vault".into()),
            ..overlay(Some("1Password"))
        };
        let list = classify_windows(&snapshot(vec![vault]), RuleNeeds::of(&policy), OWN_PID)?;
        assert_eq!(list.stats.owner_fallback, 1);
        let excluded = list.windows.iter().any(|w| {
            policy
                .evaluate(Some(&w.application_context()), None)
                .is_excluded()
        });
        assert!(excluded, "fallback owner name must still trigger the rule");
        Ok(())
    }

    #[test]
    fn a_rule_matches_either_the_executable_or_the_owner_name() -> TestResult {
        // Executable "Electron", owner name "Code": both are checked.
        let window = RawWindow {
            owner_name: Some("Code".into()),
            ..app("Electron", "main.rs", 12)
        };
        let list = classify_windows(&snapshot(vec![window]), BOTH, OWN_PID)?;
        assert_eq!(names(&list), ["Electron", "Code"]);
        // Attribution (first entry) stays the executable.
        assert_eq!(list.windows[0].process_name, "Electron");
        for name in ["Electron", "code"] {
            let policy = PrivacyPolicy {
                excluded_processes: vec![name.into()],
                ..PrivacyPolicy::default()
            };
            assert!(
                list.windows.iter().any(|w| policy
                    .evaluate(Some(&w.application_context()), None)
                    .is_excluded()),
                "{name}"
            );
        }
        // Same name in different case is not duplicated.
        let same = RawWindow {
            owner_name: Some("safari".into()),
            ..app("Safari", "x", 13)
        };
        assert_eq!(
            names(&classify_windows(&snapshot(vec![same]), BOTH, OWN_PID)?),
            ["Safari"]
        );
        Ok(())
    }

    #[test]
    fn a_window_with_no_owner_at_all_fails_closed_only_when_application_rules_exist() -> TestResult
    {
        let snap = snapshot(vec![app("Safari", "Docs", 10), overlay(None)]);
        assert_eq!(
            classify_windows(&snap, BOTH, OWN_PID).err(),
            Some(WindowListError::OwnerUnknown { windows: 1 })
        );
        let titles_only = RuleNeeds {
            process_rules: false,
            title_rules: true,
        };
        let list = classify_windows(&snap, titles_only, OWN_PID)?;
        assert_eq!(list.stats.owner_unknown, 1);
        assert_eq!(list.windows[1].process_name, UNKNOWN_PROCESS);
        Ok(())
    }

    #[test]
    fn withheld_titles_fail_closed_when_title_rules_exist() -> TestResult {
        // Every regular app window is untitled: titles are being withheld (TCC), not merely absent.
        let untitled = |exe: &str, pid| RawWindow {
            title: None,
            ..app(exe, "", pid)
        };
        let snap = snapshot(vec![
            untitled("Safari", 10),
            untitled("Terminal", 11),
            overlay(Some("Window Server")),
        ]);
        let error = classify_windows(&snap, BOTH, OWN_PID)
            .err()
            .ok_or("withheld titles must make the list unknown")?;
        assert_eq!(error, WindowListError::TitlesUnavailable { untitled: 2 });
        assert_eq!(error.code(), "titles-unavailable");
        // Without title rules there is nothing titles could defeat.
        let processes_only = RuleNeeds {
            process_rules: true,
            title_rules: false,
        };
        assert!(classify_windows(&snap, processes_only, OWN_PID).is_ok());
        // One titled regular window is evidence titles are visible: the untitled ones are kept.
        let mut snap = snap;
        snap.windows.push(app("Notes", "Shopping", 14));
        let list = classify_windows(&snap, BOTH, OWN_PID)?;
        assert_eq!(list.stats.untitled_regular, 2);
        assert_eq!(list.stats.titled_regular, 1);
        Ok(())
    }

    #[test]
    fn own_titled_windows_are_not_evidence_that_titles_are_visible() {
        let snap = snapshot(vec![
            RawWindow {
                title: None,
                ..app("Safari", "", 10)
            },
            app("rsrewind", "rsRewind", i64::from(OWN_PID)),
        ]);
        assert_eq!(
            classify_windows(&snap, BOTH, OWN_PID).err(),
            Some(WindowListError::TitlesUnavailable { untitled: 1 })
        );
    }

    #[test]
    fn windows_without_usable_bounds_are_skipped_and_counted() -> TestResult {
        let snap = snapshot(vec![
            RawWindow {
                bounds: None,
                ..app("A", "a", 1)
            },
            RawWindow {
                bounds: rect(0.0, 0.0, 0.0, 10.0),
                ..app("B", "b", 2)
            },
            RawWindow {
                bounds: rect(f64::NAN, 0.0, 10.0, 10.0),
                ..app("C", "c", 3)
            },
            RawWindow {
                bounds: rect(-5000.0, -5000.0, 10.0, 10.0),
                ..app("D", "d", 4)
            },
            app("E", "e", 5),
        ]);
        let list = classify_windows(&snap, BOTH, OWN_PID)?;
        assert_eq!(list.stats.no_bounds, 3);
        assert_eq!(list.stats.off_display, 1);
        assert_eq!(names(&list), ["E"]);
        Ok(())
    }

    #[test]
    fn bridge_errors_and_broken_displays_make_the_list_unknown() {
        let mut snap = snapshot(vec![app("Safari", "Docs", 10)]);
        snap.error = Some("no-permission".into());
        assert_eq!(
            classify_windows(&snap, BOTH, OWN_PID).err(),
            Some(WindowListError::NoPermission)
        );
        snap.error = Some("weird <title> stuff".into());
        let error = classify_windows(&snap, BOTH, OWN_PID).err();
        assert_eq!(
            error,
            Some(WindowListError::Bridge("weirdtitlestuff".into()))
        );

        let mut snap = snapshot(vec![]);
        snap.displays.clear();
        assert_eq!(
            classify_windows(&snap, BOTH, OWN_PID).err(),
            Some(WindowListError::NoDisplays)
        );
        let mut snap = snapshot(vec![]);
        snap.displays[1].width_pt = 0.0;
        assert_eq!(
            classify_windows(&snap, BOTH, OWN_PID).err(),
            Some(WindowListError::BadDisplayGeometry)
        );
        assert_eq!(
            parse_windows(b"not json").err(),
            Some(WindowListError::Malformed)
        );
    }

    #[test]
    fn parses_what_the_bridge_emits_with_fields_missing() -> TestResult {
        let json = br#"{"displays":[{"x":0,"min_x":0,"min_y":0,"width_pt":1440,"height_pt":900,"width_px":2880,"height_px":1800}],
            "windows":[{"pid":5,"owner_name":"Control Center","bounds":{"x":1300,"y":0,"width":40,"height":24}},
                       {"pid":6,"executable":"Safari","regular":true,"owner_name":"Safari","title":"t","layer":0,"bounds":{"x":0,"y":30,"width":100,"height":100}}]}"#;
        let snap = parse_windows(json)?;
        let list = classify_windows(&snap, BOTH, OWN_PID)?;
        assert_eq!(names(&list), ["Control Center", "Safari"]);
        assert_eq!(
            parse_windows(br#"{"error":"list-unavailable"}"#)?
                .error
                .as_deref(),
            Some("list-unavailable")
        );
        Ok(())
    }

    #[test]
    fn clipping_maps_points_to_pixels_per_display() {
        let ds = displays();
        // Straddles both displays: 1340..1540 pt horizontally.
        let window = RawRect {
            x: 1340.0,
            y: 10.0,
            width: 200.0,
            height: 50.0,
        };
        assert_eq!(
            clip_to_display(&window, &ds[0]),
            Some(ScreenRect {
                left: 2680,
                top: 20,
                right: 2880,
                bottom: 120
            })
        );
        assert_eq!(
            clip_to_display(&window, &ds[1]),
            Some(ScreenRect {
                left: 2880,
                top: 10,
                right: 2980,
                bottom: 60
            })
        );
        // Touching an edge is not overlap.
        let touching = RawRect {
            x: 1440.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        assert_eq!(clip_to_display(&touching, &ds[0]), None);
        // Fractional points round outward, never shrinking a window off a monitor.
        let tiny = RawRect {
            x: 10.25,
            y: 0.25,
            width: 0.5,
            height: 0.5,
        };
        assert_eq!(
            clip_to_display(&tiny, &ds[0]),
            Some(ScreenRect {
                left: 20,
                top: 0,
                right: 22,
                bottom: 2
            })
        );
    }

    #[test]
    fn a_straddling_window_is_listed_once_per_display() -> TestResult {
        let window = RawWindow {
            bounds: rect(1340.0, 10.0, 200.0, 50.0),
            ..app("Safari", "Docs", 10)
        };
        let list = classify_windows(&snapshot(vec![window]), BOTH, OWN_PID)?;
        assert_eq!(list.windows.len(), 2);
        assert_eq!(list.stats.on_screen, 1);
        Ok(())
    }

    #[test]
    fn debug_output_never_contains_a_title() {
        let window = app("Bank", "Account 12345", 1);
        let text = format!("{window:?}");
        assert!(!text.contains("12345"), "{text}");
        assert!(text.contains("13 chars"), "{text}");
    }

    #[test]
    fn rule_needs_ignore_blank_entries() {
        let policy = PrivacyPolicy {
            excluded_processes: vec!["  ".into()],
            excluded_title_patterns: vec!["*Incognito*".into()],
            ..PrivacyPolicy::default()
        };
        assert_eq!(
            RuleNeeds::of(&policy),
            RuleNeeds {
                process_rules: false,
                title_rules: true
            }
        );
    }

    fn active() -> SessionFacts {
        SessionFacts {
            have_session: true,
            on_console: Some(true),
            login_done: Some(true),
            screen_locked: None,
            windows_listed: true,
            loginwindow: Vec::new(),
            unresolved_owners: 0,
            idle_seconds: Some(2.5),
        }
    }

    #[test]
    fn an_unresolvable_window_owner_no_longer_makes_idle_unknown() {
        let facts = SessionFacts {
            unresolved_owners: 9,
            ..active()
        };
        assert_eq!(classify_session(&facts), Ok(2_500));
    }

    #[test]
    fn locked_foreign_and_unfinished_sessions_stay_unknown() {
        let cases: Vec<(SessionFacts, SessionUnknown)> = vec![
            (
                SessionFacts {
                    have_session: false,
                    ..active()
                },
                SessionUnknown::NoSessionInfo,
            ),
            (
                SessionFacts {
                    on_console: Some(false),
                    ..active()
                },
                SessionUnknown::NotOnConsole,
            ),
            (
                SessionFacts {
                    on_console: None,
                    ..active()
                },
                SessionUnknown::NotOnConsole,
            ),
            (
                SessionFacts {
                    login_done: None,
                    ..active()
                },
                SessionUnknown::LoginNotDone,
            ),
            (
                SessionFacts {
                    screen_locked: Some(true),
                    ..active()
                },
                SessionUnknown::ScreenLocked,
            ),
            (
                SessionFacts {
                    windows_listed: false,
                    ..active()
                },
                SessionUnknown::WindowListUnavailable,
            ),
            (
                SessionFacts {
                    loginwindow: vec![LoginWindowFact::default()],
                    ..active()
                },
                SessionUnknown::LoginWindowVisible { windows: 1 },
            ),
            (
                SessionFacts {
                    idle_seconds: None,
                    ..active()
                },
                SessionUnknown::IdleUnavailable,
            ),
            (
                SessionFacts {
                    idle_seconds: Some(-1.0),
                    ..active()
                },
                SessionUnknown::IdleUnavailable,
            ),
        ];
        for (facts, expected) in cases {
            assert_eq!(classify_session(&facts), Err(expected));
        }
        assert_eq!(
            classify_session(&SessionFacts {
                screen_locked: Some(false),
                ..active()
            }),
            Ok(2_500)
        );
        // An empty object (every key missing) is unknown, never "active".
        assert!(classify_session(&SessionFacts::default()).is_err());
        assert!(parse_session(b"{}").is_ok_and(|f| classify_session(&f).is_err()));
    }

    #[test]
    fn frame_failures_name_the_condition() {
        assert!(frame_failure(frame_reason::TIMEOUT, 0).contains("15 s"));
        let text = frame_failure(frame_reason::SHAREABLE_CONTENT, -3801);
        assert!(text.contains("-3801"), "{text}");
        assert!(frame_failure(99, 0).contains("unknown"));
    }
}
