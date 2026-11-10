//! Builds one snapshot exactly like the app does and prints Wi-Fi devices.
//! Usage: cargo run --example get_snap -- <path-to-config.json>

use openwrt_companion_lib::settings::{Settings, SettingsStore};
use openwrt_companion_lib::snapshot::{build_snapshot, AppState};
use std::fs;
use std::sync::RwLock;

#[tokio::main]
async fn main() {
    let path = std::env::args().nth(1).expect("usage: get_snap <config.json>");
    let raw = fs::read_to_string(&path).expect("cannot read config");
    let s: Settings = serde_json::from_str(&raw).expect("cannot parse config");
    let state = AppState {
        settings: SettingsStore {
            path: Default::default(),
            inner: RwLock::new(s),
        },
        client: RwLock::new(None),
        last: RwLock::new(None),
    };
    let snap = build_snapshot(&state).await.expect("snapshot failed");
    println!("stats rx={:?} tx={:?} uptime={:?}", snap.stats.rx_bytes, snap.stats.tx_bytes, snap.stats.uptime);
    println!("status={} limited={} devices={}", snap.status, snap.limited, snap.devices.len());
    for d in &snap.devices {
        println!(
            "  {} wifi={} online={} kind={} signal={:?} name={}",
            d.mac, d.wifi, d.online, d.kind, d.signal, d.name
        );
    }
}
