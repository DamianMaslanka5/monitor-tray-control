//! Runtime rendering of the tray icon.
//!
//! The icon is a ring gauge: a faint full-circle track with a brighter arc
//! sweeping clockwise from the top in proportion to the current brightness,
//! plus a solid dot in the middle. It is drawn at 4x and box-filtered down so
//! the curves stay smooth at tray sizes.

use tauri::image::Image;

/// Edge length of the emitted icon, in pixels.
const SIZE: u32 = 32;
/// Supersampling factor used while rasterizing.
const SS: u32 = 4;

const RADIUS: f32 = 12.0;
const STROKE: f32 = 3.5;
const DOT_RADIUS: f32 = 4.0;

/// Alpha applied to the unfilled part of the ring.
const TRACK_ALPHA: f32 = 0.28;

/// Which foreground colour keeps the icon legible against the taskbar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tone {
    /// Light glyph, for the default dark taskbar.
    Light,
    /// Dark glyph, for a light taskbar.
    Dark,
}

impl Tone {
    fn rgb(self) -> [u8; 3] {
        match self {
            Tone::Light => [0xF2, 0xF5, 0xF9],
            Tone::Dark => [0x1C, 0x1F, 0x24],
        }
    }
}

/// Renders the gauge for `percent` (0-100) as an owned RGBA image.
pub fn render(percent: u16, tone: Tone) -> Image<'static> {
    let coverage = rasterize(percent);
    let [r, g, b] = tone.rgb();

    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for a in coverage {
        rgba.extend_from_slice(&[r, g, b, a]);
    }

    Image::new_owned(rgba, SIZE, SIZE)
}

/// Renders the "no monitor" icon: the track ring only, at half strength.
pub fn render_disconnected(tone: Tone) -> Image<'static> {
    let img = render(0, tone);
    let mut rgba = img.rgba().to_vec();
    for px in rgba.chunks_exact_mut(4) {
        px[3] = px[3] / 2;
    }
    Image::new_owned(rgba, SIZE, SIZE)
}

/// Produces one alpha byte per output pixel by averaging the supersampled grid.
fn rasterize(percent: u16) -> Vec<u8> {
    let fraction = (percent.min(100) as f32) / 100.0;
    let hi = SIZE * SS;
    let scale = hi as f32 / SIZE as f32;

    let center = hi as f32 / 2.0;
    let r_outer = (RADIUS + STROKE / 2.0) * scale;
    let r_inner = (RADIUS - STROKE / 2.0) * scale;
    let r_dot = DOT_RADIUS * scale;
    let sweep = fraction * std::f32::consts::TAU;

    // Alpha at full resolution, then box-filtered down to SIZE x SIZE.
    let mut fine = vec![0.0f32; (hi * hi) as usize];
    for y in 0..hi {
        for x in 0..hi {
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let dist = (dx * dx + dy * dy).sqrt();

            let alpha = if dist <= r_dot {
                1.0
            } else if dist >= r_inner && dist <= r_outer {
                // Angle measured clockwise from straight up, in 0..TAU.
                let angle = dx.atan2(-dy).rem_euclid(std::f32::consts::TAU);
                if angle <= sweep { 1.0 } else { TRACK_ALPHA }
            } else {
                0.0
            };

            fine[(y * hi + x) as usize] = alpha;
        }
    }

    let samples = (SS * SS) as f32;
    let mut out = Vec::with_capacity((SIZE * SIZE) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut sum = 0.0;
            for sy in 0..SS {
                for sx in 0..SS {
                    sum += fine[((y * SS + sy) * hi + (x * SS + sx)) as usize];
                }
            }
            out.push((sum / samples * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    out
}

/// Reads the taskbar theme from the registry to pick a legible glyph colour.
///
/// Defaults to a light glyph, which matches Windows' own default dark taskbar.
#[cfg(windows)]
pub fn system_tone() -> Tone {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let uses_light = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_value::<u32, _>("SystemUsesLightTheme"))
        .map(|v| v == 1)
        .unwrap_or(false);

    if uses_light { Tone::Dark } else { Tone::Light }
}
