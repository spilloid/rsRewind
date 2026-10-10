//! The tray icon on Windows: a notification-area icon (`Shell_NotifyIconW`) owned by a hidden
//! window on this thread, with a popup menu built from [`crate::model::menu`].
//!
//! A worker thread runs the `rsrewind` commands (actions and the status poll) and wakes the window
//! with [`WM_STATUS`] when a new status is ready, so the message loop never waits on a child
//! process. `unsafe` is confined to the Windows API calls, each with its own `SAFETY` note.

use crate::icons::Icons;
use crate::model::{self, Action, Armed, Click, Item, Status};
use crate::{POLL, Runner};
use std::cell::RefCell;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics,
    HICON, ICONINFO, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SetForegroundWindow, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL,
    WM_RBUTTONUP, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

/// Mouse events on the icon.
const WM_TRAY: u32 = WM_APP + 1;
/// A new status is waiting in the channel.
const WM_STATUS: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
const CLASS: PCWSTR = w!("rsRewindTray");

struct State {
    hwnd: HWND,
    icons: Icons,
    status: Status,
    armed: Option<Armed>,
    actions: Sender<Action>,
    statuses: Receiver<Status>,
    hicon: Option<HICON>,
    /// Explorer broadcasts this when the taskbar is recreated; the icon must then be added again.
    taskbar_created: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Shows the icon until "Quit" from its menu.
pub fn run(runner: &Runner) -> anyhow::Result<()> {
    // SAFETY: a null module name returns this executable's handle; nothing is freed.
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: CLASS,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and its strings are 'static; registering twice in one
    // process just fails, which `CreateWindowExW` below then reports.
    unsafe { RegisterClassW(&class) };
    // SAFETY: the class exists; a zero-sized, never-shown window only receives messages. It is not
    // message-only (HWND_MESSAGE), because those miss the TaskbarCreated broadcast.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            w!("rsRewind"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }?;
    // SAFETY: the string is a 'static wide literal.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };

    let (actions, inbox) = channel::<Action>();
    let (status_tx, statuses) = channel::<Status>();
    let first = runner.status();
    STATE.with(|cell| {
        *cell.borrow_mut() = Some(State {
            hwnd,
            icons: Icons::load().map_err(|e| anyhow::anyhow!("tray artwork: {e}"))?,
            status: first,
            armed: None,
            actions,
            statuses,
            hicon: None,
            taskbar_created,
        });
        anyhow::Ok(())
    })?;
    with_state(|s| s.show(NIM_ADD));

    let worker_runner = runner.clone();
    // HWND is a raw pointer (not Send); PostMessageW to it is safe from any thread.
    let hwnd_bits = hwnd.0 as usize;
    let worker = std::thread::Builder::new()
        .name("rsrewind-tray-worker".into())
        .spawn(move || {
            loop {
                match inbox.recv_timeout(POLL) {
                    Ok(Action::Quit) | Err(RecvTimeoutError::Disconnected) => break,
                    Ok(action) => worker_runner.perform(action),
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if status_tx.send(worker_runner.status()).is_err() {
                    break;
                }
                // SAFETY: posting to a window that may already be gone just fails.
                let posted = unsafe {
                    PostMessageW(
                        Some(HWND(hwnd_bits as *mut _)),
                        WM_STATUS,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
                if posted.is_err() {
                    break;
                }
            }
        })?;

    let mut msg = MSG::default();
    // SAFETY: standard message loop on the thread that owns `hwnd`; GetMessageW returns 0 on
    // WM_QUIT and -1 on error, both of which end the loop.
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        // SAFETY: `msg` was just filled by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Dropping the state closes the action channel, which ends the worker.
    STATE.with(|cell| cell.borrow_mut().take());
    let _ = worker.join();
    Ok(())
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

impl State {
    /// Adds or updates the notification icon for the current status.
    fn show(&mut self, how: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE) {
        // SAFETY: a plain metrics query.
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.clamp(16, 128) as u32;
        let (art_size, argb) = self.icons.nearest(&self.status, size);
        let new_icon = make_icon(art_size, &argb);
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            uFlags: NIF_MESSAGE
                | NIF_TIP
                | if new_icon.is_some() {
                    NIF_ICON
                } else {
                    Default::default()
                },
            uCallbackMessage: WM_TRAY,
            hIcon: new_icon.unwrap_or_default(),
            ..Default::default()
        };
        let tip: Vec<u16> = format!("rsRewind: {}", self.status.headline())
            .encode_utf16()
            .take(data.szTip.len() - 1)
            .collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        // SAFETY: `data` is fully initialised and outlives the call; the shell copies the icon.
        let ok = unsafe { Shell_NotifyIconW(how, &data) }.as_bool();
        if !ok {
            tracing::warn!("Shell_NotifyIconW failed");
        }
        if let Some(icon) = new_icon
            && let Some(old) = self.hicon.replace(icon)
        {
            // SAFETY: `old` was created by make_icon and the shell no longer shows it.
            let _ = unsafe { DestroyIcon(old) };
        }
    }

    fn remove(&mut self) {
        let data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: ICON_ID,
            ..Default::default()
        };
        // SAFETY: as in `show`.
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        if let Some(icon) = self.hicon.take() {
            // SAFETY: created by make_icon; the notification icon is gone.
            let _ = unsafe { DestroyIcon(icon) };
        }
    }

    /// Menu choice `id` (1-based index into the menu for the current state).
    fn choose(&mut self, id: usize) {
        let items = model::menu(&self.status, self.armed, Instant::now());
        let Some(Item::Button {
            action,
            enabled: true,
            ..
        }) = id.checked_sub(1).and_then(|i| items.get(i)).cloned()
        else {
            return;
        };
        match model::click(action, self.armed, Instant::now()) {
            Click::Arm(armed) => self.armed = Some(armed),
            Click::Run(Action::Quit) => {
                self.remove();
                // SAFETY: our own window, on its own thread.
                let _ = unsafe { DestroyWindow(self.hwnd) };
            }
            Click::Run(action) => {
                self.armed = None;
                let _ = self.actions.send(action);
            }
        }
    }

    fn popup(&mut self) {
        let items = model::menu(&self.status, self.armed, Instant::now());
        // SAFETY: a new, empty menu that this function destroys before returning.
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        for (index, item) in items.iter().enumerate() {
            let id = index + 1;
            let (flags, label) = match item {
                Item::Separator => (MF_SEPARATOR, None),
                Item::Label(text) => (MF_STRING | MF_GRAYED, Some(text)),
                Item::Button { label, enabled, .. } => (
                    if *enabled {
                        MF_STRING
                    } else {
                        MF_STRING | MF_GRAYED
                    },
                    Some(label),
                ),
            };
            let wide: Vec<u16> = label
                .map(|l| l.encode_utf16().chain([0]).collect())
                .unwrap_or_default();
            let text = if wide.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(wide.as_ptr())
            };
            // SAFETY: `wide` is NUL-terminated and outlives the call (the menu copies it).
            let _ = unsafe { AppendMenuW(menu, flags, id, text) };
        }
        let mut at = POINT::default();
        // SAFETY: writes the cursor position into `at`.
        let _ = unsafe { GetCursorPos(&mut at) };
        // SAFETY: the documented dance for notification-area menus: make our window foreground so
        // the menu closes when clicking elsewhere, track it, then post a null message.
        let chosen = unsafe {
            let _ = SetForegroundWindow(self.hwnd);
            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                at.x,
                at.y,
                None,
                self.hwnd,
                None,
            )
            .0;
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            chosen
        };
        if chosen > 0 {
            self.choose(chosen as usize);
        }
    }
}

/// An HICON from `size`x`size` ARGB32 (network byte order), or `None` if Windows refuses.
fn make_icon(size: u32, argb: &[u8]) -> Option<HICON> {
    let n = size as usize;
    if argb.len() < n * n * 4 || n == 0 {
        return None;
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            // Negative height: rows top-down, as in `argb`.
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `info` describes a 32-bpp top-down DIB; Windows allocates `bits` for it.
    let color =
        unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }.ok()?;
    if bits.is_null() {
        // SAFETY: created just above.
        let _ = unsafe { DeleteObject(color.into()) };
        return None;
    }
    // SAFETY: the DIB section is exactly n*n 32-bit pixels and nothing else references it yet.
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), n * n * 4) };
    for (dst, src) in pixels
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(argb.as_chunks::<4>().0)
    {
        *dst = [src[3], src[2], src[1], src[0]]; // B, G, R, A
    }
    // An all-zero AND mask: transparency comes from the colour bitmap's alpha.
    let mask_bits = vec![0u8; n.div_ceil(16) * 2 * n];
    // SAFETY: a 1-bpp bitmap of n*n bits, rows padded to 16 bits, read from `mask_bits`.
    let mask = unsafe {
        CreateBitmap(
            size as i32,
            size as i32,
            1,
            1,
            Some(mask_bits.as_ptr().cast()),
        )
    };
    let info = ICONINFO {
        fIcon: true.into(),
        hbmMask: mask,
        hbmColor: color,
        ..Default::default()
    };
    // SAFETY: both bitmaps are valid; the icon copies them, so they are deleted afterwards.
    let icon = unsafe { CreateIconIndirect(&info) }.ok();
    // SAFETY: as above.
    unsafe {
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
    }
    icon
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam.0 as u32 {
                WM_LBUTTONUP => {
                    with_state(|s| s.actions.send(Action::OpenWindow).ok());
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    with_state(State::popup);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_STATUS => {
            with_state(|s| {
                let mut latest = None;
                while let Ok(status) = s.statuses.try_recv() {
                    latest = Some(status);
                }
                if let Some(status) = latest {
                    s.status = status;
                    // Also refreshes an expired "click again" label (the menu is rebuilt on open).
                    s.show(NIM_MODIFY);
                }
            });
            LRESULT(0)
        }
        // Menu ids posted directly (scripted checks use this; the popup returns its choice).
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            with_state(|s| s.choose(id));
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ if with_state(|s| s.taskbar_created == msg && msg != 0) == Some(true) => {
            with_state(|s| s.show(NIM_ADD));
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
