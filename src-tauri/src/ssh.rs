//! Minimal SSH client helper (password auth) used to run a one-off
//! setup command on the router from the app itself.
//!
//! Host keys are pinned: the first connection to a host learns its
//! fingerprint (stored in `hostkeys.json` next to config.json), every
//! following connection verifies the key against the stored pin and
//! refuses to connect on mismatch (possible MITM or router reflash).

use russh::client::{self, Handle, Handler};
use russh::keys::{HashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Where hostkeys.json lives; initialized once at app startup.
static HOSTKEYS_PATH: OnceLock<PathBuf> = OnceLock::new();

pub fn init_hostkeys(dir: &std::path::Path) {
    let _ = HOSTKEYS_PATH.set(dir.join("hostkeys.json"));
}

fn hostkeys_path() -> PathBuf {
    HOSTKEYS_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("hostkeys.json"))
}

/// host -> SHA256 fingerprint (OpenSSH "SHA256:..." form)
fn load_hostkeys() -> HashMap<String, String> {
    std::fs::read_to_string(hostkeys_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_hostkeys(map: &HashMap<String, String>) -> Result<(), String> {
    let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    if let Some(parent) = hostkeys_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(hostkeys_path(), json).map_err(|e| e.to_string())
}

/// Forget the pinned key for `host` (e.g. after a router reflash).
pub fn forget_host_key(host: &str) -> Result<(), String> {
    let mut map = load_hostkeys();
    map.remove(host);
    save_hostkeys(&map)
}

fn fingerprint_of(key: &PublicKeyOrCertificate) -> Option<String> {
    match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => {
            Some(key.fingerprint(HashAlg::Sha256).to_string())
        }
        // certificates are not used by OpenWrt dropbear/sshd
        PublicKeyOrCertificate::Certificate { .. } => None,
    }
}

struct HostCheck {
    /// fingerprint pinned earlier, if any
    pinned: Option<String>,
    /// fingerprint observed on this connection (learned when no pin existed)
    learned: Arc<Mutex<Option<String>>>,
    /// set when a pinned key did NOT match (refused)
    mismatch: Arc<AtomicBool>,
}

impl Handler for HostCheck {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let Some(fp) = fingerprint_of(key) else {
            return Ok(false);
        };
        match &self.pinned {
            Some(stored) => {
                if *stored == fp {
                    Ok(true)
                } else {
                    self.mismatch.store(true, Ordering::SeqCst);
                    Ok(false)
                }
            }
            // first connection to this host: accept and remember the key
            None => {
                *self.learned.lock().unwrap() = Some(fp);
                Ok(true)
            }
        }
    }
}

fn key_mismatch_message(host: &str) -> String {
    format!(
        "SSH-ключ роутера {host} изменился — подключение заблокировано. \
Если роутер перепрошивали или сбрасывали, нажмите «Забыть ключ SSH» в настройках \
(или удалите запись из файла hostkeys.json рядом с config.json). \
Если нет — это может быть перехват подключения (MITM)."
    )
}

/// Connect to `host:22`, authenticate with a password and run `cmd`,
/// returning the combined stdout+stderr output.
pub async fn run_command(host: &str, user: &str, pass: &str, cmd: &str) -> Result<String, String> {
    let out = run_command_bytes(host, user, pass, cmd).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Same as run_command but returns raw bytes (for binary output, e.g. a
/// `sysupgrade -b -` backup tarball streamed to stdout).
pub async fn run_command_bytes(host: &str, user: &str, pass: &str, cmd: &str) -> Result<Vec<u8>, String> {
    let known = load_hostkeys();
    let pinned = known.get(host).cloned();
    let learned: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mismatch = Arc::new(AtomicBool::new(false));
    let checker = HostCheck {
        pinned: pinned.clone(),
        learned: Arc::clone(&learned),
        mismatch: Arc::clone(&mismatch),
    };

    let config = Arc::new(client::Config::default());
    let mut handle: Handle<HostCheck> = client::connect(config, (host.to_string(), 22), checker)
        .await
        .map_err(|e| {
            if mismatch.load(Ordering::SeqCst) {
                key_mismatch_message(host)
            } else {
                format!("Не удалось подключиться по SSH: {e}")
            }
        })?;

    // key was accepted: persist a newly learned fingerprint for future checks
    if pinned.is_none() {
        if let Some(fp) = learned.lock().unwrap().take() {
            let mut map = known;
            map.insert(host.to_string(), fp);
            let _ = save_hostkeys(&map);
        }
    }

    let auth = handle
        .authenticate_password(user, pass)
        .await
        .map_err(|e| format!("Ошибка аутентификации SSH: {e}"))?;
    if !auth.success() {
        let _ = handle
            .disconnect(Disconnect::ByApplication, "auth failed", "en")
            .await;
        return Err("SSH: неверный логин или пароль".into());
    }

    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("SSH: не удалось открыть канал: {e}"))?;
    channel
        .exec(true, cmd)
        .await
        .map_err(|e| format!("SSH: не удалось выполнить команду: {e}"))?;

    let mut out = Vec::new();
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, .. }) => out.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => {
                if exit_status != 0 {
                    out.extend_from_slice(
                        format!("\n[код выхода: {exit_status}]").as_bytes(),
                    );
                }
                break;
            }
            Some(_) => {}
            None => break,
        }
    }

    let _ = handle
        .disconnect(Disconnect::ByApplication, "done", "en")
        .await;

    Ok(out)
}
