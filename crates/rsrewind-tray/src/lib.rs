//! `rsrewind tray`: a notification-area icon that shows whether rsRewind is recording and offers
//! pause, resume, forget, open-the-window and start/stop.
//!
//! It runs as its own process and acts only through the `rsrewind` command line (see
//! [`model`]): it opens no database and holds no capture, so closing or crashing it changes
//! nothing about recording.

#[cfg(target_os = "linux")]
mod icons;
pub mod model;

#[cfg(target_os = "linux")]
mod linux;

use model::{Action, Status};
use rsrewind_core::Timestamp;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How often the icon re-reads the recorder's state.
pub(crate) const POLL: Duration = Duration::from_secs(3);

/// Runs `rsrewind` subcommands against one data folder.
pub struct Runner {
    pub exe: PathBuf,
    pub data_dir: PathBuf,
}

impl Runner {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.exe);
        command
            .arg("--data-dir")
            .arg(&self.data_dir)
            .stdin(Stdio::null());
        command
    }

    /// The recorder's state; [`Status::UNKNOWN`] if the status command fails.
    pub fn status(&self) -> Status {
        let output = self
            .command()
            .args(["status", "--json"])
            .stderr(Stdio::null())
            .output();
        match output {
            Ok(out) => Status::parse(&String::from_utf8_lossy(&out.stdout), |ms| {
                Timestamp(ms).to_local().format("%H:%M").to_string()
            }),
            Err(error) => {
                tracing::warn!(%error, "could not run `rsrewind status`");
                Status::UNKNOWN
            }
        }
    }

    /// Carries out a menu action by running the matching `rsrewind` subcommand and waiting for it.
    pub fn perform(&self, action: Action) {
        let Some(args) = action.cli_args() else {
            return;
        };
        let result = self
            .command()
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match result {
            Ok(status) if status.success() => tracing::info!(?action, "done"),
            Ok(status) => tracing::warn!(?action, code = ?status.code(), "command failed"),
            Err(error) => tracing::warn!(?action, %error, "could not run rsrewind"),
        }
    }
}

/// Shows the tray icon until the user quits it.
#[cfg(target_os = "linux")]
pub fn run(runner: &Runner) -> anyhow::Result<()> {
    linux::run(runner)
}

#[cfg(not(target_os = "linux"))]
pub fn run(_: &Runner) -> anyhow::Result<()> {
    anyhow::bail!("the tray icon is available on Linux so far; Windows and macOS are next")
}
