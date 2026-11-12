//! Minimal SSH client helper (password auth) used to run one-off commands
//! on the router from the app itself, plus a persistent pooled session
//! reused by the poller (nftables accounting) instead of reconnecting
//! every few seconds.
//!
//! Host keys are pinned: the first connection to a host learns its
//! fingerprint (stored in `hostkeys.json` next to config.json), every
//! following connection verifies the key against the stored pin and
//! refuses to connect on mismatch (possible MITM or router reflash).
//! When a key is first learned, an `ssh-hostkey-learned` event is emitted
//! so the UI can show the fingerprint to the user (TOFU transparency).

use russh::client::{self, Handle, Handler};
use russh::keys::{HashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Where hostkeys.json lives; initialized once at app startup.
static HOSTKEYS_PATH: OnceLock<PathBuf> = OnceLock::new();

/// App handle for emitting the TOFU event; set once at app startup.
static APP_HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();

/// In-memory host key cache guarded by a mutex. Prevents the classic
/// lost-update race: two parallel SSH connections each reloading the file
/// and overwriting each other's freshly learned fingerprints on save.
static HOSTKEYS: OnceLock<Mutex<Option<HashMap<String, String>>>> = OnceLock::new();

pub fn init_hostkeys(dir: &std::path::Path, app: tauri::AppHandle) {
    let _ = HOSTKEYS_PATH.set(dir.join("hostkeys.json"));
    let _ = APP_HANDLE.set(app);
}

fn hostkeys_path() -> PathBuf {
    HOSTKEYS_PATH
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("hostkeys.json"))
}

fn hostkeys_cache() -> &'static Mutex<Option<HashMap<String, String>>> {
    HOSTKEYS.get_or_init(|| Mutex::new(None))
}

/// host -> SHA256 fingerprint (OpenSSH "SHA256:..." form); loaded from disk once
fn load_hostkeys() -> HashMap<String, String> {
    let mut cache = hostkeys_cache().lock().unwrap();
    if cache.is_none() {
        *cache = Some(
            std::fs::read_to_string(hostkeys_path())
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default(),
        );
    }
    cache.as_ref().unwrap().clone()
}

fn save_hostkeys(map: &HashMap<String, String>) -> Result<(), String> {
    let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    if let Some(parent) = hostkeys_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(hostkeys_path(), json).map_err(|e| e.to_string())?;
    // only update the cache after the file write succeeded
    *hostkeys_cache().lock().unwrap() = Some(map.clone());
    Ok(())
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

/// Connect + password-auth, verifying/learning the host key. Shared by the
/// one-off runner and the persistent pool.
async fn connect_verified(host: &str, user: &str, pass: &str) -> Result<Handle<HostCheck>, String> {
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
    // таймаут на подключение: «залипший» роутер не должен вешать команду навсегда
    let mut handle: Handle<HostCheck> = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client::connect(config, (host.to_string(), 22), checker),
    )
    .await
    .map_err(|_| format!("SSH: таймаут подключения к {host}"))?
    .map_err(|e| {
        if mismatch.load(Ordering::SeqCst) {
            key_mismatch_message(host)
        } else {
            format!("Не удалось подключиться по SSH: {e}")
        }
    })?;

    // key was accepted: persist a newly learned fingerprint for future checks
    // and tell the UI (TOFU transparency: the user sees what was pinned)
    if pinned.is_none() {
        if let Some(fp) = learned.lock().unwrap().take() {
            let mut map = known;
            map.insert(host.to_string(), fp.clone());
            if save_hostkeys(&map).is_ok() {
                if let Some(app) = APP_HANDLE.get() {
                    use tauri::Emitter;
                    let _ = app.emit(
                        "ssh-hostkey-learned",
                        serde_json::json!({ "host": host, "fingerprint": fp }),
                    );
                }
            }
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
    Ok(handle)
}

/// Run `cmd` on an already-authenticated handle; does NOT disconnect, so the
/// caller decides whether the session stays pooled or is torn down.
async fn exec_on(handle: &mut Handle<HostCheck>, cmd: &str) -> Result<Vec<u8>, String> {
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("SSH: не удалось открыть канал: {e}"))?;
    channel
        .exec(true, cmd)
        .await
        .map_err(|e| format!("SSH: не удалось выполнить команду: {e}"))?;

    let mut out = Vec::new();
    // per-chunk timeout: a stalled router must not hang the caller forever
    // (previously this loop could hang indefinitely on `run_diag` / backups)
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(60), channel.wait()).await {
            Err(_) => {
                let _ = handle
                    .disconnect(Disconnect::ByApplication, "timeout", "en")
                    .await;
                return Err("SSH: превышен таймаут выполнения команды (60 с)".into());
            }
            Ok(Some(ChannelMsg::Data { data })) => out.extend_from_slice(&data),
            Ok(Some(ChannelMsg::ExtendedData { data, .. })) => out.extend_from_slice(&data),
            Ok(Some(ChannelMsg::ExitStatus { exit_status })) => {
                if exit_status != 0 {
                    out.extend_from_slice(
                        format!("\n[код выхода: {exit_status}]").as_bytes(),
                    );
                }
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
        }
    }
    Ok(out)
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
    let mut handle = connect_verified(host, user, pass).await?;
    let out = exec_on(&mut handle, cmd).await;
    let _ = handle
        .disconnect(Disconnect::ByApplication, "done", "en")
        .await;
    out
}

/* ---------- persistent pooled session (poller / accounting) ---------- */

/// One long-lived authenticated SSH connection. `lan_accounting` used to
/// open a fresh connection (TCP + kex + auth) every poll interval; the pool
/// keeps a single session and re-runs the script over new channels.
pub struct SshPool {
    session: parking_lot::Mutex<Option<Pooled>>,
}

struct Pooled {
    host: String,
    user: String,
    pass: String,
    handle: Handle<HostCheck>,
}

impl Default for SshPool {
    fn default() -> Self {
        Self { session: parking_lot::Mutex::new(None) }
    }
}

impl SshPool {
    /// Run `cmd` over the pooled session, (re)connecting when there is none
    /// or when the saved credentials/host changed. On any failure the pooled
    /// session is dropped so the next call reconnects from scratch.
    pub async fn run(&self, host: &str, user: &str, pass: &str, cmd: &str) -> Result<Vec<u8>, String> {
        // fast path: take the handle out of the pool (guard must NOT be held
        // across awaits — parking_lot guards are !Send and this future runs
        // on the tauri async runtime)
        let mut taken: Option<Handle<HostCheck>> = {
            let mut guard = self.session.lock();
            let matched = guard.as_ref().is_some_and(|p| p.host == host && p.user == user && p.pass == pass);
            if matched {
                guard.take().map(|p| p.handle)
            } else {
                // сменились host/логин/пароль или сессии нет — пул очищаем
                *guard = None;
                None
            }
        };

        if let Some(h) = taken.as_mut() {
            match exec_on(h, cmd).await {
                Ok(out) => {
                    self.put_back(host, user, pass, taken.take());
                    return Ok(out);
                }
                Err(_) => {
                    // сессия умерла (роутер перезагрузился и т.п.) — reconnect ниже
                    taken.take();
                }
            }
        }

        // slow path: connect fresh, exec, keep the session for the next poll
        let mut handle = connect_verified(host, user, pass).await?;
        let res = exec_on(&mut handle, cmd).await;
        match res {
            Ok(out) => {
                self.put_back(host, user, pass, Some(handle));
                Ok(out)
            }
            Err(e) => {
                let _ = handle
                    .disconnect(Disconnect::ByApplication, "done", "en")
                    .await;
                Err(e)
            }
        }
    }

    fn put_back(&self, host: &str, user: &str, pass: &str, handle: Option<Handle<HostCheck>>) {
        if let Some(handle) = handle {
            *self.session.lock() = Some(Pooled {
                host: host.to_string(),
                user: user.to_string(),
                pass: pass.to_string(),
                handle,
            });
        }
    }

    /// Drop the pooled session (called when settings change / rpcd restarts).
    pub fn reset(&self) {
        *self.session.lock() = None;
    }
}
