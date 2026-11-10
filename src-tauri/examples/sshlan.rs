use russh::client::{self, Handle, Handler};
use russh::{ChannelMsg, Disconnect};
use std::sync::Arc;
struct AcceptAll;
impl Handler for AcceptAll {
    type Error = russh::Error;
    async fn check_server_key(&mut self, _k: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> { Ok(true) }
}
#[tokio::main]
async fn main() {
    let host = std::env::args().nth(1).unwrap_or("192.168.1.1".into());
    let user = std::env::args().nth(2).unwrap_or("root".into());
    let pass = std::env::args().nth(3).unwrap_or_default();
    let script = std::env::args().nth(4).unwrap_or_default();
    let config = Arc::new(client::Config::default());
    let mut h: Handle<AcceptAll> = client::connect(config, (host, 22u16), AcceptAll).await.unwrap();
    h.authenticate_password(user, pass).await.unwrap();
    let mut ch = h.channel_open_session().await.unwrap();
    ch.exec(true, script).await.unwrap();
    let mut out = Vec::new();
    loop {
        match ch.wait().await {
            Some(ChannelMsg::Data { data }) => out.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, .. }) => out.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { .. }) => break,
            Some(_) => {}
            None => break,
        }
    }
    let _ = h.disconnect(Disconnect::ByApplication, "done", "en").await;
    println!("{}", String::from_utf8_lossy(&out));
}
