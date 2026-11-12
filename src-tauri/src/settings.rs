use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use parking_lot::RwLock;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub host: String,
    pub username: String,
    /// DPAPI-encrypted password ("dpapi:<base64 blob>"); empty = not set.
    /// Legacy plain base64 values are migrated on load.
    pub password_b64: String,
    /// true when a password is stored (frontend hint: empty field = keep it)
    #[serde(default)]
    pub has_password: bool,
    pub poll_interval_secs: u32,
    pub aliases: HashMap<String, String>,
    /// manual device type overrides: MAC -> kind (pc, phone, tv...)
    #[serde(default)]
    pub kinds: HashMap<String, String>,
    /// stats tiles layout: rows of tiles (1..3)
    #[serde(default = "default_stats_rows")]
    pub stats_rows: u32,
    /// desktop notifications on device connect/disconnect / router offline
    #[serde(default = "default_true")]
    pub notify: bool,
    /// pinned device MACs (shown first, in this order)
    #[serde(default)]
    pub pinned: Vec<String>,
    /// saved router profiles (host/username/password); the active one mirrors
    /// the top-level host/username/password_b64 fields
    #[serde(default)]
    pub profiles: Vec<RouterProfile>,
    /// index of the active profile in `profiles`
    #[serde(default)]
    pub active_profile: usize,
    /// UI language: "ru" | "en"
    #[serde(default = "default_language")]
    pub language: String,
    /// UI theme: "dark" | "light"
    #[serde(default = "default_theme")]
    pub theme: String,
    /// main window always on top (tray panel convenience; user-toggleable)
    #[serde(default = "default_true")]
    pub always_on_top: bool,
    /// don't show the main window on app launch (tray-only start)
    #[serde(default)]
    pub start_minimized: bool,
}

fn default_language() -> String {
    "ru".into()
}

fn default_theme() -> String {
    "dark".into()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RouterProfile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password_b64: String,
    #[serde(default)]
    pub has_password: bool,
}

fn default_stats_rows() -> u32 {
    2
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: "192.168.1.1".into(),
            username: "root".into(),
            password_b64: String::new(),
            has_password: false,
            poll_interval_secs: 5,
            aliases: HashMap::new(),
            kinds: HashMap::new(),
            stats_rows: default_stats_rows(),
            notify: true,
            pinned: Vec::new(),
            profiles: Vec::new(),
            active_profile: 0,
            language: default_language(),
            theme: default_theme(),
            always_on_top: true,
            start_minimized: false,
        }
    }
}

fn b64_decode_bytes(input: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in input.bytes() {
        let v = match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    out
}

fn b64_decode(input: &str) -> String {
    String::from_utf8_lossy(&b64_decode_bytes(input)).into_owned()
}

pub fn b64_encode(plain: &str) -> String {
    b64_encode_bytes(plain.as_bytes())
}

pub fn b64_encode_bytes(bytes: &[u8]) -> String {
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TBL[(n >> 18) as usize & 63] as char);
        out.push(TBL[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TBL[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TBL[n as usize & 63] as char } else { '=' });
    }
    out
}

impl Settings {
    pub fn password(&self) -> String {
        if let Some(enc) = self.password_b64.strip_prefix("dpapi:") {
            return dpapi::unprotect(enc).unwrap_or_default();
        }
        b64_decode(&self.password_b64) // legacy plain base64
    }

    pub fn set_password(&mut self, plain: &str) -> Result<(), String> {
        self.password_b64 = if plain.is_empty() {
            String::new()
        } else {
            dpapi::protect(plain)?
        };
        self.has_password = !self.password_b64.is_empty();
        Ok(())
    }
}

/// DPAPI (Windows Data Protection) encryption bound to the current user
/// account: the blob can only be decrypted by the same user profile.
#[cfg(windows)]
mod dpapi {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
    };
    use windows::core::PCWSTR;

    pub fn protect(plain: &str) -> Result<String, String> {
        let data = plain.as_bytes();
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(
                &in_blob,
                PCWSTR::null(),
                None,
                None,
                None,
                0,
                &mut out,
            )
            .map_err(|e| format!("DPAPI encrypt: {e}"))?;
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
            let b64 = crate::settings::b64_encode_bytes(bytes);
            let _ = LocalFree(HLOCAL(out.pbData as *mut core::ffi::c_void));
            Ok(format!("dpapi:{b64}"))
        }
    }

    pub fn unprotect(enc_b64: &str) -> Result<String, String> {
        let data = crate::settings::b64_decode_bytes(enc_b64);
        if data.is_empty() {
            return Ok(String::new());
        }
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&in_blob, None, None, None, None, 0, &mut out)
                .map_err(|e| format!("DPAPI decrypt: {e}"))?;
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
            let plain = String::from_utf8_lossy(bytes).into_owned();
            let _ = LocalFree(HLOCAL(out.pbData as *mut core::ffi::c_void));
            Ok(plain)
        }
    }
}

#[cfg(not(windows))]
mod dpapi {
    // non-Windows fallback: plain base64 (no OS-level protection available here)
    pub fn protect(plain: &str) -> Result<String, String> {
        Ok(format!("plain:{}", crate::settings::b64_encode(plain)))
    }
    pub fn unprotect(enc: &str) -> Result<String, String> {
        enc.strip_prefix("plain:")
            .map(|s| crate::settings::b64_decode(s))
            .ok_or_else(|| "unsupported password encoding".into())
    }
}

pub struct SettingsStore {
    pub path: PathBuf,
    pub inner: RwLock<Settings>,
}

impl SettingsStore {
    pub fn load(app: &AppHandle) -> Self {
        let dir = app.path().app_data_dir().unwrap_or_else(|_| PathBuf::from("."));
        if let Err(e) = fs::create_dir_all(&dir) {
            eprintln!("settings: cannot create data dir {:?}: {e}", dir);
        }
        let path = dir.join("config.json");
        let mut settings = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .unwrap_or_default();
        // migrate legacy plain-base64 passwords to DPAPI-encrypted storage
        let legacy = !settings.password_b64.is_empty()
            && !settings.password_b64.starts_with("dpapi:")
            && !settings.password_b64.starts_with("plain:");
        if legacy {
            let legacy_value = settings.password_b64.clone();
            let plain = b64_decode(&settings.password_b64);
            if settings.set_password(&plain).is_ok() {
                let persisted = serde_json::to_string_pretty(&settings)
                    .ok()
                    .map(|json| fs::write(&path, json).is_ok())
                    .unwrap_or(false);
                if !persisted {
                    // откат: не заявляем has_password, пока блоб не сохранён на диск
                    eprintln!("settings: failed to persist migrated password — keeping legacy value");
                    settings.password_b64 = legacy_value;
                    settings.has_password = !settings.password_b64.is_empty();
                }
            }
        } else {
            settings.has_password = !settings.password_b64.is_empty();
        }
        // migrate: no profiles yet but a router is configured -> make it profile #0
        if settings.profiles.is_empty() && !settings.host.is_empty() {
            settings.profiles.push(RouterProfile {
                name: String::new(),
                host: settings.host.clone(),
                username: settings.username.clone(),
                password_b64: settings.password_b64.clone(),
                has_password: settings.has_password,
            });
            settings.active_profile = 0;
        }
        Self { path, inner: RwLock::new(settings) }
    }

    pub fn save(&self, s: &Settings) -> Result<(), String> {
        let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
        fs::write(&self.path, json).map_err(|e| {
            eprintln!("settings: failed to write {:?}: {e}", self.path);
            e.to_string()
        })
    }
}
