use crate::snapshot::{build_snapshot, Snapshot};
use crate::tray_anim;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::{AppHandle, Emitter, Manager};

/// Background poller: builds a snapshot every N seconds, emits it to the UI,
/// drives the tray animation (traffic level + event flashes) and the tooltip.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut prev: Option<Snapshot> = None;
        // подряд идущие неудачи: оффлайн объявляется только после двух, чтобы
        // одиночный сетевой выброс не флапал иконкой и оффлайн-уведомлением
        let mut err_streak: u32 = 0;
        loop {
            let interval = {
                let state = app.state::<crate::snapshot::AppState>();
                let secs = state.settings.inner.read().poll_interval_secs;
                secs
            };
            let snap = build_snapshot(&app.state::<crate::snapshot::AppState>()).await;
            match &snap {
                Ok(s) => {
                    err_streak = 0;
                    let _ = app.emit("snapshot", s);
                    let _ = update_tray(&app, s, prev.as_ref());
                    prev = Some(s.clone());
                }
                Err(e) => {
                    err_streak += 1;
                    if err_streak < 2 {
                        // одиночный сбой: оставляем в UI последний хороший снапшот
                        prev = None;
                        tokio::time::sleep(Duration::from_secs(interval.max(2) as u64)).await;
                        continue;
                    }
                    let offline = crate::snapshot::Snapshot {
                        status: "offline".into(),
                        ts: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0),
                        error: Some(e.clone()),
                        limited: false,
                        devices: vec![],
                        radios: vec![],
                        stats: Default::default(),
                        blocked_macs: vec![],
                    };
                    let _ = app.emit("snapshot", &offline);
                    tray_anim::set_offline(&app, true);
                    if let Some(tray) = app.tray_by_id("main") {
                        let lang = crate::i18n::lang(&app);
                        let _ = tray.set_tooltip(Some(format!(
                            "OpenWrt Companion\n{}\n{}",
                            crate::i18n::tr(&lang, "Роутер недоступен", "Router unreachable"),
                            truncate(&e, 80)
                        )));
                    }
                    prev = None;
                }
            }
            tokio::time::sleep(Duration::from_secs(interval.max(2) as u64)).await;
        }
    });
}

/// WAN speed diff between two snapshots (busiest WAN-side counter, same
/// logic as the frontend tile). Returns (down, up) in bits/s.
fn wan_speed(prev: Option<&Snapshot>, cur: &Snapshot) -> (f64, f64) {
    let p = match prev {
        Some(p) if p.ts > 0 && cur.ts > p.ts => p,
        _ => return (0.0, 0.0),
    };
    let dt = ((cur.ts - p.ts) / 1000).max(1) as f64;
    let rate = |c: Option<u64>, o: Option<u64>| -> f64 {
        match (c, o) {
            (Some(c), Some(o)) if c >= o => ((c - o) as f64 * 8.0) / dt,
            _ => 0.0,
        }
    };
    // busiest counter, mirroring the frontend logic: prefer split-capable
    // ports (min << max) over aggregated CPU ports where rx ~ tx ~ total
    let mut best = (
        rate(cur.stats.rx_bytes, p.stats.rx_bytes),
        rate(cur.stats.tx_bytes, p.stats.tx_bytes),
    );
    let mut best_score = best.0 + best.1;
    let mut best_split = best.0 < 0.75 * best.1 || best.1 < 0.75 * best.0;
    // индекс по имени интерфейса вместо O(n²) find в цикле
    let prev_by_iface: HashMap<&str, &crate::snapshot::WanCounter> =
        p.stats.wan_counters.iter().map(|x| (x.iface.as_str(), x)).collect();
    for c in &cur.stats.wan_counters {
        if let Some(pc) = prev_by_iface.get(c.iface.as_str()) {
            let r = rate(Some(c.rx_bytes), Some(pc.rx_bytes));
            let t = rate(Some(c.tx_bytes), Some(pc.tx_bytes));
            let split = r < 0.75 * t || t < 0.75 * r;
            let score = r + t;
            if score > best_score && (split || !best_split) {
                best_score = score;
                best_split = split;
                best = (r, t);
            }
        }
    }
    best
}

fn fmt_bits(bps: f64, lang: &str) -> String {
    if bps < 1_000_000.0 {
        format!("{:.0} {}", bps / 1000.0, crate::i18n::tr(lang, "кбит/с", "kbit/s"))
    } else if bps < 1_000_000_000.0 {
        format!("{:.1} {}", bps / 1_000_000.0, crate::i18n::tr(lang, "Мбит/с", "Mbit/s"))
    } else {
        format!("{:.2} {}", bps / 1_000_000_000.0, crate::i18n::tr(lang, "Гбит/с", "Gbit/s"))
    }
}

fn fmt_short(bps: f64) -> String {
    if bps < 1_000_000.0 {
        format!("{:.0}к", bps / 1000.0)
    } else if bps < 1_000_000_000.0 {
        format!("{:.1}М", bps / 1_000_000.0)
    } else {
        format!("{:.1}Г", bps / 1_000_000_000.0)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

fn update_tray(app: &AppHandle, s: &Snapshot, prev: Option<&Snapshot>) -> tauri::Result<()> {
    let lang = crate::i18n::lang(app);
    tray_anim::set_offline(app, false);

    // traffic level 0..3 -> bar animation
    let (down, _up) = wan_speed(prev, s);
    let level = if down < 500_000.0 {
        0
    } else if down < 5_000_000.0 {
        1
    } else if down < 50_000_000.0 {
        2
    } else {
        3
    };
    tray_anim::set_level(app, level);

    // flash on device connect/disconnect transitions
    let online: HashSet<&String> = s.devices.iter().filter(|d| d.online).map(|d| &d.mac).collect();
    if let Some(p) = prev {
        let was: HashSet<&String> = p.devices.iter().filter(|d| d.online).map(|d| &d.mac).collect();
        if online.iter().any(|m| !was.contains(*m)) {
            tray_anim::flash(app, 0); // green: somebody connected
        }
        if was.iter().any(|m| !online.contains(*m)) {
            tray_anim::flash(app, 1); // red: somebody dropped
        }
    }

    // dynamic menu: active devices sorted by download speed
    let mut speeds: Vec<(String, String, f64, f64)> = Vec::new(); // mac, name, down_bps, up_bps
    if let Some(p) = prev {
        let dt = ((s.ts - p.ts) / 1000).max(1) as f64;
        let rate = |c: Option<u64>, o: Option<u64>| -> f64 {
            match (c, o) {
                (Some(c), Some(o)) if c >= o => (c - o) as f64 / dt,
                _ => 0.0,
            }
        };
        // индекс предыдущего снапшота по MAC вместо find в цикле
        let prev_by_mac: HashMap<&str, &crate::snapshot::Device> =
            p.devices.iter().map(|x| (x.mac.as_str(), x)).collect();
        for d in &s.devices {
            if !d.online {
                continue;
            }
            if let Some(pd) = prev_by_mac.get(d.mac.as_str()) {
                // AP tx -> download, AP rx -> upload (same as the frontend)
                speeds.push((
                    d.mac.clone(),
                    truncate(&(d.alias.clone().unwrap_or_else(|| d.name.clone())), 22),
                    rate(d.tx_bytes, pd.tx_bytes) * 8.0,
                    rate(d.rx_bytes, pd.rx_bytes) * 8.0,
                ));
            }
        }
    }
    speeds.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let mut menu = MenuBuilder::new(app);
    if speeds.is_empty() {
        let it = MenuItemBuilder::with_id("noop", crate::i18n::tr(lang, "Нет активных устройств", "No active devices")).enabled(false).build(app)?;
        menu = menu.item(&it);
    }
    for (mac, name, down, up) in speeds.iter().take(10) {
        let label = format!("{name} — ↓ {} ↑ {}", fmt_bits(*down, lang), fmt_bits(*up, lang));
        let it = MenuItemBuilder::with_id(format!("dev:{mac}"), label).build(app)?;
        menu = menu.item(&it);
    }
    let sep0 = tauri::menu::PredefinedMenuItem::separator(app)?;
    let show = MenuItemBuilder::with_id("show", crate::i18n::tr(lang, "Показать панель", "Show panel")).build(app)?;
    let wifi24 = MenuItemBuilder::with_id("wifi24", crate::i18n::tr(lang, "Перезагрузить Wi-Fi 2.4 ГГц", "Reboot Wi-Fi 2.4 GHz")).build(app)?;
    let wifi5 = MenuItemBuilder::with_id("wifi5", crate::i18n::tr(lang, "Перезагрузить Wi-Fi 5 ГГц", "Reboot Wi-Fi 5 GHz")).build(app)?;
    let reboot = MenuItemBuilder::with_id("reboot", crate::i18n::tr(lang, "Перезагрузить роутер", "Reboot router")).build(app)?;
    let quit = MenuItemBuilder::with_id("quit", crate::i18n::tr(lang, "Выход", "Quit")).build(app)?;
    let sep1 = tauri::menu::PredefinedMenuItem::separator(app)?;
    let sep2 = tauri::menu::PredefinedMenuItem::separator(app)?;
    let sep3 = tauri::menu::PredefinedMenuItem::separator(app)?;
    menu = menu.item(&sep0).item(&show).item(&sep1)
        .item(&wifi24).item(&wifi5).item(&sep2)
        .item(&reboot).item(&sep3).item(&quit);
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_menu(Some(menu.build()?));
    }

    // tooltip: just the device list (status is shown by the icon itself -
    // yellow when offline). Empty tooltip would flash "OpenWrt Companion",
    // so offline keeps a short hint line.
    let tip = if online.is_empty() {
        crate::i18n::tr(lang, "Роутер офлайн", "Router offline").to_string()
    } else {
        speeds
            .iter()
            .take(8)
            .map(|(_, name, d, u)| {
                let n = if name.len() > 12 {
                    format!("{}…", name.chars().take(11).collect::<String>())
                } else {
                    name.clone()
                };
                format!("{n} ↓{} ↑{}", fmt_short(*d), fmt_short(*u))
            })
            .collect::<Vec<String>>()
            .join("
")
    };
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(truncate(&tip, 120)));
    }
    Ok(())
}
