//! Wheel-over-tray-icon input.
//!
//! Windows delivers wheel events to the focused window, and the notification
//! area belongs to Explorer, so a tray icon never sees `WM_MOUSEWHEEL` through
//! its own callback. The only way to read the wheel over a tray icon is a
//! global `WH_MOUSE_LL` hook that inspects every wheel event and keeps the ones
//! whose cursor position falls inside our icon.
//!
//! The icon's screen rectangle comes from the tray's own hover events, which
//! `tray-icon` reports with the rect Explorer gave it, so it stays correct
//! across DPI changes and taskbar moves.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};

use log::{debug, warn};
use tauri::tray::TrayIconEvent;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, MSG, MSLLHOOKSTRUCT,
    SetWindowsHookExW, TranslateMessage, WH_MOUSE_LL, WM_MOUSEWHEEL,
};

use crate::monitor::Command;

/// One wheel notch, as reported by Windows.
const WHEEL_DELTA: i32 = 120;

/// The icon rect in physical screen pixels: (left, top, right, bottom).
static TRAY_RECT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
/// Whether the cursor is currently inside the icon, per the tray's own events.
static HOVERING: AtomicBool = AtomicBool::new(false);
/// Leftover wheel delta, so high-resolution wheels accumulate into notches.
static REMAINDER: AtomicI32 = AtomicI32::new(0);
/// Where accepted notches go. Set once, before the hook is installed.
static COMMANDS: OnceLock<Sender<Command>> = OnceLock::new();

/// Feeds tray hover events in so the hook knows where the icon is.
pub fn track_tray_event(event: &TrayIconEvent) {
    match event {
        TrayIconEvent::Enter { rect, .. } | TrayIconEvent::Move { rect, .. } => {
            let position = rect.position.to_physical::<f64>(1.0);
            let size = rect.size.to_physical::<f64>(1.0);
            let left = position.x.round() as i32;
            let top = position.y.round() as i32;
            let bounds = (
                left,
                top,
                left + size.width.round() as i32,
                top + size.height.round() as i32,
            );

            if let Ok(mut guard) = TRAY_RECT.lock() {
                *guard = Some(bounds);
            }
            HOVERING.store(true, Ordering::Relaxed);
        }
        TrayIconEvent::Leave { .. } => {
            HOVERING.store(false, Ordering::Relaxed);
            // Any partial notch belongs to this hover, not the next one.
            REMAINDER.store(0, Ordering::Relaxed);
        }
        _ => {}
    }
}

/// Installs the global wheel hook on a thread of its own.
///
/// A low-level hook is invoked on the thread that installed it, and that thread
/// must keep pumping messages or Windows silently unhooks it. Giving it a
/// dedicated thread keeps it clear of Tauri's event loop, which can block on
/// long-running UI work.
pub fn install(commands: Sender<Command>) {
    if COMMANDS.set(commands).is_err() {
        warn!("scroll hook already installed");
        return;
    }

    std::thread::Builder::new()
        .name("scroll-hook".into())
        .spawn(|| unsafe {
            let module = match GetModuleHandleW(None) {
                Ok(module) => module,
                Err(e) => {
                    warn!("could not resolve module handle: {e}");
                    return;
                }
            };

            let hook = match SetWindowsHookExW(WH_MOUSE_LL, Some(wheel_hook), Some(module.into()), 0)
            {
                Ok(hook) => hook,
                Err(e) => {
                    warn!("could not install the mouse hook, scrolling is disabled: {e}");
                    return;
                }
            };
            debug!("mouse hook installed");

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            let _ = windows::Win32::UI::WindowsAndMessaging::UnhookWindowsHookEx(hook);
        })
        .expect("failed to spawn the scroll hook thread");
}

/// Runs for every mouse event system-wide, so it must stay cheap: Windows drops
/// hooks that overrun `LowLevelHooksTimeout`. All it does is a bounds check and
/// a channel send; the DDC write happens on the worker thread.
unsafe extern "system" fn wheel_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wparam.0 as u32 == WM_MOUSEWHEEL {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if over_tray_icon(info.pt.x, info.pt.y) {
            // The wheel delta is the signed high word of mouseData.
            let delta = ((info.mouseData >> 16) as u16) as i16 as i32;
            let total = REMAINDER.fetch_add(delta, Ordering::Relaxed) + delta;
            let notches = total / WHEEL_DELTA;
            if notches != 0 {
                REMAINDER.fetch_sub(notches * WHEEL_DELTA, Ordering::Relaxed);
                if let Some(tx) = COMMANDS.get() {
                    let _ = tx.send(Command::Adjust(notches));
                }
            }
            // Swallow the event so the taskbar does not also act on it.
            return LRESULT(1);
        }
    }

    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// True when the cursor is over our icon.
///
/// The hover flag alone would be enough in the common case, but a missed
/// `Leave` (the tray can drop one when the taskbar auto-hides) would leave the
/// hook eating wheel events everywhere, so the rect is checked as well.
fn over_tray_icon(x: i32, y: i32) -> bool {
    if !HOVERING.load(Ordering::Relaxed) {
        return false;
    }

    // try_lock, never lock: this runs inside the hook and must not block on the
    // UI thread updating the rect.
    match TRAY_RECT.try_lock() {
        Ok(guard) => match *guard {
            Some((left, top, right, bottom)) => {
                x >= left && x < right && y >= top && y < bottom
            }
            None => false,
        },
        Err(_) => false,
    }
}
