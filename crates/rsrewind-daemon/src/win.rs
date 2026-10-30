//! Process-level Windows plumbing: single instance, stop signal, Ctrl+C, thread priority.

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, GetCurrentThread, OpenEventW, SetEvent,
    SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, WaitForSingleObject,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

/// `Local\` scopes both objects to the interactive session: recording belongs to the user's own
/// desktop, and another user's recorder on the same machine is not ours to stop.
const INSTANCE_MUTEX: PCWSTR = w!("Local\\rsRewind.Recorder");
/// The one stop event per session that recorders up to 0.1.2 created. Recorders from this version
/// on never create it, so signalling it can only reach such an older recorder, whichever folder it
/// records into. `rsrewind stop` uses it only when asked to (`--legacy-session`).
const LEGACY_STOP_EVENT: PCWSTR = w!("Local\\rsRewind.Stop");

static CTRL_STOP: AtomicBool = AtomicBool::new(false);

/// The stop event of one run of a recorder, named for the random token that run publishes in its
/// heartbeat. A stop aimed at a run that has since exited cannot reach any other run (astra
/// review, 2026-10-19): the event goes away with its process, and no other run has its name.
fn stop_event_name(token: u64) -> HSTRING {
    HSTRING::from(format!("Local\\rsRewind.Stop.{token:016x}"))
}

/// A fresh random token: std's `RandomState` is seeded from the OS's random source, mixed with
/// the pid and the time. Uniqueness, not secrecy, is the point: anyone who can read the heartbeat
/// can stop the recorder anyway.
fn random_token() -> u64 {
    use std::hash::BuildHasher;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::collections::hash_map::RandomState::new().hash_one((std::process::id(), now))
}

/// Owns a kernel handle and closes it on drop.
pub struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own this handle (created by us, never duplicated or closed elsewhere).
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// SAFETY: kernel object handles are process-wide and usable from any thread.
unsafe impl Send for OwnedHandle {}

/// Creates (auto-reset, unsignalled) the stop event for `token`. `Ok(None)` if an event of that
/// name already exists, so a token is never shared with another run.
fn create_stop_event(token: u64) -> windows::core::Result<Option<OwnedHandle>> {
    let name = stop_event_name(token);
    // SAFETY: `name` is a valid NUL-terminated wide string that outlives the call; GetLastError is
    // read immediately after CreateEventW.
    unsafe {
        let event = OwnedHandle(CreateEventW(None, false, false, PCWSTR(name.as_ptr()))?);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return Ok(None);
        }
        Ok(Some(event))
    }
}

/// Holds the single-instance mutex for the lifetime of the recorder.
pub struct InstanceGuard {
    _mutex: OwnedHandle,
    stop_event: OwnedHandle,
    stop_token: u64,
    /// The stop event is auto-reset, so the first wait that sees it also clears it. Latch it,
    /// or a check inside the sleep loop swallows the signal and the outer loop never stops.
    stop_seen: AtomicBool,
}

impl InstanceGuard {
    /// `Ok(None)` when another recorder already owns the session.
    pub fn acquire() -> windows::core::Result<Option<Self>> {
        // SAFETY: plain Win32 calls with static, NUL-terminated names; GetLastError is read
        // immediately after CreateMutexW, before any other API call can overwrite it.
        let mutex = unsafe {
            let mutex = CreateMutexW(None, true, INSTANCE_MUTEX)?;
            let already = GetLastError() == ERROR_ALREADY_EXISTS;
            let mutex = OwnedHandle(mutex);
            if already {
                return Ok(None);
            }
            mutex
        };
        // Created before the first heartbeat (which publishes the token) and closed only when the
        // process exits, after the last one: while the heartbeat is live, the event exists.
        loop {
            let token = random_token();
            if let Some(stop_event) = create_stop_event(token)? {
                return Ok(Some(Self {
                    _mutex: mutex,
                    stop_event,
                    stop_token: token,
                    stop_seen: AtomicBool::new(false),
                }));
            }
        }
    }

    /// This run's stop token, published in the heartbeat.
    pub fn stop_token(&self) -> u64 {
        self.stop_token
    }

    /// True once `rsrewind stop` signalled the event or Ctrl+C / console close arrived.
    pub fn stop_requested(&self) -> bool {
        if CTRL_STOP.load(Ordering::Relaxed) || self.stop_seen.load(Ordering::Relaxed) {
            return true;
        }
        // SAFETY: valid event handle owned by self; zero timeout never blocks.
        let signalled = unsafe { WaitForSingleObject(self.stop_event.0, 0) == WAIT_OBJECT_0 };
        if signalled {
            self.stop_seen.store(true, Ordering::Relaxed);
        }
        signalled
    }
}

/// Sets the named event if it exists. `Ok(false)` if no process holds an event of that name.
fn set_named_event(name: PCWSTR) -> windows::core::Result<bool> {
    // SAFETY: OpenEventW with a valid NUL-terminated name; the handle is closed by OwnedHandle.
    unsafe {
        let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, name) else {
            return Ok(false);
        };
        let event = OwnedHandle(event);
        SetEvent(event.0)?;
        Ok(true)
    }
}

/// Signals the recorder run whose heartbeat published `token` to stop. `Ok(false)` if that run is
/// not running (any more) in this session.
pub fn signal_stop(token: u64) -> windows::core::Result<bool> {
    let name = stop_event_name(token);
    set_named_event(PCWSTR(name.as_ptr()))
}

/// Signals the session-wide stop event of recorders from 0.1.2 or earlier. It reaches whichever
/// such recorder runs in this session at that moment, whatever folder it records into.
/// `Ok(false)` if none runs.
pub fn signal_legacy_stop() -> windows::core::Result<bool> {
    set_named_event(LEGACY_STOP_EVENT)
}

unsafe extern "system" fn on_console_ctrl(_ctrl_type: u32) -> BOOL {
    CTRL_STOP.store(true, Ordering::Relaxed);
    // Handled: let the capture loop shut down cleanly instead of being killed mid-write. Windows
    // still terminates the process after a few seconds on console close, which is why every write
    // is ordered so that an abrupt stop leaves at most an orphan file.
    BOOL(1)
}

pub fn install_ctrl_handler() -> windows::core::Result<()> {
    // SAFETY: registers a `'static` handler that only stores to an atomic.
    unsafe { SetConsoleCtrlHandler(Some(on_console_ctrl), true) }
}

pub fn lower_current_thread_priority() {
    // SAFETY: GetCurrentThread returns a pseudo-handle that needs no closing.
    unsafe {
        if let Err(error) = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) {
            tracing::debug!(%error, "could not lower thread priority");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signalled(event: &OwnedHandle) -> bool {
        // SAFETY: a valid event handle we own; zero timeout never blocks.
        unsafe { WaitForSingleObject(event.0, 0) == WAIT_OBJECT_0 }
    }

    /// The stop signal reaches only the run whose token the folder's heartbeat published. Fresh
    /// random tokens, so this cannot touch a recorder running on this machine, nor the
    /// session-wide event older recorders use.
    #[test]
    fn a_stop_reaches_only_the_run_it_names() -> Result<(), Box<dyn std::error::Error>> {
        let (exited, running) = (random_token(), random_token());
        assert_ne!(exited, running);
        let running_event = create_stop_event(running)?.ok_or("token already in use")?;
        // The run that was checked has exited (its event is gone) and another one runs. A stop
        // meant for the first must not reach the second: with one event per session, it did.
        assert!(!signal_stop(exited)?);
        assert!(!signalled(&running_event));
        assert!(signal_stop(running)?);
        assert!(signalled(&running_event));
        // A second run can never take a token that is in use.
        assert!(create_stop_event(running)?.is_none());
        Ok(())
    }
}
