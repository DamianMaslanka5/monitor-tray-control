//! One process per user session.
//!
//! Two copies would fight over the same monitor and, worse, each would install
//! its own global wheel hook, so a single notch would move brightness twice.
//! A named mutex is the cheapest way to notice: the kernel keeps the name alive
//! for as long as any handle to it is open, and releases it when the last
//! holder dies, so even a crash never leaves a stale lock behind.
//!
//! The name is unprefixed, so it lands in the caller's session namespace: each
//! logged-in user has their own taskbar, and so may have their own instance.

use log::{info, warn};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// Holds the claim for as long as it is alive; dropping it, or exiting, frees
/// the name for the next launch.
pub struct Guard(Option<HANDLE>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // Closing the last handle destroys the mutex; ownership is
            // irrelevant, so there is no ReleaseMutex to pair with this.
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

/// Claims the single-instance slot, or returns `None` when another copy holds it.
pub fn acquire() -> Option<Guard> {
    // The handle comes back valid whether or not we are first; `GetLastError`
    // is what tells the two apart, so nothing may run in between.
    let handle = match unsafe { CreateMutexW(None, true, w!("monitor-tray-control-single-instance")) }
    {
        Ok(handle) => handle,
        Err(e) => {
            // The namespace is unhappy rather than occupied. Starting anyway is
            // friendlier than refusing to run at all.
            warn!("could not create the single-instance mutex, skipping the check: {e}");
            return Some(Guard(None));
        }
    };
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    // Owned either way: the handle has to be closed even when we lose, or the
    // name would outlive this process.
    let guard = Guard(Some(handle));

    if already_running {
        info!("another instance is already running; exiting");
        return None;
    }

    Some(guard)
}
