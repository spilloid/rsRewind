//! The window's view of the recorder, and its "Start recording" / "Start at login" buttons.
//!
//! Like the tray, the window never touches capture or storage: it learns the recorder's state from
//! `rsrewind status --json` and acts by running `rsrewind` subcommands as child processes, off the UI
//! thread. The executable is the one running this window (`rsrewind ui --foreground`).

use iced::futures::channel::oneshot;
use serde::Deserialize;
use std::future::Future;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How often the window re-reads the recorder's state.
pub const POLL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorder {
    Recording,
    /// Running and not paused, but storing nothing (status `not_saving`): the chip must not just
    /// say "Recording" then (issue #33).
    NotSaving(NotSaving),
    Paused,
    NotRunning,
    Error,
    /// Not read yet, or the status command failed.
    Unknown,
}

/// Why a recording recorder stores nothing, from `not_saving.reason` in `rsrewind status --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotSaving {
    WindowContext,
    Session,
    Capture,
    NoDisplays,
    Idle,
    Excluded,
    /// A reason this window does not know (a newer recorder).
    Other,
}

impl NotSaving {
    fn parse(reason: &str) -> Self {
        match reason {
            "window_context_unavailable" => Self::WindowContext,
            "session_state_unknown" => Self::Session,
            "capture_failing" => Self::Capture,
            "no_displays" => Self::NoDisplays,
            "idle" => Self::Idle,
            "privacy_excluded" => Self::Excluded,
            _ => Self::Other,
        }
    }

    /// A problem to fix, not an expected pause in saving.
    pub fn is_fault(self) -> bool {
        !matches!(self, Self::Idle | Self::Excluded)
    }

    /// Why nothing is saved, one sentence for the empty window.
    pub fn explain(self) -> &'static str {
        match self {
            Self::WindowContext => {
                "The recorder cannot tell which windows are on screen, so it stores nothing (an unknown window list could hide an excluded app). Run `rsrewind doctor` for the exact cause."
            }
            Self::Session => {
                "The recorder cannot confirm an unlocked session on this screen, so it stores nothing. Run `rsrewind doctor` for the exact cause."
            }
            Self::Capture => {
                "Screen capture is failing on every display. Run `rsrewind doctor` for the exact cause."
            }
            Self::NoDisplays => "No displays were found. Run `rsrewind doctor`.",
            Self::Idle => "Nothing is stored while you are away; it resumes when you are back.",
            Self::Excluded => {
                "An excluded window is visible on every screen, so nothing is stored."
            }
            Self::Other => "The recorder reports that it stores nothing. Run `rsrewind doctor`.",
        }
    }
}

impl Recorder {
    /// Parses `rsrewind status --json`; anything unreadable is [`Recorder::Unknown`].
    pub fn parse(json: &str) -> Self {
        #[derive(Deserialize)]
        struct Reason {
            reason: String,
        }
        #[derive(Deserialize)]
        struct Raw {
            state: String,
            #[serde(default)]
            not_saving: Option<Reason>,
        }
        match serde_json::from_str::<Raw>(json) {
            Ok(r) if r.state == "recording" => match r.not_saving {
                Some(n) => Self::NotSaving(NotSaving::parse(&n.reason)),
                None => Self::Recording,
            },
            Ok(r) if r.state == "paused" => Self::Paused,
            Ok(r) if r.state == "not_running" => Self::NotRunning,
            Ok(r) if r.state == "error" => Self::Error,
            _ => Self::Unknown,
        }
    }

    /// Running and not paused, whether or not anything is being saved.
    pub fn is_recording(self) -> bool {
        matches!(self, Self::Recording | Self::NotSaving(_))
    }

    /// Short label for the top bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Recording => "● Recording",
            Self::NotSaving(why) => match why {
                NotSaving::WindowContext => {
                    "● Recording · nothing saved: window context unavailable"
                }
                NotSaving::Session => "● Recording · nothing saved: session state unknown",
                NotSaving::Capture => "● Recording · nothing saved: capture failing",
                NotSaving::NoDisplays => "● Recording · nothing saved: no displays",
                NotSaving::Idle => "● Recording · idle",
                NotSaving::Excluded => "● Recording · excluded window on screen",
                NotSaving::Other => "● Recording · nothing saved",
            },
            Self::Paused => "Paused",
            Self::NotRunning => "Not recording",
            Self::Error => "Recorder error",
            Self::Unknown => "…",
        }
    }

    /// Whether offering "Start recording" makes sense.
    pub fn can_start(self) -> bool {
        matches!(self, Self::NotRunning | Self::Error)
    }
}

/// Runs `rsrewind --data-dir <dir> <args>` for this window's data folder.
#[derive(Debug, Clone)]
pub struct Control {
    exe: Option<PathBuf>,
    data_dir: PathBuf,
}

impl Control {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            exe: std::env::current_exe().ok(),
            data_dir,
        }
    }

    fn command(&self, args: &[&str]) -> Option<Command> {
        let mut command = Command::new(self.exe.as_ref()?);
        command
            .arg("--data-dir")
            .arg(&self.data_dir)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        Some(command)
    }

    /// The recorder's state, after waiting `delay` (so polls can be chained without a timer).
    pub fn status(&self, delay: Duration) -> impl Future<Output = Recorder> + Send + 'static {
        let this = self.clone();
        off_thread(Recorder::Unknown, move || {
            std::thread::sleep(delay);
            this.command(&["status", "--json"])
                .and_then(|mut c| c.stdout(Stdio::piped()).output().ok())
                .map_or(Recorder::Unknown, |out| {
                    Recorder::parse(&String::from_utf8_lossy(&out.stdout))
                })
        })
    }

    /// Runs one subcommand; `Ok(())` or the error it printed (one line, for display).
    pub fn run(
        &self,
        args: &'static [&'static str],
    ) -> impl Future<Output = Result<(), String>> + Send + 'static {
        self.run_args(args.iter().map(|a| (*a).to_owned()).collect(), None)
    }

    /// [`Control::run`] with arguments built at run time (mark ids and times), and `input` written
    /// to the child's standard input. Each argument is one argv element (no shell). Labels go as
    /// `input` (`--label-stdin`), never as arguments: arguments are visible in the process list.
    pub fn run_args(
        &self,
        args: Vec<String>,
        input: Option<String>,
    ) -> impl Future<Output = Result<(), String>> + Send + 'static {
        let this = self.clone();
        off_thread(Err("could not run rsrewind".to_string()), move || {
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let Some(mut command) = this.command(&refs) else {
                return Err("could not find the rsrewind executable".into());
            };
            // `start` and `tray` leave a background process behind; it must not hold our pipes.
            command.stdout(Stdio::null());
            let output = match input {
                None => command.output().map_err(|e| e.to_string())?,
                Some(input) => {
                    use std::io::Write;
                    let mut child = command
                        .stdin(Stdio::piped())
                        .spawn()
                        .map_err(|e| e.to_string())?;
                    if let Some(mut stdin) = child.stdin.take() {
                        // A write error shows up as the child's own error below.
                        let _ = stdin.write_all(input.as_bytes());
                    }
                    child.wait_with_output().map_err(|e| e.to_string())?
                }
            };
            if output.status.success() {
                Ok(())
            } else {
                let err = String::from_utf8_lossy(&output.stderr);
                Err(err
                    .lines()
                    .last()
                    .unwrap_or("it did not work")
                    .trim()
                    .trim_start_matches("rsrewind: ")
                    .to_string())
            }
        })
    }
}

/// Runs blocking work on its own thread and awaits the result without blocking the UI; `fallback`
/// if the thread cannot start or vanishes.
pub(crate) fn off_thread<T: Send + 'static>(
    fallback: T,
    work: impl FnOnce() -> T + Send + 'static,
) -> impl Future<Output = T> + Send + 'static {
    let (tx, rx) = oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("rsrewind-ui-cli".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .is_ok();
    async move {
        if !spawned {
            return fallback;
        }
        rx.await.unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_maps_to_a_state() {
        assert_eq!(
            Recorder::parse(r#"{"state":"recording","running":true}"#),
            Recorder::Recording
        );
        assert_eq!(Recorder::parse(r#"{"state":"paused"}"#), Recorder::Paused);
        assert_eq!(
            Recorder::parse(r#"{"state":"not_running"}"#),
            Recorder::NotRunning
        );
        assert_eq!(Recorder::parse(r#"{"state":"error"}"#), Recorder::Error);
        assert_eq!(Recorder::parse(""), Recorder::Unknown);
        assert_eq!(Recorder::parse(r#"{"state":"?"}"#), Recorder::Unknown);
        assert!(Recorder::NotRunning.can_start());
        assert!(!Recorder::Recording.can_start());
        assert!(!Recorder::Unknown.can_start());
    }

    #[test]
    fn recording_but_not_saving_is_never_shown_as_plain_recording() {
        let r = Recorder::parse(
            r#"{"state":"recording","not_saving":{"reason":"window_context_unavailable","detail":"x","fault":true,"since":1}}"#,
        );
        assert_eq!(r, Recorder::NotSaving(NotSaving::WindowContext));
        assert!(r.label().contains("nothing saved"));
        assert!(r.is_recording());
        assert!(!r.can_start());
        // A reason from a newer recorder still says nothing is saved.
        let r = Recorder::parse(r#"{"state":"recording","not_saving":{"reason":"later"}}"#);
        assert_eq!(r, Recorder::NotSaving(NotSaving::Other));
        assert!(r.label().contains("nothing saved"));
        assert!(NotSaving::Other.is_fault());
        assert!(!NotSaving::Idle.is_fault());
        // Absent: plain recording, as before.
        assert_eq!(
            Recorder::parse(r#"{"state":"recording","not_saving":null}"#),
            Recorder::Recording
        );
    }
}
