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
use windows::core::{BOOL, w};

/// `Local\` scopes both objects to the interactive session: recording belongs to the user's own
/// desktop, and another user's recorder on the same machine is not ours to stop.
const INSTANCE_MUTEX: windows::core::PCWSTR = w!("Local\\rsRewind.Recorder");
const STOP_EVENT: windows::core::PCWSTR = w!("Local\\rsRewind.Stop");

static CTRL_STOP: AtomicBool = AtomicBool::new(false);

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
        unsafe {
            let mutex = CreateMutexW(None, true, INSTANCE_MUTEX)?;
            let already = GetLastError() == ERROR_ALREADY_EXISTS;
            let mutex = OwnedHandle(mutex);
            if already {
                return Ok(None);
            }
            let stop_event = OwnedHandle(CreateEventW(None, false, false, STOP_EVENT)?);
            Ok(Some(Self {
                _mutex: mutex,
                stop_event,
                stop_seen: AtomicBool::new(false),
            }))
        }
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

/// Signals a running recorder to stop. `Ok(false)` if none is running in this session.
pub fn signal_stop() -> windows::core::Result<bool> {
    // SAFETY: OpenEventW with a static name; the handle is closed by OwnedHandle.
    unsafe {
        let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, STOP_EVENT) else {
            return Ok(false);
        };
        let event = OwnedHandle(event);
        SetEvent(event.0)?;
        Ok(true)
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
