use crate::snapshot::AppState;
use serde_json::json;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub fn show_main_window(app: &AppHandle, near_cursor: bool) {
    if let Some(win) = app.get_webview_window("main") {
        if near_cursor {
            if let Ok(pos) = win.current_monitor() {
                let cursor = win.cursor_position().ok();
                let scale = pos.as_ref().map(|m| m.scale_factor()).unwrap_or(1.0);
                let size = win.outer_size().unwrap_or_default();
                if let Some(c) = cursor {
                    let mut x = (c.x as i32 - size.width as i32 - 8) as f64;
                    let mut y = (c.y as i32 - size.height as i32 - 8) as f64;
                    if let Some(m) = pos.as_ref() {
                        let mpos = m.position();
                        let msize = m.size();
                        let max_x = (mpos.x as i32 + msize.width as i32) as f64;
                        let max_y = (mpos.y as i32 + msize.height as i32) as f64;
                        x = x.clamp(mpos.x as f64 + 8.0, max_x - size.width as f64 - 8.0);
                        y = y.clamp(mpos.y as f64 + 8.0, max_y - size.height as f64 - 8.0);
                        let _ = scale; // cursor_position is already physical pixels; window position physical too
                    }
                    let _ = win.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
                }
            }
        }
        let _ = win.show();
        let _ = win.set_focus();
    }
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let lang = crate::i18n::lang(app);
    let show = MenuItemBuilder::with_id("show", crate::i18n::tr(lang, "Показать панель", "Show panel")).build(app)?;
    let wifi24 = MenuItemBuilder::with_id("wifi24", crate::i18n::tr(lang, "Перезагрузить Wi-Fi 2.4 ГГц", "Reboot Wi-Fi 2.4 GHz")).build(app)?;
    let wifi5 = MenuItemBuilder::with_id("wifi5", crate::i18n::tr(lang, "Перезагрузить Wi-Fi 5 ГГц", "Reboot Wi-Fi 5 GHz")).build(app)?;
    let reboot = MenuItemBuilder::with_id("reboot", crate::i18n::tr(lang, "Перезагрузить роутер", "Reboot router")).build(app)?;
    let quit = MenuItemBuilder::with_id("quit", crate::i18n::tr(lang, "Выход", "Quit")).build(app)?;
    let sep1 = tauri::menu::PredefinedMenuItem::separator(app)?;
    let sep2 = tauri::menu::PredefinedMenuItem::separator(app)?;
    let sep3 = tauri::menu::PredefinedMenuItem::separator(app)?;

    let menu = MenuBuilder::new(app)
        .item(&show)
        .item(&sep1)
        .item(&wifi24)
        .item(&wifi5)
        .item(&sep2)
        .item(&reboot)
        .item(&sep3)
        .item(&quit)
        .build()?;

    let mut tray = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("OpenWrt Companion")
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            match id {
                "show" => show_main_window(app, false),
                "noop" => {}
                "quit" => app.exit(0),
                "wifi24" | "wifi5" => {
                    let want = if id == "wifi24" { "2g" } else { "5g" };
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        reboot_band(&app, want).await;
                    });
                }
                "reboot" => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let state = app.state::<AppState>();
                        let client = state.client_or_new();
                        let _ = client.call("system", "reboot", json!({})).await;
                    });
                }
                // device rows: "dev:<mac>" -> open the panel
                _ => {
                    if id.starts_with("dev:") {
                        show_main_window(app, true);
                    }
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            // double left click toggles the panel: hidden -> shown, shown -> hidden
            if let TrayIconEvent::DoubleClick {
                button: tauri::tray::MouseButton::Left,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(win) = app.get_webview_window("main") {
                    if win.is_visible().unwrap_or(false) {
                        let _ = win.hide();
                    } else {
                        show_main_window(app, true);
                    }
                }
            }
        });

    if let Some(icon) = app.default_window_icon().cloned() {
        tray = tray.icon(icon);
    }
    tray.build(app)?;
    Ok(())
}

/// Reboot the first radio matching the requested band ("2g"/"5g").
async fn reboot_band(app: &AppHandle, band: &str) {
    let state = app.state::<AppState>();
    let client = state.client_or_new();
    // find a radio with the matching band from luci-rpc getWirelessDevices
    if let Ok(wl) = client.call("luci-rpc", "getWirelessDevices", json!({})).await {
        if let Some(map) = wl.as_object() {
            let target = map
                .iter()
                .find(|(_, r)| {
                    str_of(r, &["config", "band"]).as_deref().map(|b| b.eq_ignore_ascii_case(band)).unwrap_or(false)
                })
                .or_else(|| map.iter().next())
                .map(|(k, _)| k.clone());
            if let Some(radio) = target {
                // reuse the same fallback logic as the reboot_radio command
                let _ = crate::commands::reboot_radio_impl(&client, &radio).await;
            }
        }
    }
}

fn str_of(v: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().map(|s| s.to_string())
}
