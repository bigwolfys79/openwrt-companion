use crate::settings::Settings;
use crate::snapshot::{build_snapshot, AppState, Snapshot};
use serde_json::json;
use tauri::State;

#[tauri::command]
pub async fn get_snapshot(state: State<'_, AppState>) -> Result<Snapshot, String> {
    match build_snapshot(&state).await {
        Ok(s) => Ok(s),
        Err(e) => Ok(Snapshot {
            status: "offline".into(),
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            error: Some(e),
            limited: false,
            devices: vec![],
            radios: vec![],
            stats: Default::default(),
            blocked_macs: vec![],
        }),
    }
}

/// The SSH command that installs the companion ACL (frontend hint copies it).
#[tauri::command]
pub fn acl_setup_command_cmd() -> String {
    crate::snapshot::acl_setup_command()
}

#[tauri::command]
pub async fn test_connection(state: State<'_, AppState>) -> Result<String, String> {
    state.reset_client();
    let client = state.client_or_new();
    client.login().await?;
    let board = client.call("system", "board", json!({})).await?;
    let model = board
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown router")
        .to_string();
    Ok(model)
}

#[tauri::command]
pub async fn save_settings(
    state: State<'_, AppState>,
    host: String,
    username: String,
    password: String,
    poll_interval_secs: u32,
    stats_rows: u32,
    profile_name: Option<String>,
    notify: Option<bool>,
    tariff_mbit: Option<u32>,
) -> Result<(), String> {
    // Единая write-блокировка на весь read-modify-write: параллельные команды
    // (set_alias / toggle_pin) не могут вклиниться между клоном и записью.
    let reconnect = {
        let mut s = state.settings.inner.write();
        let recon = (s.host != host || s.username != username)
            || (!password.is_empty() && s.password() != password);
        s.host = host.trim().to_string();
        s.username = username.trim().to_string();
        // empty password field = keep the stored one
        if !password.is_empty() {
            s.set_password(&password)?;
        }
        if let Some(n) = notify {
            s.notify = n;
        }
        s.poll_interval_secs = poll_interval_secs.clamp(2, 60);
        s.stats_rows = stats_rows.clamp(1, 3);
        // тариф для шкалы трея: 0 = авто-пороги; ограничим разумным диапазоном
        if let Some(t) = tariff_mbit {
            s.tariff_mbit = t.clamp(0, 100_000);
        }
        // persist the edited values into the active profile too (host/name/password),
        // so switching profiles later doesn't lose them
        if s.active_profile < s.profiles.len() {
            let idx = s.active_profile;
            let (cur_host, cur_user, cur_pass, cur_has) =
                (s.host.clone(), s.username.clone(), s.password_b64.clone(), s.has_password);
            let p = s.profiles.get_mut(idx).unwrap();
            if let Some(name) = profile_name {
                p.name = name.trim().to_string();
            }
            p.host = cur_host;
            p.username = cur_user;
            p.password_b64 = cur_pass;
            p.has_password = cur_has;
        }
        state.settings.save(&s).map_err(|e| e.to_string())?;
        recon
    }; // лок освобождён до await
    if reconnect {
        state.reset_client();
    }
    Ok(())
}

/// Drop the pinned SSH host key for the active router (e.g. after a reflash);
/// the next SSH connection re-learns it.
#[tauri::command]
pub fn ssh_forget_host_key(state: State<'_, AppState>) -> Result<(), String> {
    let host = state.settings.inner.read().host.clone();
    crate::ssh::forget_host_key(&host)
}

/// Persist UI-only preferences (language, theme) immediately — applied without
/// pressing "Save" in the settings view.
#[tauri::command]
pub fn set_ui_prefs(state: State<'_, AppState>, language: String, theme: String) -> Result<(), String> {
    // одна write-блокировка: никаких lost update между клоном и записью
    let mut s = state.settings.inner.write();
    s.language = match language.as_str() {
        "en" => "en".into(),
        _ => "ru".into(),
    };
    s.theme = match theme.as_str() {
        "light" => "light".into(),
        _ => "dark".into(),
    };
    state.settings.save(&s).map_err(|e| e.to_string())
}

/// Toggle the main window "always on top" flag; persists the preference and
/// applies it to the live window immediately.
#[tauri::command]
pub fn set_always_on_top(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    on: bool,
) -> Result<(), String> {
    use tauri::Manager;
    // одна write-блокировка: сохранить и применить — без гонок с save_settings
    {
        let mut s = state.settings.inner.write();
        s.always_on_top = on;
        state.settings.save(&s).map_err(|e| e.to_string())?;
    }
    if let Some(win) = app.get_webview_window("main") {
        win.set_always_on_top(on).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Toggle whether the app starts minimized to the tray (no main window on launch).
#[tauri::command]
pub fn set_start_minimized(state: State<'_, AppState>, on: bool) -> Result<(), String> {
    // одна write-блокировка: без гонок с save_settings / set_always_on_top
    let mut s = state.settings.inner.write();
    s.start_minimized = on;
    state.settings.save(&s).map_err(|e| e.to_string())
}

/// Open a URL in the system browser (About section: project page).
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    // only https links, so the command can't be abused to launch arbitrary apps
    if !url.starts_with("https://") {
        return Err("only https URLs are allowed".into());
    }
    // white-list символов URL: cmd.exe-метасимволы (`"`, `&`, `|`, `<`, `>`,
    // `^`, `%`) не могут попасть в строку команды — cmd-инъекция невозможна.
    if !url
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "?=&%#+@./:-_~[]!$'()*,:;".contains(c))
    {
        return Err("URL содержит недопустимые символы".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/c", "start", "", &url])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(&url)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    let mut s = state.settings.inner.read().clone();
    s.has_password = !s.password().is_empty();
    // DPAPI-блоб не нужен фронтенду (он использует только hasPassword) —
    // не отдаём зашифрованный пароль в webview
    s.password_b64 = String::new();
    s
}

#[tauri::command]
pub async fn set_alias(state: State<'_, AppState>, mac: String, name: String) -> Result<(), String> {
    let mac = mac.to_uppercase().replace('-', ":");
    let mut s = state.settings.inner.write();
    // при ошибке записи откатываем мутацию в памяти: диск и память не расходятся
    let old = s.aliases.get(&mac).cloned();
    if name.trim().is_empty() {
        s.aliases.remove(&mac);
    } else {
        s.aliases.insert(mac.clone(), name.trim().to_string());
    }
    if let Err(e) = state.settings.save(&s) {
        match old {
            Some(v) => s.aliases.insert(mac, v),
            None => s.aliases.remove(&mac),
        };
        return Err(e);
    }
    Ok(())
}

/// Deauthenticate a wifi client (kick).
#[tauri::command]
pub async fn kick_client(state: State<'_, AppState>, mac: String) -> Result<(), String> {
    let mac = mac.to_uppercase().replace('-', ":");
    let client = state.client_or_new();
    // find the interface the client is on
    let snap = build_snapshot(&state).await?;
    let dev = snap
        .devices
        .iter()
        .find(|d| d.mac == mac)
        .and_then(|d| d.iface.clone())
        .ok_or("device is not on Wi-Fi")?;
    client
        .call(
            &format!("hostapd.{dev}"),
            "del_client",
            json!({"addr": mac, "reason": 5, "deauth": true, "ban_time": 60000}),
        )
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn block_device(state: State<'_, AppState>, mac: String, block: bool) -> Result<(), String> {
    let mac = mac.to_uppercase().replace('-', ":");
    let client = state.client_or_new();
    if block {
        let rule_name = format!("oc-block-{}", mac.replace(':', "").to_lowercase());
        client
            .call(
                "uci",
                "add",
                json!({
                    "config": "firewall",
                    "type": "rule",
                    "values": {
                        "name": rule_name,
                        "src": "lan",
                        "src_mac": mac,
                        "dest": "*",
                        "target": "REJECT",
                        "family": "any"
                    }
                }),
            )
            .await?;
        client.call("uci", "commit", json!({"config": "firewall"})).await?;
    } else {
        let fw = client.call("uci", "get", json!({"config": "firewall"})).await?;
        if let Some(values) = fw.get("values").and_then(|v| v.as_object()) {
            for (sid, sec) in values {
                let stype = sec.get(".type").and_then(|t| t.as_str()).unwrap_or("");
                let name = sec.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let src_mac = sec.get("src_mac").and_then(|m| m.as_str()).unwrap_or("").to_uppercase().replace('-', ":");
                if stype == "rule" && name.starts_with("oc-block") && src_mac == mac {
                    client.call("uci", "delete", json!({"config": "firewall", "section": sid})).await?;
                }
            }
        }
        client.call("uci", "commit", json!({"config": "firewall"})).await?;
    }
    // reload firewall; tolerate failure (fw4 usually reloads on commit)
    let _ = client
        .call("file", "exec", json!({"command": ["/etc/init.d/firewall", "reload"]}))
        .await;
    Ok(())
}

/// Shared radio-reboot implementation (used by the command and the tray menu).
pub async fn reboot_radio_impl(client: &crate::ubus::UbusClient, device: &str) -> Result<(), String> {
    let direct = async {
        client
            .call("network.wireless", "down", json!({"device": device}))
            .await?;
        client
            .call("network.wireless", "up", json!({"device": device}))
            .await
    };

    if direct.await.is_ok() {
        return Ok(());
    }

    // fallback via uci: disable -> commit+apply -> wait -> enable -> commit+apply
    let section = format!("wireless.{device}");
    client
        .call("uci", "set", json!({"config": "wireless", "section": section, "values": {"disabled": "1"}}))
        .await?;
    client.call("uci", "commit", json!({"config": "wireless"})).await?;
    let _ = client.call("uci", "apply", json!({"rollback": true, "timeout": 30})).await;

    tokio::time::sleep(std::time::Duration::from_secs(4)).await;

    client
        .call("uci", "set", json!({"config": "wireless", "section": section, "values": {"disabled": "0"}}))
        .await?;
    client.call("uci", "commit", json!({"config": "wireless"})).await?;
    client
        .call("uci", "apply", json!({"rollback": false}))
        .await?;
    Ok(())
}

/// Reboot a single radio (e.g. radio0 / radio1) = restart that Wi-Fi band.
#[tauri::command]
pub async fn reboot_radio(state: State<'_, AppState>, device: String) -> Result<(), String> {
    let client = state.client_or_new();
    reboot_radio_impl(&client, &device).await
}

#[tauri::command]
pub async fn reboot_router(state: State<'_, AppState>) -> Result<(), String> {
    let client = state.client_or_new();
    client.call("system", "reboot", json!({})).await?;
    Ok(())
}

/// Toggle the main window (used by tray). Managed in tray.rs too.
#[tauri::command]
pub fn show_window(app: tauri::AppHandle) {
    crate::tray::show_main_window(&app, false);
}

/// Run the ACL setup command over SSH with the saved credentials
/// so the user doesn't have to paste it manually.
#[tauri::command]
pub async fn ssh_fix_limited(state: State<'_, AppState>) -> Result<String, String> {
    let s = state.settings.inner.read().clone();
    let host = s.host.clone();
    let user = s.username.clone();
    let pass = s.password();
    if pass.is_empty() {
        return Err("Пароль не задан — укажите его в настройках".into());
    }
    let cmd = crate::snapshot::acl_setup_command();
    let out = crate::ssh::run_command(&host, &user, &pass, &cmd).await?;
    // rpcd was restarted, the old ubus session is stale
    state.reset_client();
    Ok(out)
}

/// Manually override a device's icon type ("pc", "tv", ...; empty = auto).
#[tauri::command]
pub async fn set_kind(state: State<'_, AppState>, mac: String, kind: String) -> Result<(), String> {
    let mac = mac.to_uppercase().replace('-', ":");
    let mut s = state.settings.inner.write();
    let old = s.kinds.get(&mac).cloned();
    if kind.is_empty() || kind == "auto" {
        s.kinds.remove(&mac);
    } else {
        s.kinds.insert(mac.clone(), kind);
    }
    if let Err(e) = state.settings.save(&s) {
        match old {
            Some(v) => s.kinds.insert(mac, v),
            None => s.kinds.remove(&mac),
        };
        return Err(e);
    }
    Ok(())
}

/// Toggle pinning of a device (pinned devices are shown first).
#[tauri::command]
pub async fn toggle_pin(state: State<'_, AppState>, mac: String) -> Result<bool, String> {
    let mac = mac.to_uppercase().replace('-', ":");
    let mut s = state.settings.inner.write();
    if let Some(pos) = s.pinned.iter().position(|m| m == &mac) {
        s.pinned.remove(pos);
        if let Err(e) = state.settings.save(&*s) {
            s.pinned.insert(pos, mac); // откат при ошибке записи
            return Err(e);
        }
        Ok(false)
    } else {
        s.pinned.insert(0, mac.clone());
        if let Err(e) = state.settings.save(&*s) {
            s.pinned.retain(|m| m != &mac); // откат при ошибке записи
            return Err(e);
        }
        Ok(true)
    }
}

/// Scan Wi-Fi networks around the router for one interface (neighbor channels).
#[tauri::command]
pub async fn wifi_scan(state: State<'_, AppState>, device: String) -> Result<serde_json::Value, String> {
    let client = state.client_or_new();
    client
        .call("iwinfo", "scan", json!({"device": device}))
        .await
        .map_err(|e| format!("Скан недоступен: {e}"))
}

/// Run a diagnostic preset over SSH. kind: "ping_gw" | "ping_dns" | "log" | "mem".
#[tauri::command]
pub async fn run_diag(state: State<'_, AppState>, kind: String) -> Result<String, String> {
    let s = state.settings.inner.read().clone();
    let pass = s.password();
    if pass.is_empty() {
        return Err("Пароль не задан — укажите его в настройках".into());
    }
    let host = s.host.clone();
    let user = s.username.clone();
    let cmd = match kind.as_str() {
        "ping_gw" => "GW=$(ip route show default | awk '{print $3; exit}'); ping -c 4 -W 2 \"${GW:-192.168.1.1}\" 2>&1 | tail -n 8",
        "ping_dns" => "ping -c 4 -W 2 8.8.8.8 2>&1 | tail -n 8",
        // dropbear lines flood the log on every poll (the app's own SSH logins)
        "log" => "logread | grep -v dropbear | tail -n 60",
        "mem" => "free && echo && df -h / /tmp 2>/dev/null",
        _ => return Err("неизвестная диагностика".into()),
    };
    crate::ssh::run_command(&host, &user, &pass, cmd).await
}

/// Create a router config backup (sysupgrade tarball) and save it to
/// the user's Downloads folder. Returns the saved file path.
#[tauri::command]
pub async fn backup_config(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    use tauri::Manager;
    let s = state.settings.inner.read().clone();
    let pass = s.password();
    if pass.is_empty() {
        return Err("Пароль не задан — укажите его в настройках".into());
    }
    let out = crate::ssh::run_command_bytes(&s.host, &s.username, &pass, "sysupgrade -b - 2>/dev/null").await?;
    // gzip magic (1f 8b) or plain tar ("ustar" at offset 257)
    let is_gzip = out.starts_with(b"\x1f\x8b");
    let is_tar = out.get(257..262) == Some(b"ustar".as_slice());
    if out.len() < 512 || (!is_gzip && !is_tar) {
        let text = String::from_utf8_lossy(&out);
        return Err(format!("Роутер вернул не архив: {}", text.chars().take(120).collect::<String>()));
    }
    let dir = app.path().download_dir().map_err(|e| e.to_string())?;
    let ts = chrono_fmt();
    let path = dir.join(format!("openwrt-backup-{ts}.tar.gz"));
    std::fs::write(&path, &out).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// timestamp like 20261110-184505
fn chrono_fmt() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = now / 86400;
    let rem = now % 86400;
    let (h, mi, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil-from-days algorithm (UTC)
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}-{h:02}{mi:02}{sec:02}")
}

/// Switch to another saved router profile (saves the current one first).
#[tauri::command]
pub async fn switch_profile(state: State<'_, AppState>, index: usize) -> Result<(), String> {
    let mut s = state.settings.inner.write();
    if index >= s.profiles.len() {
        return Err("нет такого профиля".into());
    }
    // flush current top-level values into the active profile
    let (cur_host, cur_user, cur_pass, cur_has) = (s.host.clone(), s.username.clone(), s.password_b64.clone(), s.has_password);
    if s.active_profile < s.profiles.len() {
        let idx = s.active_profile;
        let a = s.profiles.get_mut(idx).unwrap();
        a.host = cur_host;
        a.username = cur_user;
        a.password_b64 = cur_pass;
        a.has_password = cur_has;
    }
    s.active_profile = index;
    let p = s.profiles[index].clone();
    s.host = p.host.clone();
    s.username = p.username.clone();
    s.password_b64 = p.password_b64.clone();
    s.has_password = p.has_password;
    state.settings.save(&*s).map_err(|e| e.to_string())?;
    drop(s);
    state.reset_client();
    Ok(())
}

/// Add a new empty router profile and switch to it.
#[tauri::command]
pub async fn add_profile(state: State<'_, AppState>, name: String) -> Result<(), String> {
    let mut s = state.settings.inner.write();
    // flush current top-level values into the active profile
    let (cur_host, cur_user, cur_pass, cur_has) = (s.host.clone(), s.username.clone(), s.password_b64.clone(), s.has_password);
    if s.active_profile < s.profiles.len() {
        let idx = s.active_profile;
        let a = s.profiles.get_mut(idx).unwrap();
        a.host = cur_host;
        a.username = cur_user;
        a.password_b64 = cur_pass;
        a.has_password = cur_has;
    }
    let count = s.profiles.len();
    s.profiles.push(crate::settings::RouterProfile {
        name: if name.trim().is_empty() { format!("Роутер {}", count + 1) } else { name.trim().to_string() },
        host: String::new(),
        username: "root".into(),
        password_b64: String::new(),
        has_password: false,
    });
    s.active_profile = s.profiles.len() - 1;
    s.host = String::new();
    s.username = "root".into();
    s.password_b64 = String::new();
    s.has_password = false;
    state.settings.save(&*s).map_err(|e| e.to_string())?;
    drop(s);
    state.reset_client();
    Ok(())
}

/// Delete a router profile by index (keeps at least one).
#[tauri::command]
pub async fn delete_profile(state: State<'_, AppState>, index: usize) -> Result<(), String> {
    let mut s = state.settings.inner.write();
    if s.profiles.len() <= 1 || index >= s.profiles.len() {
        return Err("нельзя удалить профиль".into());
    }
    s.profiles.remove(index);
    if s.active_profile >= s.profiles.len() {
        s.active_profile = s.profiles.len() - 1;
    } else if s.active_profile > index && s.active_profile > 0 {
        s.active_profile -= 1;
    }
    let p = s.profiles[s.active_profile].clone();
    s.host = p.host.clone();
    s.username = p.username.clone();
    s.password_b64 = p.password_b64.clone();
    s.has_password = p.has_password;
    state.settings.save(&*s).map_err(|e| e.to_string())?;
    drop(s);
    state.reset_client();
    Ok(())
}

/// Switch a radio to another channel (uci wireless.<device>.channel),
/// committing and applying the config.
#[tauri::command]
pub async fn set_channel(state: State<'_, AppState>, device: String, channel: i64) -> Result<(), String> {
    let client = state.client_or_new();
    client
        .call(
            "uci",
            "set",
            json!({"config": "wireless", "section": device, "values": {"channel": channel.to_string()}}),
        )
        .await?;
    client.call("uci", "commit", json!({"config": "wireless"})).await?;
    // rpcd applies changes on commit, so a follow-up apply usually returns
    // ubus error 5 (NO_DATA) — it is noise, not a failure. Restarting the
    // radio is what actually matters for the channel switch.
    let _ = client.call("uci", "apply", json!({"rollback": false})).await;
    Ok(())
}
