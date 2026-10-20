//! Process-level Windows plumbing: single instance, stop signal, Ctrl+C, thread priority.

use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, GetCurrentThread, OpenEventW, OpenProcess,
    PROCESS_SYNCHRONIZE, SetEvent, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
    WaitForSingleObject,
};
use windows::core::{BOOL, HSTRING, PCWSTR, w};

/// `Local\` scopes both objects to the interactive session: recording belongs to the user's own
/// desktop, and another user's recorder on the same machine is not ours to stop.
const INSTANCE_MUTEX: PCWSTR = w!("Local\\rsRewind.Recorder");
/// The one stop event per session that recorders up to 0.1.2 created. Recorders from this version
/// on never create it, so signalling it can only reach such an older recorder.
const LEGACY_STOP_EVENT: PCWSTR = w!("Local\\rsRewind.Stop");

static CTRL_STOP: AtomicBool = AtomicBool::new(false);

/// The stop event of the recorder with process id `pid`. Bound to that one process, so a stop
/// aimed at a recorder that has since exited cannot reach a recorder started after it (astra
/// review, 2026-10-19): the event goes away with its process, and another recorder's event has
/// another name.
fn stop_event_name(pid: u32) -> HSTRING {
    HSTRING::from(format!("Local\\rsRewind.Stop.{pid}"))
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

/// Creates (auto-reset, unsignalled) the stop event named for `pid`.
fn create_stop_event(pid: u32) -> windows::core::Result<OwnedHandle> {
    let name = stop_event_name(pid);
    // SAFETY: `name` is a valid NUL-terminated wide string that outlives the call.
    unsafe {
        Ok(OwnedHandle(CreateEventW(
            None,
            false,
            false,
            PCWSTR(name.as_ptr()),
        )?))
    }
}

/// Holds the single-instance mutex for the lifetime of the recorder.
pub struct InstanceGuard {
    _mutex: OwnedHandle,
    stop_event: OwnedHandle,
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
        // Created before the first heartbeat and closed only when the process exits, after the
        // last one: while this recorder's heartbeat is live and names its pid, the event exists.
        let stop_event = create_stop_event(std::process::id())?;
        Ok(Some(Self {
            _mutex: mutex,
            stop_event,
            stop_seen: AtomicBool::new(false),
        }))
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

/// Signals the recorder with process id `pid` (read from its data folder's heartbeat) to stop.
/// `Ok(false)` if no recorder of this version or later with that pid runs in this session.
pub fn signal_stop(pid: u32) -> windows::core::Result<bool> {
    let name = stop_event_name(pid);
    set_named_event(PCWSTR(name.as_ptr()))
}

/// Signals the session-wide stop event of a recorder from 0.1.2 or earlier, which named no pid.
/// Only such a recorder can receive it. `Ok(false)` if none runs in this session.
pub fn signal_legacy_stop() -> windows::core::Result<bool> {
    set_named_event(LEGACY_STOP_EVENT)
}

/// Whether a process with this id is running (and visible to us).
pub fn process_alive(pid: u32) -> bool {
    // SAFETY: OpenProcess with a plain access mask; the handle is closed by OwnedHandle; a zero
    // timeout never blocks.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
            return false;
        };
        let process = OwnedHandle(process);
        WaitForSingleObject(process.0, 0) == WAIT_TIMEOUT
    }
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

    /// The stop signal reaches only the recorder whose pid was read from the folder's heartbeat.
    /// The pids are ones no process can have (Windows pids are multiples of 4), so this cannot
    /// touch a recorder running on this machine, nor the session-wide event older recorders use.
    #[test]
    fn a_stop_reaches_only_the_recorder_it_names() -> windows::core::Result<()> {
        let (exited, running) = (u32::MAX - 2, u32::MAX - 6);
        let running_event = create_stop_event(running)?;
        // The recorder that was checked has exited (its event is gone) and another one runs. A
        // stop meant for the first must not reach the second: with one event per session, it did.
        assert!(!signal_stop(exited)?);
        assert!(!signalled(&running_event));
        assert!(signal_stop(running)?);
        assert!(signalled(&running_event));
        Ok(())
    }

    #[test]
    fn only_a_running_process_is_alive() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(u32::MAX - 2));
    }
}
