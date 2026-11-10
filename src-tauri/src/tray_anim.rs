//! Animated tray icon: 4 signal-style bars whose lit count follows the WAN
//! traffic level, with a soft pulse; plus short color flashes on device
//! connect/disconnect events and a static yellow state when offline.

use std::sync::Mutex;
use std::time::Duration;
use tauri::image::Image;
use tauri::{AppHandle, Manager};

const SIZE: u32 = 32;
const TICK_MS: u64 = 350;

/// bar geometry: 4 bars, ascending heights
const BAR_X: [u32; 4] = [3, 10, 17, 24];
const BAR_W: u32 = 6;
const BAR_BOTTOM: u32 = 30;
const BAR_H: [u32; 4] = [8, 14, 20, 26];

#[derive(Default)]
pub struct AnimState {
    /// lit-bar level 0..=3 derived from WAN traffic
    pub level: u8,
    /// offline / router unreachable -> steady yellow
    pub offline: bool,
    /// (color kind: 0 green, 1 red, ticks left) overlay flash
    pub flash: Option<(u8, u8)>,
}

fn kind_rgb(kind: u8) -> [u8; 3] {
    match kind {
        0 => [92, 214, 148],   // green: device connected
        1 => [244, 106, 106],  // red: device disconnected
        _ => [245, 198, 109],  // yellow: offline
    }
}

/// Draw one frame: dark rounded backdrop + 4 bars, first `level+1` lit.
/// `phase` drives the pulse alpha of lit bars; flash overrides colors.
fn draw_frame(level: u8, phase: usize, flash: Option<u8>, offline: bool) -> Image<'static> {
    let mut px = vec![0u8; (SIZE * SIZE * 4) as usize];
    let put = |px: &mut [u8], x: u32, y: u32, c: [u8; 3], a: u8| {
        if x >= SIZE || y >= SIZE {
            return;
        }
        let i = ((y * SIZE + x) * 4) as usize;
        px[i] = c[0];
        px[i + 1] = c[1];
        px[i + 2] = c[2];
        px[i + 3] = a;
    };

    // rounded dark backdrop
    let bg = [22u8, 26, 36];
    let stroke = [70u8, 80, 100];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = (x as i32 - 16).abs().min(4);
            let dy = (y as i32 - 16).abs().min(4);
            let inside = (dx as f32).hypot(dy as f32) < 4.2;
            let ring = ((x as f32 - 15.5).hypot(y as f32 - 15.5)).abs();
            if ring < 15.5 {
                put(&mut px, x, y, bg, if inside { 230 } else { 200 });
            }
            if ring > 14.2 && ring < 15.8 {
                put(&mut px, x, y, stroke, 190);
            }
        }
    }

    let (lit, color, a_base): (usize, [u8; 3], i32) = if let Some(k) = flash {
        (4, kind_rgb(k), 255)
    } else if offline {
        (4, kind_rgb(2), 235)
    } else {
        let swing = if level >= 3 { 80 } else { 50 };
        let a = 200 - (swing as f32 * (phase as f32 / 8.0 * std::f32::consts::TAU).sin()) as i32;
        (
            level as usize + 1,
            [79, 216, 200], // teal accent
            a,
        )
    };

    for (i, &bx) in BAR_X.iter().enumerate() {
        let h = BAR_H[i];
        for y in (BAR_BOTTOM - h)..=BAR_BOTTOM {
            // shave 1px corners on the top/bottom rows
            let short = y == BAR_BOTTOM - h || y == BAR_BOTTOM;
            let x_start = if short { bx + 1 } else { bx };
            let x_end = if short { bx + BAR_W - 2 } else { bx + BAR_W - 1 };
            for x in x_start..=x_end {
                if i < lit {
                    put(&mut px, x, y, color, a_base.clamp(60, 255) as u8);
                } else {
                    put(&mut px, x, y, [110, 118, 134], 70);
                }
            }
        }
    }

    Image::new_owned(px, SIZE, SIZE)
}

/// Animation loop: swaps tray icon frames every TICK_MS.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut phase: usize = 0;
        loop {
            let (level, offline, flash) = {
                let st = app.state::<Mutex<AnimState>>();
                let mut a = st.lock().unwrap();
                let flash = a.flash.as_ref().map(|(k, _)| *k);
                if let Some((_, t)) = a.flash.as_mut() {
                    *t -= 1;
                    if *t == 0 {
                        a.flash = None;
                    }
                }
                (a.level, a.offline, flash)
            };
            phase = (phase + 1) % 8;
            let img = draw_frame(level, phase, flash, offline);
            if let Some(tray) = app.tray_by_id("main") {
                let _ = tray.set_icon(Some(img));
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    });
}

pub fn set_level(app: &AppHandle, level: u8) {
    app.state::<Mutex<AnimState>>().lock().unwrap().level = level.min(3);
}

pub fn set_offline(app: &AppHandle, offline: bool) {
    app.state::<Mutex<AnimState>>().lock().unwrap().offline = offline;
}

pub fn flash(app: &AppHandle, kind: u8) {
    let st = app.state::<Mutex<AnimState>>();
    let mut st = st.lock().unwrap();
    st.flash = Some((kind, 7)); // ~2.5 s of blinking frames
}
