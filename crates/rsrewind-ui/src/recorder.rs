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
    Paused,
    NotRunning,
    Error,
    /// Not read yet, or the status command failed.
    Unknown,
}

impl Recorder {
    /// Parses `rsrewind status --json`; anything unreadable is [`Recorder::Unknown`].
    pub fn parse(json: &str) -> Self {
        #[derive(Deserialize)]
        struct Raw {
            state: String,
        }
        match serde_json::from_str::<Raw>(json).map(|r| r.state) {
            Ok(s) if s == "recording" => Self::Recording,
            Ok(s) if s == "paused" => Self::Paused,
            Ok(s) if s == "not_running" => Self::NotRunning,
            Ok(s) if s == "error" => Self::Error,
            _ => Self::Unknown,
        }
    }

    /// Short label for the top bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Recording => "● Recording",
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
        let this = self.clone();
        off_thread(Err("could not run rsrewind".to_string()), move || {
            let Some(mut command) = this.command(args) else {
                return Err("could not find the rsrewind executable".into());
            };
            // `start` and `tray` leave a background process behind; it must not hold our pipes.
            let output = command
                .stdout(Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            if output.status.success() {
                Ok(())
            } else {
                let err = String::from_utf8_lossy(&output.stderr);
                Err(err
                    .lines()
                    .last()
                    .unwrap_or("it did not work")
                    .trim()
                    .to_string())
            }
        })
    }
}

/// Runs blocking work on its own thread and awaits the result without blocking the UI; `fallback`
/// if the thread cannot start or vanishes.
fn off_thread<T: Send + 'static>(
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
}
