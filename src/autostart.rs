use log::warn;
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Wry};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

/// The plugin, ready for the Tauri builder.
pub fn plugin() -> TauriPlugin<Wry> {
    // The launcher choice is read only on macOS and ignored here. The app takes
    // no arguments, so nothing is appended to the registered command line.
    tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, None)
}

/// Whether Windows will start the app at logon.
///
/// This is false when the Run entry is missing *or* when the user has switched
/// the app off in Task Manager, which records its veto separately.
pub fn is_enabled(app: &AppHandle<Wry>) -> bool {
    match app.autolaunch().is_enabled() {
        Ok(enabled) => enabled,
        Err(e) => {
            // Treat an unreadable registry as "off" so the menu only ever
            // claims what it could confirm.
            warn!("could not read the autostart setting: {e}");
            false
        }
    }
}

/// Turns autostart on or off, returning the state that actually took effect so
/// a failed registry write leaves the caller showing the truth.
pub fn set(app: &AppHandle<Wry>, enable: bool) -> bool {
    let manager = app.autolaunch();

    let outcome = if enable {
        manager.enable()
    } else {
        manager.disable()
    };

    match outcome {
        Ok(()) => enable,
        Err(e) => {
            warn!(
                "could not turn autostart {}: {e}",
                if enable { "on" } else { "off" }
            );
            is_enabled(app)
        }
    }
}
