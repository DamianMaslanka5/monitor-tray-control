# Monitor Tray Control

A Windows tray app that changes your monitor's brightness over DDC/CI.
**Scroll the mouse wheel over the tray icon** to raise and lower brightness.

> This project was written by Claude Code.

## Why this exists

It replaces [ClickMonitorDDC](https://github.com/nubix/ClickmonitorDDC) for me.
ClickMonitorDDC does the same job, but scrolling over its tray icon leaves a
noticeable lag before the monitor's brightness actually moves, so a scroll feels
disconnected from the result.

## Usage

| Action | Result |
| --- | --- |
| Scroll up / down over the tray icon | Brightness +/- 5% |
| Hover the tray icon | Tooltip shows the monitor and current brightness |
| Right-click | Menu: presets (0/25/50/75/100%), Launch on startup, Reconnect, Quit |

The icon itself is a ring gauge that fills clockwise with the current level, and
it picks a light or dark glyph to suit your taskbar theme.

## Building

```sh
cargo build --release
```

The binary lands in `target/release/monitor-tray-control.exe` and is
self-contained. To produce an installer instead:

```sh
cargo install tauri-cli --version "^2"
cargo tauri build
```

Run with `RUST_LOG=monitor_tray_control=debug` to see what it is talking to.
Release builds are detached from the console, so redirect to a file if you want
the log.

## How scrolling on the tray icon works

Windows sends `WM_MOUSEWHEEL` to the *focused* window, and the notification area
belongs to Explorer, so a tray icon never receives wheel events through its own
callback — `tray-icon` accordingly has no scroll event. The workaround is a
global low-level mouse hook:

- `src/scroll.rs` installs a `WH_MOUSE_LL` hook on a dedicated thread with its
  own message pump. Low-level hooks run on the installing thread and Windows
  silently removes them if that thread stops pumping messages or overruns
  `LowLevelHooksTimeout`, so it stays clear of Tauri's event loop and does no
  real work: it just bounds-checks the cursor and posts to a channel.
- The icon's screen rectangle comes from the tray's own `Enter`/`Move`/`Leave`
  events, which carry the rect Explorer reports via `Shell_NotifyIconGetRect`.
  That keeps it correct across DPI changes and taskbar moves.
- Wheel events over the icon are consumed (`LRESULT(1)`) so the taskbar does not
  also act on them. Everything else is passed straight through.
- Deltas accumulate in a remainder so high-resolution wheels still produce
  whole notches.

## Design notes

`ddc_hi::Display` wraps a raw Windows monitor handle and is not `Send`, so the
display lives on one worker thread (`src/monitor.rs`) and is driven over a
channel. DDC round-trips take tens of milliseconds and monitors drop requests
when hammered, so a burst of scroll input is coalesced into a single write, and
the tray updates optimistically before the write lands.

Only the `ddc-winapi` backend is enabled; the `nvapi` and `i2c` backends that
`ddc-hi` turns on by default are dropped, as they pull in vendor DLLs this app
does not need.

## Limitations

- Single monitor: the first display that answers a brightness query is used.
- The monitor must have DDC/CI enabled — many ship with it off, under a name
  like "DDC/CI", "MCCS", or "auto source" in the OSD.
- Laptop internal panels generally do not speak DDC/CI.
- One instance per user session: a second launch sees the named mutex
  `monitor-tray-control-single-instance` and exits silently. Two copies would
  double every scroll notch, since each installs its own global wheel hook.
