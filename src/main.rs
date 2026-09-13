//! A tray-only control for monitor brightness over DDC/CI.
//!
//! Scroll the wheel over the tray icon to change brightness; the menu offers
//! presets and a reconnect. The app deliberately creates no window, so nothing
//! ever instantiates a webview.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_os = "windows"))]
compile_error!("monitor-tray-control targets Windows only");

mod autostart;
mod icon;
mod monitor;
mod scroll;
mod single_instance;

use std::sync::Mutex;
use std::sync::mpsc::Sender;

use log::warn;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent, Wry};

use monitor::{Command, Update};

/// Identifies our tray icon for `tray_by_id` lookups.
const TRAY_ID: &str = "brightness";

/// Menu-item id prefix for the brightness presets.
const PRESET_PREFIX: &str = "preset:";

/// Everything the update handler needs to redraw the tray.
struct Ui {
    /// The non-clickable menu row that shows the current reading.
    status: MenuItem<Wry>,
    /// The "Launch on startup" tick, kept so a refused registry write can put
    /// it back the way it was.
    autostart: CheckMenuItem<Wry>,
    /// Glyph colour chosen for the current taskbar theme.
    tone: icon::Tone,
    /// Display name, kept so brightness updates can keep it in the tooltip.
    name: Mutex<String>,
}

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("monitor_tray_control=info"),
    )
    .init();

    let Some(_instance) = single_instance::acquire() else {
        return;
    };

    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let tone = icon::system_tone();

            // Registered here rather than on the builder so the plugin's own
            // setup has run, and the manager it stores is live, before the menu
            // below asks it whether autostart is currently on.
            app.handle().plugin(autostart::plugin())?;

            let status = MenuItem::with_id(app, "status", "Connecting...", false, None::<&str>)?;
            let autostart_item = CheckMenuItem::with_id(
                app,
                "autostart",
                "Launch on startup",
                true,
                autostart::is_enabled(&handle),
                None::<&str>,
            )?;
            let menu = build_menu(app, &status, &autostart_item)?;

            app.manage(Ui {
                status,
                autostart: autostart_item,
                tone,
                name: Mutex::new(String::new()),
            });

            // The worker owns the display handle; updates come back here and
            // are applied on the main thread, which is where Windows expects
            // tray and menu mutations to happen.
            let update_handle = handle.clone();
            let commands = monitor::spawn(move |update| {
                let handle = update_handle.clone();
                if let Err(e) = update_handle
                    .run_on_main_thread(move || apply_update(&handle, update))
                {
                    warn!("could not deliver a tray update: {e}");
                }
            });

            app.manage(Commands(commands.clone()));

            TrayIconBuilder::with_id(TRAY_ID)
                .icon(icon::render_disconnected(tone))
                .tooltip("Monitor Tray Control - connecting...")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(handle_menu_event)
                .on_tray_icon_event(|_tray, event| scroll::track_tray_event(&event))
                .build(app)?;

            scroll::install(commands);

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start monitor-tray-control")
        .run(|_app, event| {
            // With no windows there is nothing to close, but Tauri still asks
            // to exit when the last one goes away; keep the tray alive so only
            // the Quit item ends the process.
            if let RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}

/// The channel to the DDC worker, stored so menu handlers can reach it.
struct Commands(Sender<Command>);

fn build_menu<M: Manager<Wry>>(
    app: &M,
    status: &MenuItem<Wry>,
    autostart: &CheckMenuItem<Wry>,
) -> tauri::Result<Menu<Wry>> {
    let presets: Vec<MenuItem<Wry>> = [0u16, 25, 50, 75, 100]
        .into_iter()
        .map(|percent| {
            MenuItem::with_id(
                app,
                format!("{PRESET_PREFIX}{percent}"),
                format!("{percent}%"),
                true,
                None::<&str>,
            )
        })
        .collect::<tauri::Result<_>>()?;

    // A menu item belongs to one position, so the two rules are separate items.
    let top_rule = PredefinedMenuItem::separator(app)?;
    let bottom_rule = PredefinedMenuItem::separator(app)?;
    let reconnect = MenuItem::with_id(app, "reconnect", "Reconnect", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let mut items: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = vec![status, &top_rule];
    items.extend(presets.iter().map(|p| p as &dyn tauri::menu::IsMenuItem<Wry>));
    items.push(&bottom_rule);
    items.push(autostart);
    items.push(&reconnect);
    items.push(&quit);

    Menu::with_items(app, &items)
}

fn handle_menu_event(app: &AppHandle<Wry>, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();

    if id == "quit" {
        app.exit(0);
        return;
    }

    if id == "autostart" {
        toggle_autostart(app);
        return;
    }

    let Some(commands) = app.try_state::<Commands>() else {
        return;
    };

    let command = if id == "reconnect" {
        Command::Reconnect
    } else if let Some(percent) = id.strip_prefix(PRESET_PREFIX) {
        match percent.parse::<u16>() {
            Ok(percent) => Command::SetPercent(percent),
            Err(_) => return,
        }
    } else {
        return;
    };

    if commands.0.send(command).is_err() {
        warn!("the DDC worker is gone; ignoring {id}");
    }
}

/// Flips the Run entry and re-syncs the tick, which the menu has already
/// toggled on its own by the time the click reaches us.
fn toggle_autostart(app: &AppHandle<Wry>) {
    let Some(ui) = app.try_state::<Ui>() else {
        return;
    };

    let enabled = autostart::set(app, !autostart::is_enabled(app));
    if let Err(e) = ui.autostart.set_checked(enabled) {
        warn!("could not update the autostart tick: {e}");
    }
}

/// Applies a worker update to the tray icon, tooltip and menu. Main thread only.
fn apply_update(app: &AppHandle<Wry>, update: Update) {
    let Some(ui) = app.try_state::<Ui>() else {
        return;
    };
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    let (image, tooltip, status) = match update {
        Update::Connected { name, percent } => {
            if let Ok(mut current) = ui.name.lock() {
                *current = name.clone();
            }
            (
                icon::render(percent, ui.tone),
                format!("{name}\nBrightness: {percent}%"),
                format!("Brightness: {percent}%"),
            )
        }
        Update::Brightness { percent } => {
            let name = ui
                .name
                .lock()
                .map(|n| n.clone())
                .unwrap_or_default();
            (
                icon::render(percent, ui.tone),
                format!("{name}\nBrightness: {percent}%"),
                format!("Brightness: {percent}%"),
            )
        }
        Update::Disconnected { reason } => (
            icon::render_disconnected(ui.tone),
            format!("Monitor Tray Control\n{reason}"),
            reason,
        ),
    };

    if let Err(e) = tray.set_icon(Some(image)) {
        warn!("could not update the tray icon: {e}");
    }
    if let Err(e) = tray.set_tooltip(Some(&tooltip)) {
        warn!("could not update the tooltip: {e}");
    }
    if let Err(e) = ui.status.set_text(&status) {
        warn!("could not update the status item: {e}");
    }
}
