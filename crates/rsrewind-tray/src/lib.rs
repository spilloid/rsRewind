//! `rsrewind tray`: a notification-area icon that shows whether rsRewind is recording and offers
//! pause, resume, forget, open-the-window and start/stop.
//!
//! It runs as its own process and acts only through the `rsrewind` command line (see
//! [`model`]): it opens no database and holds no capture, so a crash or a kill of the icon changes
//! nothing about recording. Choosing **Quit** from its menu is different: that means "stop
//! rsRewind", so it stops the recorder first (see [`Runner::quit`]).

#[cfg(any(target_os = "linux", windows))]
mod icons;
pub mod model;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use model::{Action, Status};
use rsrewind_core::Timestamp;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// How often the icon re-reads the recorder's state.
pub(crate) const POLL: Duration = Duration::from_secs(3);

/// Runs `rsrewind` subcommands against one data folder.
#[derive(Debug, Clone)]
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
        // The tray has no console; without this every status poll would flash a console window.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
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
    /// True if there was nothing to run or the command succeeded.
    pub fn perform(&self, action: Action) -> bool {
        let Some(args) = action.cli_args() else {
            return true;
        };
        let result = self
            .command()
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match result {
            Ok(status) if status.success() => {
                tracing::info!(?action, "done");
                true
            }
            Ok(status) => {
                tracing::warn!(?action, code = ?status.code(), "command failed");
                false
            }
            Err(error) => {
                tracing::warn!(?action, %error, "could not run rsrewind");
                false
            }
        }
    }

    /// The **Quit** menu item: closing the icon means stopping rsRewind, on every platform, so recording
    /// never continues with nothing on screen to show it. Stops the recorder and returns true once it
    /// is no longer running. If it could not be stopped this returns false and the caller must keep the
    /// icon up, because the icon is how you see that recording is still on.
    pub fn quit(&self) -> bool {
        if self.perform(Action::StopRecorder) {
            return true;
        }
        // `stop` also fails when there was nothing to stop; the recorder's own state decides.
        matches!(self.status().recorder, model::Recorder::NotRunning)
    }
}

/// Shows the tray icon until the user quits it.
#[cfg(target_os = "linux")]
pub fn run(runner: &Runner) -> anyhow::Result<()> {
    linux::run(runner)
}

/// Shows the tray icon until the user quits it.
#[cfg(windows)]
pub fn run(runner: &Runner) -> anyhow::Result<()> {
    windows::run(runner)
}

#[cfg(target_os = "macos")]
pub fn run(runner: &Runner) -> anyhow::Result<()> {
    macos::run(runner)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn run(_: &Runner) -> anyhow::Result<()> {
    anyhow::bail!("the tray icon is available on Linux, Windows and macOS")
}

// Compile the bridge model on other hosts too; tests exercise only its pure menu snapshot,
// never the AppKit FFI entry points. Native event-loop acceptance remains a macOS check.
#[cfg(all(test, not(target_os = "macos")))]
#[allow(dead_code)]
#[path = "macos.rs"]
mod macos_validation;

#[cfg(all(test, unix))]
mod quit_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in `rsrewind` whose `stop` exits with `stop_exit` and whose `status --json` reports `state`.
    fn fake(
        dir: &std::path::Path,
        name: &str,
        stop_exit: i32,
        state: &str,
    ) -> Result<Runner, Box<dyn std::error::Error>> {
        let exe = dir.join(name);
        // The tray passes `--data-dir <dir>` first, so the subcommand is the third argument.
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\ncase \"$3\" in\n  stop) exit {stop_exit} ;;\n  status) echo '{{\"state\":\"{state}\"}}' ;;\nesac\n"
            ),
        )?;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
        Ok(Runner {
            exe,
            data_dir: dir.to_path_buf(),
        })
    }

    #[test]
    fn quit_stops_the_recorder_and_only_closes_the_icon_when_it_is_off()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("rsrewind-tray-quit-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        // `stop` worked: the icon may go.
        assert!(fake(&dir, "stopped", 0, "recording")?.quit());
        // `stop` failed and the recorder is still running: the icon must stay and keep saying so.
        assert!(!fake(&dir, "stuck", 1, "recording")?.quit());
        assert!(!fake(&dir, "stuck-paused", 1, "paused")?.quit());
        // `stop` failed because there was nothing to stop: nothing is recording, so the icon may go.
        assert!(fake(&dir, "already-off", 1, "not_running")?.quit());
        // `stop` failed and the state cannot be read: not provably off, so the icon stays.
        assert!(!fake(&dir, "unknown", 1, "garbage")?.quit());
        // The menu item says what it does.
        assert!(model::menu(
            &Status::UNKNOWN,
            None,
            std::time::Instant::now()
        )
        .iter()
        .any(|item| matches!(item, model::Item::Button { label, action: Action::Quit, .. } if label == "Quit rsRewind (stops recording)")));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
