//! The tray icon on Linux: a StatusNotifierItem over the session bus (KDE Plasma natively; most
//! other desktops through their tray applet), via `ksni`.

use crate::icons::Icons;
use crate::model::{self, Action, Armed, Click, Item, Status};
use crate::{POLL, Runner};
use ksni::blocking::TrayMethods;
use std::sync::mpsc::{Sender, channel};
use std::time::Instant;

struct RsTray {
    icons: Icons,
    status: Status,
    armed: Option<Armed>,
    actions: Sender<Action>,
}

impl RsTray {
    fn clicked(&mut self, action: Action) {
        match model::click(action, self.armed, Instant::now()) {
            Click::Arm(armed) => self.armed = Some(armed),
            Click::Run(action) => {
                self.armed = None;
                let _ = self.actions.send(action);
            }
        }
    }
}

impl ksni::Tray for RsTray {
    fn id(&self) -> String {
        "rsrewind".into()
    }

    fn title(&self) -> String {
        "rsRewind".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons
            .for_status(&self.status)
            .into_iter()
            .map(|(size, data)| ksni::Icon {
                width: size as i32,
                height: size as i32,
                data,
            })
            .collect()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "rsRewind".into(),
            description: self.status.headline(),
            ..Default::default()
        }
    }

    /// Left click opens the window; the menu is on right click.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send(Action::OpenWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        model::menu(&self.status, self.armed, Instant::now())
            .into_iter()
            .map(|item| match item {
                Item::Label(label) => ksni::menu::StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                Item::Separator => ksni::MenuItem::Separator,
                Item::Button {
                    label,
                    action,
                    enabled,
                } => ksni::menu::StandardItem {
                    label,
                    enabled,
                    activate: Box::new(move |tray: &mut Self| tray.clicked(action)),
                    ..Default::default()
                }
                .into(),
            })
            .collect()
    }
}

/// Shows the icon until "Quit" (or the session bus goes away).
pub fn run(runner: &Runner) -> anyhow::Result<()> {
    let (actions, inbox) = channel();
    let tray = RsTray {
        icons: Icons::load().map_err(|e| anyhow::anyhow!("tray artwork: {e}"))?,
        status: runner.status(),
        armed: None,
        actions,
    };
    let handle = tray
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not show the tray icon: {e}"))?;
    loop {
        match inbox.recv_timeout(POLL) {
            Ok(Action::Quit) => break,
            Ok(action) => runner.perform(action),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let status = runner.status();
        // Also refreshes an expired "click again" label back to its normal text.
        if handle.update(|tray| tray.status = status).is_none() {
            break;
        }
    }
    handle.shutdown().wait();
    Ok(())
}
