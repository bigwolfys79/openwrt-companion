//! Runs an arbitrary command on the router over SSH using saved credentials.
//! Usage: cargo run --example ssh_cmd -- <config.json> <command...>

use openwrt_companion_lib::settings::Settings;
use openwrt_companion_lib::ssh;
use std::fs;

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.remove(0);
    let cmd = args.join(" ");
    let raw = fs::read_to_string(&path).expect("cannot read config");
    let s: Settings = serde_json::from_str(&raw).expect("cannot parse config");
    match ssh::run_command(&s.host, &s.username, &s.password(), &cmd).await {
        Ok(out) => println!("{out}"),
        Err(e) => eprintln!("ERR: {e}"),
    }
}
