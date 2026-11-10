//! Re-applies the companion ACL on the router over SSH using saved credentials.
//! Usage: cargo run --example fix_acl -- <path-to-config.json>

use openwrt_companion_lib::settings::Settings;
use openwrt_companion_lib::{snapshot, ssh};
use std::fs;

#[tokio::main]
async fn main() {
    let path = std::env::args().nth(1).expect("usage: fix_acl <config.json>");
    let raw = fs::read_to_string(&path).expect("cannot read config");
    let s: Settings = serde_json::from_str(&raw).expect("cannot parse config");
    let out = ssh::run_command(&s.host, &s.username, &s.password(), &snapshot::acl_setup_command())
        .await
        .expect("ssh command failed");
    println!("{out}");
}
