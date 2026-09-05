//! DDC/CI brightness control, driven from a dedicated worker thread.
//!
//! `ddc_hi::Display` wraps a raw Windows monitor handle and is not `Send`, so
//! the display never leaves the worker thread. Callers talk to it over a
//! channel of [`Command`]s and observe it through the [`Update`] callback.
//!
//! DDC round-trips take tens of milliseconds and monitors drop requests when
//! hammered, so bursts of scroll input are coalesced into a single write.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use ddc_hi::{Ddc, Display};
use log::{debug, warn};

/// MCCS feature code for luminance.
const VCP_BRIGHTNESS: u8 = 0x10;

/// Brightness change per scroll notch, in percent.
pub const STEP_PERCENT: i32 = 5;

/// Attempts made when a DDC read fails before giving up on the display.
const READ_ATTEMPTS: usize = 3;
/// Pause between retries, giving the monitor time to settle.
const RETRY_DELAY: Duration = Duration::from_millis(120);

/// A request for the worker thread.
#[derive(Debug, Clone, Copy)]
pub enum Command {
    /// Move brightness by this many scroll notches.
    Adjust(i32),
    /// Jump to an absolute percentage (0-100).
    SetPercent(u16),
    /// Drop the current handle and enumerate displays again.
    Reconnect,
}

/// A change the worker wants reflected in the UI.
#[derive(Debug, Clone)]
pub enum Update {
    /// A display is available; `percent` is its current brightness.
    Connected { name: String, percent: u16 },
    /// Brightness moved to `percent` on the already-connected display.
    Brightness { percent: u16 },
    /// No usable display; the string explains why.
    Disconnected { reason: String },
}

/// The worker's view of the attached display.
struct Connection {
    display: Display,
    name: String,
    /// Raw VCP value, which is not necessarily 0-100.
    value: u16,
    /// Raw VCP maximum reported by the monitor.
    max: u16,
}

impl Connection {
    fn percent(&self) -> u16 {
        if self.max == 0 {
            return 0;
        }
        ((self.value as u32 * 100 + self.max as u32 / 2) / self.max as u32).min(100) as u16
    }

    /// Converts a percentage back into the monitor's own scale.
    fn value_for(&self, percent: u16) -> u16 {
        let percent = percent.min(100) as u32;
        ((percent * self.max as u32 + 50) / 100) as u16
    }
}

/// Starts the worker thread and returns the channel used to drive it.
///
/// `on_update` is called from the worker thread for every observable change,
/// including the initial connection attempt.
pub fn spawn<F>(on_update: F) -> Sender<Command>
where
    F: FnMut(Update) + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("ddc-worker".into())
        .spawn(move || run(rx, on_update))
        .expect("failed to spawn DDC worker thread");
    tx
}

fn run<F>(rx: Receiver<Command>, mut on_update: F)
where
    F: FnMut(Update) + Send + 'static,
{
    let mut conn = match connect() {
        Ok(conn) => {
            on_update(Update::Connected {
                name: conn.name.clone(),
                percent: conn.percent(),
            });
            Some(conn)
        }
        Err(reason) => {
            warn!("no display at startup: {reason}");
            on_update(Update::Disconnected { reason });
            None
        }
    };

    while let Ok(first) = rx.recv() {
        // Fold everything queued behind this command into one write. Scrolling
        // produces commands far faster than the monitor can accept them.
        let mut notches = 0i32;
        let mut absolute = None;
        let mut reconnect = false;

        for cmd in std::iter::once(first).chain(rx.try_iter()) {
            match cmd {
                Command::Adjust(n) => notches += n,
                Command::SetPercent(p) => {
                    absolute = Some(p);
                    notches = 0;
                }
                Command::Reconnect => reconnect = true,
            }
        }

        if reconnect {
            conn = None;
        }

        if conn.is_none() {
            match connect() {
                Ok(fresh) => {
                    on_update(Update::Connected {
                        name: fresh.name.clone(),
                        percent: fresh.percent(),
                    });
                    conn = Some(fresh);
                }
                Err(reason) => {
                    on_update(Update::Disconnected { reason });
                    continue;
                }
            }
        }

        let Some(active) = conn.as_mut() else {
            continue;
        };

        let target = match absolute {
            Some(percent) => percent.min(100),
            None if notches != 0 => {
                let moved = active.percent() as i32 + notches * STEP_PERCENT;
                moved.clamp(0, 100) as u16
            }
            // Nothing but a reconnect; the Connected update already went out.
            None => continue,
        };

        if target == active.percent() && absolute.is_none() {
            continue;
        }

        let raw = active.value_for(target);
        // Report the new value before the write so the tray reacts instantly;
        // the monitor catches up a few tens of milliseconds later.
        active.value = raw;
        on_update(Update::Brightness {
            percent: active.percent(),
        });

        if let Err(e) = active.display.handle.set_vcp_feature(VCP_BRIGHTNESS, raw) {
            warn!("failed to set brightness: {e}");
            let reason = format!("Lost contact with the display: {e}");
            conn = None;
            on_update(Update::Disconnected { reason });
        }
    }

    debug!("DDC worker shutting down");
}

/// Finds the first display that answers a brightness query.
fn connect() -> Result<Connection, String> {
    let displays = Display::enumerate();
    if displays.is_empty() {
        return Err("No monitor found".into());
    }

    let mut last_error = None;
    for mut display in displays {
        let name = display
            .info
            .model_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| display.info.id.clone());

        match read_brightness(&mut display) {
            Ok((value, max)) => {
                debug!("using display {name} (brightness {value}/{max})");
                return Ok(Connection {
                    display,
                    name,
                    value,
                    max,
                });
            }
            Err(e) => {
                warn!("{name} did not answer a brightness query: {e}");
                last_error = Some(e);
            }
        }
    }

    Err(match last_error {
        Some(e) => format!("No monitor supports DDC/CI brightness: {e}"),
        None => "No monitor supports DDC/CI brightness".into(),
    })
}

/// Reads the current and maximum brightness, retrying flaky DDC responses.
fn read_brightness(display: &mut Display) -> Result<(u16, u16), String> {
    let mut last = String::new();
    for attempt in 0..READ_ATTEMPTS {
        if attempt > 0 {
            thread::sleep(RETRY_DELAY);
        }
        match display.handle.get_vcp_feature(VCP_BRIGHTNESS) {
            Ok(value) if value.maximum() > 0 => return Ok((value.value(), value.maximum())),
            // A zero maximum means the monitor answered but the feature is
            // unusable, which no amount of retrying will fix.
            Ok(_) => return Err("monitor reports a maximum brightness of 0".into()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}
