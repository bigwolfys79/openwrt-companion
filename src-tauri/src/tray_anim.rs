//! Animated tray icon: 4 signal-style bars whose lit count follows the WAN
//! traffic level, with a soft pulse; plus short color flashes on device
//! connect/disconnect events and a static yellow state when offline.

use parking_lot::Mutex;
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

/// Draw one frame: 4 bars on a transparent background, first `level+1` lit.
/// `phase` drives the pulse alpha of lit bars; flash overrides colors.
/// (Раньше рисовалась тёмная круглая подложка с обводкой — на тёмной панели
/// она выглядела посторонним «кружком»; теперь фон полностью прозрачный.)
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
/// When nothing is animating (level 0, online, no flash) the frame is static:
/// it is drawn once and set_icon is skipped until the state changes, so the
/// loop does not repaint the tray 3 times a second around the clock.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut phase: usize = 0;
        // подпись последнего отрисованного статичного кадра
        let mut static_drawn: Option<(u8, bool)> = None;
        loop {
            let (level, offline, flash) = {
                let st = app.state::<Mutex<AnimState>>();
                let mut a = st.lock();
                let flash = a.flash.as_ref().map(|(k, _)| *k);
                if let Some((_, t)) = a.flash.as_mut() {
                    *t -= 1;
                    if *t == 0 {
                        a.flash = None;
                    }
                }
                (a.level, a.offline, flash)
            };
            let static_idle = level == 0 && !offline && flash.is_none();
            if static_idle && static_drawn == Some((level, offline)) {
                // кадр не менялся — не перерисовываем
                tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                continue;
            }
            if static_idle {
                static_drawn = Some((level, offline));
            } else {
                static_drawn = None;
            }
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
    app.state::<Mutex<AnimState>>().lock().level = level.min(3);
}

pub fn set_offline(app: &AppHandle, offline: bool) {
    app.state::<Mutex<AnimState>>().lock().offline = offline;
}

pub fn flash(app: &AppHandle, kind: u8) {
    let st = app.state::<Mutex<AnimState>>();
    let mut st = st.lock();
    st.flash = Some((kind, 7)); // ~2.5 s of blinking frames
}
