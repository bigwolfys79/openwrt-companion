use reqwest::Client;
use serde_json::{json, Value};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;

/// ubus JSON-RPC client over the LuCI /ubus endpoint.
#[derive(Clone)]
pub struct UbusClient {
    inner: Arc<Inner>,
}

struct Inner {
    url: String,
    http: Client,
    username: String,
    password: String,
    token: RwLock<String>,
    /// single-flight gate for relogin: when a session expires, N parallel
    /// calls all see the auth error, but only ONE performs login();
    /// the rest wait on this mutex and retry with the fresh token.
    relogin: tokio::sync::Mutex<()>,
}

const ANON: &str = "00000000000000000000000000000000";

/// ubus error 6 = INVALID_TOKEN / unauthorized session; also accept the
/// "authoriz" wording some builds return. Typed check instead of a fragile
/// substring match anywhere in the message.
fn is_auth_err(e: &str) -> bool {
    e == "ubus error 6" || e.to_lowercase().contains("authoriz")
}

impl UbusClient {
    pub fn new(host: &str, username: &str, password: &str) -> Self {
        let host = host.trim().trim_end_matches('/').to_string();
        let url = if host.contains("://") { format!("{}/ubus", host) } else { format!("http://{}/ubus", host) };
        Self {
            inner: Arc::new(Inner {
                url,
                http: Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap_or_default(),
                username: username.to_string(),
                password: password.to_string(),
                token: RwLock::new(ANON.to_string()),
                relogin: tokio::sync::Mutex::new(()),
            }),
        }
    }

    async fn rpc(&self, params: Value, id: u32) -> Result<Value, String> {
        let body = json!({"jsonrpc": "2.0", "id": id, "method": "call", "params": params});
        let resp = self
            .inner
            .http
            .post(&self.inner.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let v: Value = resp.json().await.map_err(|e| format!("JSON: {e}"))?;
        if let Some(err) = v.get("error") {
            return Err(format!("rpc: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("error")));
        }
        let result = v
            .get("result")
            .and_then(|r| r.as_array())
            .ok_or("empty ubus result")?;
        let code = result.first().and_then(|c| c.as_i64()).unwrap_or(-1);
        if code != 0 {
            return Err(format!("ubus error {}", code));
        }
        Ok(result.get(1).cloned().unwrap_or(Value::Null))
    }

    pub async fn login(&self) -> Result<(), String> {
        let res = self
            .rpc(
                json!([ANON, "session", "login", {
                    "username": self.inner.username,
                    "password": self.inner.password,
                }]),
                1,
            )
            .await?;
        let token = res
            .get("ubus_rpc_session")
            .and_then(|t| t.as_str())
            .ok_or("login failed: no session token (check credentials)")?
            .to_string();
        *self.inner.token.write() = token;
        Ok(())
    }

    /// Call an ubus object method; auto-relogins once on auth failure.
    /// Relogin is single-flight: parallel callers that hit the same expired
    /// session wait on a mutex and reuse the token obtained by the first one.
    pub async fn call(&self, object: &str, method: &str, params: Value) -> Result<Value, String> {
        let old_tok = self.inner.token.read().clone();
        match self.rpc(json!([old_tok, object, method, params]), 2).await {
            Ok(v) => Ok(v),
            Err(e) if is_auth_err(&e) => {
                // only one caller re-logins; the others reuse the fresh token
                let _guard = self.inner.relogin.lock().await;
                let cur = self.inner.token.read().clone();
                let tok = if cur != old_tok {
                    cur // someone else already renewed it
                } else {
                    self.login().await?;
                    self.inner.token.read().clone()
                };
                self.rpc(json!([tok, object, method, params]), 3).await
            }
            Err(e) => Err(e),
        }
    }
}
