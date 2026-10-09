//! KDE Plasma on Wayland: screens, windows, focus and screenshots from KWin over the user's
//! session bus.
//!
//! - **Screenshots** come from `org.kde.KWin.ScreenShot2.CaptureScreen`, which KWin only answers
//!   for executables listed in a `.desktop` file with
//!   `X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2` (see [`desktop_entry`]).
//! - **Screens, windows and focus** come from a tiny KWin script, loaded for one run per question,
//!   which reports a JSON snapshot back by calling a method on this process's own bus connection.
//!   A report is accepted only from KWin's unique bus name and only with the nonce of the run that
//!   asked for it; anything else is ignored, and no answer within [`REPORT_TIMEOUT`] is an error
//!   (the recorder then stores nothing for that tick).
//!
//! The bus connection registers no well-known name and exposes nothing but that one report
//! method; control and data never travel over it (decision recorded in the Linux port motion).
//!
//! Coordinates are KWin's logical (scaled) desktop coordinates for screens and windows alike, so
//! the recorder's window-to-monitor intersection compares like with like. Screenshots are taken at
//! native resolution, so a frame is larger than its monitor's logical size by the screen scale.

use crate::context::{ScreenRect, UNKNOWN_PROCESS, VisibleWindow};
use rsrewind_core::{ApplicationContext, BgraFrame, FocusContext, MonitorInfo, WindowContext};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use zbus::blocking::Connection;
use zbus::zvariant::{Fd, OwnedValue, Value};

const KWIN: &str = "org.kde.KWin";
const SCREENSHOT_PATH: &str = "/org/kde/KWin/ScreenShot2";
const SCREENSHOT_IFACE: &str = "org.kde.KWin.ScreenShot2";
const REPORT_PATH: &str = "/org/rsrewind/KwinReport";
const REPORT_IFACE: &str = "org.rsrewind.KwinReport";
/// How long a script run may take to report before the snapshot counts as unknown.
pub const REPORT_TIMEOUT: Duration = Duration::from_secs(2);

/// A failed KWin call. The message names the call and the D-Bus error, never captured content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KwinError {
    pub message: String,
    /// Retrying (or reconnecting) is expected to help.
    pub recoverable: bool,
}

impl KwinError {
    fn new(message: impl Into<String>, recoverable: bool) -> Self {
        Self {
            message: message.into(),
            recoverable,
        }
    }
}

impl std::fmt::Display for KwinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KwinError {}

fn bus_error(call: &str, error: zbus::Error) -> KwinError {
    // A D-Bus error name/message from KWin describes the failure, not the screen.
    let recoverable = !matches!(&error, zbus::Error::MethodError(name, _, _)
        if name.as_str().ends_with("NoAuthorized"));
    KwinError::new(format!("{call}: {error}"), recoverable)
}

/// The `.desktop` file that lets `exe` take screenshots through KWin. Plasma reads it from
/// `~/.local/share/applications/`.
pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=rsRewind recorder\nComment=Local screen history (rsrewind daemon)\nExec={} daemon\nNoDisplay=true\nX-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2\n",
        exe.display()
    )
}

/// One screen as the script reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct KwinScreen {
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    #[serde(default = "one")]
    pub scale: f64,
}

fn one() -> f64 {
    1.0
}

/// One window as the script reports it.
#[derive(Clone, PartialEq, Deserialize)]
pub struct KwinWindow {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub class: String,
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub w: f64,
    #[serde(default)]
    pub h: f64,
    #[serde(default)]
    pub output: String,
}

impl std::fmt::Debug for KwinWindow {
    // Titles are captured content; they never reach a log through Debug.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KwinWindow")
            .field("class", &self.class)
            .field("pid", &self.pid)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

/// What one script run saw. `windows` is topmost first and holds only windows that are visible
/// on the current virtual desktop (not minimized, not hidden).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Snapshot {
    pub nonce: String,
    pub screens: Vec<KwinScreen>,
    #[serde(default)]
    pub active: Option<KwinWindow>,
    pub windows: Vec<KwinWindow>,
}

impl Snapshot {
    pub fn parse(json: &str) -> Result<Self, KwinError> {
        // serde_json's message names a position and an expected token, not the input text.
        serde_json::from_str(json)
            .map_err(|error| KwinError::new(format!("KWin snapshot: {error}"), true))
    }

    /// Screens as monitors, in logical coordinates; the first screen KWin lists is primary.
    pub fn monitors(&self) -> Vec<MonitorInfo> {
        self.screens
            .iter()
            .enumerate()
            .map(|(i, s)| MonitorInfo {
                device_name: s.name.clone(),
                left: s.x,
                top: s.y,
                width: s.w,
                height: s.h,
                dpi: (96.0 * s.scale).round().clamp(1.0, 10_000.0) as u32,
                primary: i == 0,
            })
            .collect()
    }

    pub fn visible_windows(
        &self,
        names: &mut dyn FnMut(&KwinWindow) -> String,
    ) -> Vec<VisibleWindow> {
        self.windows
            .iter()
            .filter_map(|w| {
                let rect = window_rect(w)?;
                Some(VisibleWindow {
                    process_name: names(w),
                    title: w.title.clone(),
                    rect,
                    monitor: w.output.clone(),
                    pid: pid(w),
                })
            })
            .collect()
    }

    pub fn foreground(&self, names: &mut dyn FnMut(&KwinWindow) -> String) -> Option<FocusContext> {
        let w = self.active.as_ref()?;
        Some(FocusContext {
            application: ApplicationContext {
                process_name: names(w),
                exe_path: None,
            },
            window: WindowContext {
                title: w.title.clone(),
                class_name: Some(w.class.clone()).filter(|c| !c.is_empty()),
            },
            pid: pid(w),
        })
    }
}

fn pid(w: &KwinWindow) -> u32 {
    u32::try_from(w.pid).unwrap_or(0)
}

/// Rounded outward, so a window is never reported smaller than it is. Empty or non-finite
/// geometry is dropped (it covers nothing).
fn window_rect(w: &KwinWindow) -> Option<ScreenRect> {
    let finite = [w.x, w.y, w.w, w.h].iter().all(|v| v.is_finite());
    if !finite || w.w <= 0.0 || w.h <= 0.0 {
        return None;
    }
    let clamp = |v: f64| v.clamp(i32::MIN as f64, i32::MAX as f64) as i32;
    Some(ScreenRect {
        left: clamp(w.x.floor()),
        top: clamp(w.y.floor()),
        right: clamp((w.x + w.w).ceil()),
        bottom: clamp((w.y + w.h).ceil()),
    })
}

/// The process name privacy rules match: the executable's file name (`keepassxc`), else the
/// kernel's command name, else KWin's window class, else [`UNKNOWN_PROCESS`].
pub fn process_name(w: &KwinWindow) -> String {
    let pid = pid(w);
    if pid != 0 {
        if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe"))
            && let Some(name) = exe.file_name().and_then(|n| n.to_str())
        {
            // A replaced binary reads as "name (deleted)".
            return name.trim_end_matches(" (deleted)").to_string();
        }
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
            let comm = comm.trim();
            if !comm.is_empty() {
                return comm.to_string();
            }
        }
    }
    if w.class.is_empty() {
        UNKNOWN_PROCESS.to_string()
    } else {
        w.class.clone()
    }
}

/// The script KWin runs. Plain ECMAScript for KWin 6's declarative-less scripting API.
fn script(reply_to: &str, nonce: &str) -> String {
    format!(
        r#"(function () {{
  var out = {{ nonce: "{nonce}", screens: [], active: null, windows: [] }};
  var screens = workspace.screens;
  for (var i = 0; i < screens.length; i++) {{
    var s = screens[i], g = s.geometry;
    out.screens.push({{ name: s.name, x: g.x, y: g.y, w: g.width, h: g.height, scale: s.devicePixelRatio }});
  }}
  var current = workspace.currentDesktop ? workspace.currentDesktop.id : null;
  function shown(w) {{
    if (w.deleted || w.minimized || w.hidden) return false;
    if (w.onAllDesktops || current === null) return true;
    var ds = w.desktops || [];
    for (var j = 0; j < ds.length; j++) if (ds[j] && ds[j].id === current) return true;
    return ds.length === 0;
  }}
  function info(w) {{
    var g = w.frameGeometry;
    return {{ title: "" + (w.caption || ""), class: "" + (w.resourceClass || ""), pid: w.pid || 0,
             x: g.x, y: g.y, w: g.width, h: g.height, output: w.output ? w.output.name : "" }};
  }}
  var order = workspace.stackingOrder;
  for (var k = order.length - 1; k >= 0; k--) if (shown(order[k])) out.windows.push(info(order[k]));
  if (workspace.activeWindow) out.active = info(workspace.activeWindow);
  callDBus("{reply_to}", "{REPORT_PATH}", "{REPORT_IFACE}", "Report", JSON.stringify(out));
}})();
"#
    )
}

/// Receives script reports. Accepts a call only from KWin's current unique name.
struct Reporter {
    kwin_owner: Arc<RwLock<String>>,
    tx: Sender<String>,
}

#[zbus::interface(name = "org.rsrewind.KwinReport")]
impl Reporter {
    fn report(&self, #[zbus(header)] header: zbus::message::Header<'_>, json: String) {
        let from_kwin = match (header.sender(), self.kwin_owner.read()) {
            (Some(sender), Ok(owner)) => sender.as_str() == owner.as_str(),
            _ => false,
        };
        if from_kwin {
            let _ = self.tx.send(json);
        } else {
            tracing::warn!("ignored a KWin report from a sender that is not KWin");
        }
    }
}

/// A session-bus connection to KWin. One per recorder.
pub struct Kwin {
    conn: Connection,
    kwin_owner: Arc<RwLock<String>>,
    reports: Receiver<String>,
    script_path: PathBuf,
    plugin: String,
    runs: u64,
}

impl Kwin {
    /// Connects to the session bus. `runtime_dir` holds the script file (created `0700`).
    pub fn connect(runtime_dir: &Path) -> Result<Self, KwinError> {
        let conn = Connection::session().map_err(|e| bus_error("connect to the session bus", e))?;
        let kwin_owner = Arc::new(RwLock::new(String::new()));
        let (tx, reports) = channel();
        conn.object_server()
            .at(
                REPORT_PATH,
                Reporter {
                    kwin_owner: kwin_owner.clone(),
                    tx,
                },
            )
            .map_err(|e| bus_error("export the report object", e))?;
        create_private_dir(runtime_dir)?;
        let id = std::process::id();
        Ok(Self {
            conn,
            kwin_owner,
            reports,
            script_path: runtime_dir.join(format!("kwin-snapshot-{id}.js")),
            plugin: format!("rsrewind-snapshot-{id}"),
            runs: 0,
        })
    }

    /// True when KWin is on the bus with the screenshot interface.
    pub fn available(&self) -> bool {
        self.owner().is_ok()
    }

    fn owner(&self) -> Result<String, KwinError> {
        let reply = self
            .conn
            .call_method(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                Some("org.freedesktop.DBus"),
                "GetNameOwner",
                &(KWIN,),
            )
            .map_err(|e| bus_error("find KWin on the session bus", e))?;
        reply
            .body()
            .deserialize::<String>()
            .map_err(|e| bus_error("read KWin's bus name", e))
    }

    fn scripting(
        &self,
        method: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
    ) -> Result<zbus::message::Message, KwinError> {
        self.conn
            .call_method(
                Some(KWIN),
                "/Scripting",
                Some("org.kde.kwin.Scripting"),
                method,
                body,
            )
            .map_err(|e| bus_error(&format!("KWin Scripting.{method}"), e))
    }

    /// Runs the snapshot script once and waits for its report.
    pub fn snapshot(&mut self) -> Result<Snapshot, KwinError> {
        let owner = self.owner()?;
        if let Ok(mut current) = self.kwin_owner.write() {
            *current = owner;
        }
        self.runs += 1;
        let nonce = format!("{}-{}", std::process::id(), self.runs);
        let reply_to = self
            .conn
            .unique_name()
            .map(|n| n.to_string())
            .ok_or_else(|| KwinError::new("no unique bus name", true))?;
        std::fs::write(&self.script_path, script(&reply_to, &nonce))
            .map_err(|e| KwinError::new(format!("write the KWin script: {e}"), true))?;
        // A run that timed out earlier may still be loaded under our plugin name.
        let _ = self.scripting("unloadScript", &(self.plugin.as_str(),));
        let id: i32 = self
            .scripting(
                "loadScript",
                &(
                    self.script_path.to_string_lossy().as_ref(),
                    self.plugin.as_str(),
                ),
            )?
            .body()
            .deserialize()
            .map_err(|e| bus_error("read the KWin script id", e))?;
        if id < 0 {
            return Err(KwinError::new(
                "KWin refused to load the snapshot script",
                true,
            ));
        }
        let run = self.conn.call_method(
            Some(KWIN),
            format!("/Scripting/Script{id}").as_str(),
            Some("org.kde.kwin.Script"),
            "run",
            &(),
        );
        let result = run
            .map_err(|e| bus_error("run the KWin script", e))
            .and_then(|_| self.wait_for(&nonce));
        let _ = self.scripting("unloadScript", &(self.plugin.as_str(),));
        result
    }

    fn wait_for(&self, nonce: &str) -> Result<Snapshot, KwinError> {
        let deadline = Instant::now() + REPORT_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let json = self
                .reports
                .recv_timeout(left)
                .map_err(|_| KwinError::new("KWin did not report a snapshot in time", true))?;
            let snapshot = Snapshot::parse(&json)?;
            if snapshot.nonce == nonce {
                return Ok(snapshot);
            }
            // A late report from an earlier run: stale, never used.
        }
    }

    /// Whether the session's screen locker is active; `None` when it cannot be told.
    pub fn screen_locked(&self) -> Option<bool> {
        self.conn
            .call_method(
                Some("org.freedesktop.ScreenSaver"),
                "/ScreenSaver",
                Some("org.freedesktop.ScreenSaver"),
                "GetActive",
                &(),
            )
            .ok()?
            .body()
            .deserialize::<bool>()
            .ok()
    }

    /// One screen at native resolution, as packed BGRA.
    pub fn capture_screen(&self, name: &str) -> Result<BgraFrame, KwinError> {
        let (mut reader, writer) =
            std::io::pipe().map_err(|e| KwinError::new(format!("create a pipe: {e}"), true))?;
        // Read concurrently: KWin writes after replying, and a full pipe would stall it.
        let pump = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).map(|_| bytes)
        });
        let mut options: HashMap<&str, Value<'_>> = HashMap::new();
        options.insert("native-resolution", Value::from(true));
        options.insert("include-cursor", Value::from(false));
        let writer_fd: std::os::fd::OwnedFd = writer.into();
        let reply = self.conn.call_method(
            Some(KWIN),
            SCREENSHOT_PATH,
            Some(SCREENSHOT_IFACE),
            "CaptureScreen",
            &(name, options, Fd::from(&writer_fd)),
        );
        drop(writer_fd);
        let reply = reply.map_err(|e| bus_error("KWin CaptureScreen", e))?;
        let meta: HashMap<String, OwnedValue> = reply
            .body()
            .deserialize()
            .map_err(|e| bus_error("read the screenshot description", e))?;
        let bytes = pump
            .join()
            .map_err(|_| KwinError::new("screenshot reader panicked", true))?
            .map_err(|e| KwinError::new(format!("read the screenshot: {e}"), true))?;
        frame_from_raw(&meta, bytes)
    }
}

impl Drop for Kwin {
    fn drop(&mut self) {
        let _ = self.scripting("unloadScript", &(self.plugin.as_str(),));
        let _ = std::fs::remove_file(&self.script_path);
    }
}

fn meta_u32(meta: &HashMap<String, OwnedValue>, key: &str) -> Result<u32, KwinError> {
    meta.get(key)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| KwinError::new(format!("screenshot description lacks {key}"), false))
}

/// QImage formats whose bytes are B, G, R, A on a little-endian machine.
const QIMAGE_RGB32: u32 = 4;
const QIMAGE_ARGB32: u32 = 5;
const QIMAGE_ARGB32_PREMULTIPLIED: u32 = 6;

/// Validates KWin's raw image against its description and repacks it as opaque BGRA.
pub fn frame_from_raw(
    meta: &HashMap<String, OwnedValue>,
    bytes: Vec<u8>,
) -> Result<BgraFrame, KwinError> {
    let width = meta_u32(meta, "width")?;
    let height = meta_u32(meta, "height")?;
    let stride = meta_u32(meta, "stride")?;
    let format = meta_u32(meta, "format")?;
    if !cfg!(target_endian = "little")
        || ![QIMAGE_RGB32, QIMAGE_ARGB32, QIMAGE_ARGB32_PREMULTIPLIED].contains(&format)
    {
        return Err(KwinError::new(
            format!("unsupported screenshot format {format}"),
            false,
        ));
    }
    let row = width as usize * 4;
    let needed = (stride as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| KwinError::new("screenshot size overflows", false))?;
    if width == 0 || height == 0 || (stride as usize) < row || bytes.len() < needed {
        return Err(KwinError::new(
            format!("screenshot is {} bytes, expected {needed}", bytes.len()),
            true,
        ));
    }
    let mut frame = BgraFrame {
        width,
        height,
        stride,
        pixels: bytes,
    }
    .into_packed();
    frame.pixels.truncate(row * height as usize);
    for alpha in frame.pixels.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    Ok(frame)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<(), KwinError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .and_then(|_| std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)))
        .map_err(|e| KwinError::new(format!("create {}: {e}", dir.display()), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SNAPSHOT: &str = r#"{"nonce":"7-1","screens":[{"name":"eDP-1","x":0,"y":0,"w":1849,"h":1233,"scale":1.35},{"name":"DP-2","x":1849,"y":0,"w":1920,"h":1080}],
      "active":{"title":"notes - Kate","class":"org.kde.kate","pid":0,"x":10,"y":20,"w":800,"h":600,"output":"eDP-1"},
      "windows":[{"title":"notes - Kate","class":"org.kde.kate","pid":0,"x":10.5,"y":20,"w":800.2,"h":600,"output":"eDP-1"},
                 {"title":"empty","class":"x","pid":-3,"x":0,"y":0,"w":0,"h":10,"output":"eDP-1"},
                 {"title":"Vault","class":"keepassxc","pid":0,"x":1800,"y":5,"w":200,"h":100,"output":"DP-2"}]}"#;

    fn by_class(w: &KwinWindow) -> String {
        w.class.clone()
    }

    #[test]
    fn a_snapshot_becomes_monitors_windows_and_focus() -> Result<(), Box<dyn std::error::Error>> {
        let snap = Snapshot::parse(SNAPSHOT)?;
        assert_eq!(snap.nonce, "7-1");
        let monitors = snap.monitors();
        assert_eq!(monitors.len(), 2);
        assert_eq!(
            (
                monitors[0].device_name.as_str(),
                monitors[0].dpi,
                monitors[0].primary
            ),
            ("eDP-1", 130, true)
        );
        assert_eq!(
            (monitors[1].left, monitors[1].dpi, monitors[1].primary),
            (1849, 96, false)
        );

        let windows = snap.visible_windows(&mut by_class);
        // The zero-width window covers nothing and is dropped; order (topmost first) is kept.
        assert_eq!(windows.len(), 2);
        assert_eq!(
            windows[0].rect,
            ScreenRect {
                left: 10,
                top: 20,
                right: 811,
                bottom: 620
            }
        );
        assert_eq!(windows[1].process_name, "keepassxc");
        // A window straddling two screens touches both.
        assert!(windows[1].rect.intersects(&monitors[0]));
        assert!(windows[1].rect.intersects(&monitors[1]));

        let focus = snap.foreground(&mut by_class).ok_or("no focus")?;
        assert_eq!(focus.window.class_name.as_deref(), Some("org.kde.kate"));
        assert_eq!(focus.pid, 0);
        Ok(())
    }

    #[test]
    fn malformed_reports_are_errors_and_debug_hides_titles() {
        assert!(Snapshot::parse("{").is_err());
        assert!(Snapshot::parse(r#"{"nonce":"1"}"#).is_err());
        let w = KwinWindow {
            title: "secret title".into(),
            class: "c".into(),
            pid: 1,
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            output: String::new(),
        };
        assert!(!format!("{w:?}").contains("secret"));
    }

    #[test]
    fn process_names_fall_back_to_the_window_class() {
        let mut w = KwinWindow {
            title: String::new(),
            class: "org.example.app".into(),
            pid: 0,
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            output: String::new(),
        };
        assert_eq!(process_name(&w), "org.example.app");
        w.class.clear();
        assert_eq!(process_name(&w), UNKNOWN_PROCESS);
        w.pid = i64::from(std::process::id());
        assert_ne!(process_name(&w), UNKNOWN_PROCESS);
    }

    fn meta(width: u32, height: u32, stride: u32, format: u32) -> HashMap<String, OwnedValue> {
        [
            ("width", width),
            ("height", height),
            ("stride", stride),
            ("format", format),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), OwnedValue::from(v)))
        .collect()
    }

    #[test]
    fn raw_screenshots_are_validated_and_packed_opaque() -> Result<(), Box<dyn std::error::Error>> {
        // 2x2 with 4 bytes of row padding.
        let raw: Vec<u8> = (0u8..24).collect();
        let frame = frame_from_raw(&meta(2, 2, 12, 6), raw.clone())?;
        assert_eq!((frame.width, frame.height, frame.stride), (2, 2, 8));
        assert_eq!(
            frame.pixels,
            vec![0, 1, 2, 255, 4, 5, 6, 255, 12, 13, 14, 255, 16, 17, 18, 255]
        );
        assert!(frame_from_raw(&meta(2, 2, 12, 13), raw.clone()).is_err());
        assert!(frame_from_raw(&meta(2, 2, 4, 6), raw.clone()).is_err());
        assert!(frame_from_raw(&meta(2, 3, 12, 6), raw.clone()).is_err());
        assert!(frame_from_raw(&meta(0, 2, 12, 6), raw).is_err());
        Ok(())
    }

    #[test]
    fn the_desktop_entry_authorizes_exactly_this_executable() {
        let entry = desktop_entry(Path::new("/opt/rsrewind/rsrewind"));
        assert!(entry.contains("\nExec=/opt/rsrewind/rsrewind daemon\n"));
        assert!(entry.contains("X-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2\n"));
    }
}
