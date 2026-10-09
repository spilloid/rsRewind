//! Time since the last user input on a Wayland desktop, from the compositor's
//! `ext-idle-notify-v1` protocol (KWin, Sway, Hyprland, Mutter 48+ and others).
//!
//! The protocol reports transitions, not a counter: "idle for at least N ms" and "input again". A
//! thread owns the Wayland connection and records when the seat went idle; [`WaylandIdle::idle_millis`]
//! turns that into a duration. If the connection dies, or the compositor lacks the protocol, the
//! answer is `None` (unknown), which the recorder treats as idle.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    self, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

/// The notification threshold. Idle time is known to this resolution.
const THRESHOLD: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
struct Shared {
    /// Set when the compositor said "idle", cleared on "resumed".
    idle_since: Option<Instant>,
    /// False once the listener thread has stopped for any reason.
    alive: bool,
}

/// A running idle listener.
#[derive(Debug, Clone)]
pub struct WaylandIdle {
    shared: Arc<Mutex<Shared>>,
}

impl WaylandIdle {
    /// Connects to the compositor (`WAYLAND_DISPLAY`) and starts listening. `Err` names why idle
    /// time is unavailable.
    pub fn start() -> Result<Self, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("connect to Wayland: {e}"))?;
        let (globals, mut queue) =
            registry_queue_init::<Listener>(&conn).map_err(|e| format!("Wayland registry: {e}"))?;
        let qh = queue.handle();
        let seat: wl_seat::WlSeat = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| format!("no Wayland seat: {e}"))?;
        let notifier: ExtIdleNotifierV1 = globals
            .bind(&qh, 1..=1, ())
            .map_err(|e| format!("the compositor does not offer ext-idle-notify-v1: {e}"))?;
        let millis = u32::try_from(THRESHOLD.as_millis()).unwrap_or(u32::MAX);
        let notification = notifier.get_idle_notification(millis, &seat, &qh, ());
        let shared = Arc::new(Mutex::new(Shared {
            idle_since: None,
            alive: true,
        }));
        let mut listener = Listener {
            shared: shared.clone(),
        };
        std::thread::Builder::new()
            .name("rsrewind-idle".into())
            .spawn(move || {
                // Kept alive for as long as the loop runs.
                let _objects = (seat, notifier, notification);
                let error = loop {
                    if let Err(error) = queue.blocking_dispatch(&mut listener) {
                        break error;
                    }
                };
                tracing::warn!(%error, "Wayland idle listener stopped; idle time is now unknown");
                if let Ok(mut shared) = listener.shared.lock() {
                    shared.alive = false;
                }
            })
            .map_err(|e| format!("start the idle thread: {e}"))?;
        Ok(Self { shared })
    }

    /// Milliseconds since the last input, to [`THRESHOLD`] resolution; `None` when unknown.
    pub fn idle_millis(&self) -> Option<u64> {
        let shared = self.shared.lock().ok()?;
        if !shared.alive {
            return None;
        }
        Some(match shared.idle_since {
            None => 0,
            Some(since) => {
                let idle = THRESHOLD + since.elapsed();
                u64::try_from(idle.as_millis()).unwrap_or(u64::MAX)
            }
        })
    }
}

struct Listener {
    shared: Arc<Mutex<Shared>>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Listener {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Listener {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for Listener {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for Listener {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Ok(mut shared) = state.shared.lock() else {
            return;
        };
        match event {
            ext_idle_notification_v1::Event::Idled => shared.idle_since = Some(Instant::now()),
            ext_idle_notification_v1::Event::Resumed => shared.idle_since = None,
            _ => {}
        }
    }
}
