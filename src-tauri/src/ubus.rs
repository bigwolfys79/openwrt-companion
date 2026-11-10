use reqwest::Client;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
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
}

const ANON: &str = "00000000000000000000000000000000";

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
        *self.inner.token.write().unwrap() = token;
        Ok(())
    }

    /// Call an ubus object method; auto-relogins once on auth failure.
    pub async fn call(&self, object: &str, method: &str, params: Value) -> Result<Value, String> {
        let tok = self.inner.token.read().unwrap().clone();
        match self.rpc(json!([tok, object, method, params]), 2).await {
            Ok(v) => Ok(v),
            Err(e) if e.contains("ubus error 6") || e.to_lowercase().contains("authoriz") => {
                self.login().await?;
                let tok = self.inner.token.read().unwrap().clone();
                self.rpc(json!([tok, object, method, params]), 3).await
            }
            Err(e) => Err(e),
        }
    }
}
