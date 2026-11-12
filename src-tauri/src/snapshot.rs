use crate::settings::SettingsStore;
use crate::ubus::UbusClient;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use parking_lot::RwLock;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub mac: String,
    pub name: String,
    pub ip: String,
    pub signal: Option<i64>,
    pub wifi: bool,
    /// true when the device is currently reachable (assoclist or ARP entry)
    pub online: bool,
    pub iface: Option<String>,
    pub radio: Option<String>,
    pub band: Option<String>,
    pub kind: String,
    pub alias: Option<String>,
    pub blocked: bool,
    /// pinned by the user (always shown first)
    #[serde(default)]
    pub pinned: bool,
    /// cumulative bytes received from this client (Wi-Fi station counter),
    /// diffed by the frontend between polls to show download speed
    #[serde(default)]
    pub rx_bytes: Option<u64>,
    /// cumulative bytes sent to this client (Wi-Fi station counter)
    #[serde(default)]
    pub tx_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Radio {
    pub device: String,
    pub band: String,
    pub channel: Option<i64>,
    /// channel width in MHz from htmode (HT40 -> 40, HE80 -> 80, none -> 20)
    #[serde(default)]
    pub width: Option<i64>,
    pub up: bool,
    pub networks: Vec<NetInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetInfo {
    pub ifname: String,
    pub ssid: Option<String>,
}

/// rx/tx byte counters of a WAN-side candidate device.
/// The frontend diffs them between polls and shows the busiest one.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WanCounter {
    pub iface: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub uptime: Option<u64>,
    pub load: Option<[f64; 3]>,
    pub wan_ip: Option<String>,
    pub rx_bytes: Option<u64>,
    pub tx_bytes: Option<u64>,
    pub mem_total: Option<u64>,
    pub mem_free: Option<u64>,
    /// SoC temperature in °C (from thermal_zone0), if available
    pub temp: Option<f64>,
    /// auto-detected WAN device name (l3 device of the wan interface)
    pub wan_iface: Option<String>,
    /// counters of all WAN-side candidates; the busiest one is the real uplink
    /// (on DSA routers the logical "wan" port may lag behind its eth master)
    #[serde(default)]
    pub wan_counters: Vec<WanCounter>,
    /// flow offloading mode on the router ("hw" | "sw"), if detected.
    /// With offload enabled, DSA port counters bypass forwarded traffic.
    #[serde(default)]
    pub offload: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub status: String, // "ok" | "offline"
    /// wall-clock time of snapshot creation, ms since epoch (for speed diffing)
    #[serde(default)]
    pub ts: u64,
    pub error: Option<String>,
    /// true when the session lacks iwinfo/hostapd/network.wireless ACLs
    /// (Wi-Fi client detection and radio reboot fall back to limited paths).
    pub limited: bool,
    pub devices: Vec<Device>,
    pub radios: Vec<Radio>,
    pub stats: Stats,
    pub blocked_macs: Vec<String>,
}

pub struct AppState {
    pub settings: SettingsStore,
    pub client: RwLock<Option<UbusClient>>,
    /// переиспользуемая SSH-сессия для nftables-accounting (без реконнекта
    /// на каждом опросе); сбрасывается вместе с ubus-клиентом
    pub ssh: crate::ssh::SshPool,
}

impl AppState {
    pub fn client_or_new(&self) -> UbusClient {
        let mut guard = self.client.write();
        if let Some(c) = guard.as_ref() {
            return c.clone();
        }
        let s = self.settings.inner.read().clone();
        let c = UbusClient::new(&s.host, &s.username, &s.password());
        *guard = Some(c.clone());
        c
    }

    pub fn reset_client(&self) {
        *self.client.write() = None;
        self.ssh.reset();
    }
}

fn kind_from_name(name: &str) -> String {
    let n = name.to_lowercase();
    // order matters: more specific keywords first
    for (kw, kind) in [
        ("watch", "watch"),
        ("ipad", "tablet"), ("tablet", "tablet"), ("tab ", "tablet"),
        ("laptop", "laptop"), ("notebook", "laptop"), ("thinkpad", "laptop"), ("macbook", "laptop"),
        ("firestick", "tv"), ("fire stick", "tv"), ("chromecast", "tv"), ("roku", "tv"),
        ("appletv", "tv"), ("apple tv", "tv"), ("android tv", "tv"),
        ("tv", "tv"), ("shield", "tv"), ("bravia", "tv"),
        ("esp", "iot"), ("shelly", "iot"), ("tuya", "iot"), ("sonoff", "iot"),
        ("cam", "iot"), ("bulb", "iot"), ("lamp", "iot"), ("sensor", "iot"),
        ("printer", "printer"),
        ("redmi", "phone"), ("poco", "phone"), ("xiaomi", "phone"), ("mi ", "phone"),
        ("iphone", "phone"), ("android", "phone"), ("pixel", "phone"),
        ("galaxy", "phone"), ("samsung", "phone"), ("huawei", "phone"), ("honor", "phone"),
        ("oneplus", "phone"), ("realme", "phone"), ("oppo", "phone"), ("vivo", "phone"),
        ("phone", "phone"),
        ("pc", "pc"), ("desktop", "pc"), ("win", "pc"),
    ] {
        if n.contains(kw) {
            return kind.to_string();
        }
    }
    "unknown".into()
}

fn str_at(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().map(|s| s.to_string())
}

/// Per-LAN-host traffic accounting over SSH: ensures an nftables table with
/// a forward-hook chain and per-IP counter rules (one for download
/// `ip daddr`, one for upload `ip saddr`, keyed off the ARP cache), then
/// prints the counters. Returns IP -> (upload_bytes, download_bytes).
/// Note: with HW/SW flow offloading the forward hook undercounts offloaded
/// flows — the same caveat as the DSA port counters.
async fn lan_accounting(state: &AppState) -> Result<(HashMap<String, (u64, u64)>, HashSet<String>), String> {
    let s = state.settings.inner.read().clone();
    let pass = s.password();
    if pass.is_empty() {
        return Err("пароль не задан".into());
    }
    let script = "\
        nft list table inet oc_acc >/dev/null 2>&1 || \
        printf 'table inet oc_acc {\\n chain oc_fwd {\\n  type filter hook forward priority -150; policy accept;\\n }\\n}\\n' | nft -f - ; \
        for ip in $(awk '$3==\"0x2\"{print $1}' /proc/net/arp); do \
          nft list chain inet oc_acc oc_fwd | grep -q \"oc-d-$ip\" || \
            nft add rule inet oc_acc oc_fwd ip daddr $ip counter comment \\\"oc-d-$ip\\\" ; \
          nft list chain inet oc_acc oc_fwd | grep -q \"oc-s-$ip\" || \
            nft add rule inet oc_acc oc_fwd ip saddr $ip counter comment \\\"oc-s-$ip\\\" ; \
        done ; \
        nft list chain inet oc_acc oc_fwd | grep 'oc-' ;         echo ===NEIGH=== ;         ip neigh show";
    // пул держит одну SSH-сессию между опросами: вместо нового
    // TCP+kex+auth каждые несколько секунд — только новый канал
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        state.ssh.run(&s.host, &s.username, &pass, script),
    )
    .await
    .map_err(|_| "SSH таймаут".to_string())??;
    let out = String::from_utf8_lossy(&out).trim().to_string();

    let mut map: HashMap<String, (u64, u64)> = HashMap::new();
    let mut reachable: HashSet<String> = HashSet::new();
    // counters and `ip neigh` are separated by the ===NEIGH=== marker
    let (counter_out, neigh_out) = match out.split_once("===NEIGH===") {
        Some((a, b)) => (a, b),
        None => (out.as_str(), ""),
    };
    for line in counter_out.lines() {
        let mut bytes: Option<u64> = None;
        let mut dir = ' ';
        let mut ip: Option<String> = None;
        for tok in line.split_whitespace() {
            if tok == "bytes" {
                if let Some(v) = tok_next_num(line, tok) {
                    bytes = Some(v);
                }
            } else if let Some(rest) = tok.trim_matches('"').strip_prefix("oc-") {
                let mut it = rest.splitn(2, '-');
                dir = it.next().unwrap_or(" ").chars().next().unwrap_or(' ');
                ip = it.next().map(String::from);
            }
        }
        if let (Some(b), Some(ip)) = (bytes, ip) {
            let e = map.entry(ip).or_insert((0, 0));
            match dir {
                's' => e.0 = b, // upload: traffic sourced from the host
                'd' => e.1 = b, // download: traffic destined to the host
                _ => {}
            }
        }
    }
    // `ip neigh show` lines: "192.168.1.2 dev br-lan lladdr AA:BB:... REACHABLE"
    for line in neigh_out.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.len() < 4 {
            continue;
        }
        let mac = toks.iter().position(|t| t.eq_ignore_ascii_case("lladdr")).and_then(|i| toks.get(i + 1).copied());
        let state = toks.last().copied().unwrap_or("");
        let fresh = matches!(state, "REACHABLE" | "DELAY" | "PROBE");
        if let (Some(m), true) = (mac, fresh) {
            reachable.insert(m.to_uppercase().replace('-', ":"));
        }
    }
    Ok((map, reachable))
}

/// helper: number following a "bytes" token in the same whitespace stream
fn tok_next_num(line: &str, tok: &str) -> Option<u64> {
    let mut it = line.split_whitespace();
    while let Some(t) = it.next() {
        if t == tok {
            return it.next().and_then(|v| v.parse::<u64>().ok());
        }
    }
    None
}

/// Build a full snapshot from the router. Sub-steps degrade gracefully,
/// but an unreachable router / failed login is propagated as Err.
pub async fn build_snapshot(state: &AppState) -> Result<Snapshot, String> {
    let client = state.client_or_new();
    let aliases: HashMap<String, String> =
        state.settings.inner.read().aliases.clone();
    let kind_overrides: HashMap<String, String> =
        state.settings.inner.read().kinds.clone();
    let pinned_list: Vec<String> = state.settings.inner.read().pinned.clone();

    // reachability + auth check
    client.login().await?;
    client.call("system", "board", json!({})).await?;

    // --- независимые RPC выполняются параллельно через tokio::join! ---
    // раньше каждый ждал предыдущего, растягивая цикл опроса в 2-3 раза.
    // Только network.device status (зависит от wan_device из dump) и
    // iwinfo assoclist (зависит от if_to_radio) остаются последовательными.
    let (wl_res, arp_res, dump_res, hints_res, dhcp_res, fw_res, info_res) = tokio::join!(
        client.call("luci-rpc", "getWirelessDevices", json!({})),
        client.call("file", "exec", json!({"command": "/bin/cat", "params": ["/proc/net/arp"]})),
        client.call("network.interface", "dump", json!({})),
        client.call("luci-rpc", "getHostHints", json!({})),
        client.call("luci-rpc", "getDHCPLeases", json!({})),
        client.call("uci", "get", json!({"config": "firewall"})),
        client.call("system", "info", json!({})),
    );

    // wifi interface -> radio mapping + band info.
    // Primary source: luci-rpc getWirelessDevices (always ACL-allowed for LuCI sessions).
    // It returns {radio0: {up, config:{band:"2g", channel}, interfaces:[{ifname, config:{ssid}, stations:[...]}]}}.
    let mut radios: Vec<Radio> = Vec::new();
    let mut if_to_radio: HashMap<String, (String, String)> = HashMap::new(); // ifname -> (radio, band)
    let mut station_clients: HashMap<String, (Option<i64>, String, String, Option<u64>, Option<u64>)> = HashMap::new(); // mac -> (signal, iface, radio, rx_bytes, tx_bytes)
    if let Ok(wl) = wl_res {
        if let Some(map) = wl.as_object() {
            for (radio_dev, r) in map {
                // band comes lowercase ("2g"/"5g") from uci - normalize
                let band = str_at(r, &["config", "band"])
                    .map(|b| b.to_uppercase())
                    .unwrap_or_default();
                let channel = r
                    .get("config")
                    .and_then(|c| c.get("channel"))
                    .and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.parse().ok())));
                let up = r.get("up").and_then(|u| u.as_bool()).unwrap_or(false);
                let mut networks = Vec::new();
                if let Some(ifs) = r.get("interfaces").and_then(|i| i.as_array()) {
                    for i in ifs {
                        let ifname = str_at(i, &["ifname"]).unwrap_or_default();
                        let ssid = str_at(i, &["config", "ssid"]);
                        if !ifname.is_empty() {
                            if_to_radio.insert(ifname.clone(), (radio_dev.clone(), band.clone()));
                            // fallback client list when iwinfo is ACL-denied
                            if let Some(sts) = i.get("stations").and_then(|s| s.as_array()) {
                                for st in sts {
                                    let mac = match st
                                        .get("mac")
                                        .or_else(|| st.get("addr"))
                                        .and_then(|m| m.as_str())
                                    {
                                        Some(m) => m.to_uppercase().replace('-', ":"),
                                        None => continue,
                                    };
                                    let signal = st.get("signal").and_then(|s| s.as_i64());
                                    station_clients.insert(
                                        mac,
                                        (signal, ifname.clone(), radio_dev.clone(), None, None),
                                    );
                                }
                            }
                            networks.push(NetInfo { ifname, ssid });
                        }
                    }
                }
                let width = str_at(r, &["config", "htmode"])
                    .and_then(|h| {
                        // "HT40" / "VHT80" / "HE160" / "EHT80" -> digits
                        let digits: String = h.chars().filter(|c| c.is_ascii_digit()).collect();
                        digits.parse::<i64>().ok()
                    })
                    .or(Some(20));
                radios.push(Radio { device: radio_dev.clone(), band, channel, width, up, networks });
            }
        }
        radios.sort_by(|a, b| a.device.cmp(&b.device));
    }

    // wifi clients: prefer iwinfo assoclist (richer data; needs the ACL grant),
    // fall back to stations from getWirelessDevices
    let mut wifi_clients: HashMap<String, (Option<i64>, String, String, Option<u64>, Option<u64>)> = station_clients;
    // результат этого же вызова служит флагом ограниченных ACL — раньше
    // iwinfo devices вызывался повторно в конце build_snapshot (лишний RPC)
    let iwinfo_res = client.call("iwinfo", "devices", json!({})).await;
    let limited = iwinfo_res.is_err();
    if let Ok(devs) = iwinfo_res {
        let names: Vec<String> = devs
            .get("devices")
            .and_then(|d| d.as_array())
            // elements may be plain strings ("phy1-ap0") or {"device": "..."} objects
            .map(|a| a.iter().filter_map(|d| d.as_str().map(String::from).or_else(|| d.get("device").and_then(|x| x.as_str()).map(String::from))).collect())
            .unwrap_or_default();
        for dev in names {
            if let Ok(list) = client.call("iwinfo", "assoclist", json!({"device": dev})).await {
                if let Some(arr) = list.get("results").and_then(|r| r.as_array()) {
                    for c in arr {
                        let mac = match c.get("mac").and_then(|m| m.as_str()) {
                            Some(m) => m.to_uppercase().replace('-', ":"),
                            None => continue,
                        };
                        let signal = c.get("signal").and_then(|s| s.as_i64());
                        let (radio, _) = if_to_radio
                            .get(&dev)
                            .cloned()
                            .unwrap_or((String::new(), String::new()));
                        let rx = c.get("rx").and_then(|r| r.get("bytes")).and_then(|v| v.as_u64());
                        let tx = c.get("tx").and_then(|t| t.get("bytes")).and_then(|v| v.as_u64());
                        wifi_clients.insert(mac, (signal, dev.clone(), radio.clone(), rx, tx));
                    }
                }
            }
        }
    }

    // active LAN clients from the ARP cache (needs the file-exec ACL grant)
    let mut arp_clients: HashSet<String> = HashSet::new();
    // complete ARP entries on non-LAN sides (wan etc.) - hosts to hide
    let mut other_macs: HashSet<String> = HashSet::new();
    if let Ok(ex) = arp_res {
        if let Some(txt) = ex.get("stdout").and_then(|s| s.as_str()) {
            for line in txt.lines().skip(1) {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() >= 6 {
                    // 0x2 = ATF_COM (complete entry); skip incomplete probes
                    let complete = i64::from_str_radix(cols[2].trim_start_matches("0x"), 16)
                        .map(|f| f & 2 != 0)
                        .unwrap_or(false);
                    if complete {
                        let mac = cols[3].to_uppercase().replace('-', ":");
                        let dev = cols[5];
                        // only LAN-side entries count as active clients;
                        // WAN entries (e.g. the upstream modem) are not devices
                        if dev.starts_with("br-lan") || dev.starts_with("lan") || dev.starts_with("eth0") {
                            arp_clients.insert(mac);
                        } else {
                            other_macs.insert(mac);
                        }
                    }
                }
            }
        }
    }

    // the router's own interface IPs (to exclude itself / WAN uplink from the list)
    let mut own_ips: HashSet<String> = HashSet::new();
    let iface_dump = dump_res.ok();
    if let Some(dump) = &iface_dump {
        if let Some(ifs) = dump.get("interface").and_then(|i| i.as_array()) {
            for i in ifs {
                if let Some(addrs) = i.get("ipv4-address").and_then(|a| a.as_array()) {
                    for a in addrs {
                        if let Some(ip) = a.get("address").and_then(|x| x.as_str()) {
                            own_ips.insert(ip.to_string());
                        }
                    }
                }
            }
        }
    }

    // host hints (names + ips)
    let mut hints: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    if let Ok(h) = hints_res {
        // result is either {"devices": {MAC: hint}} or a top-level {MAC: hint} map
        let hint_obj: Vec<(String, Value)> = h
            .get("devices")
            .and_then(|d| d.as_object())
            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_else(|| {
                h.as_object()
                    .map(|o| o.iter().filter(|(k, _)| k.len() == 17).map(|(k, v)| (k.clone(), v.clone())).collect())
                    .unwrap_or_default()
            });
        for (mac, v) in hint_obj {
            let mac = mac.to_uppercase().replace('-', ":");
            let name = v.get("name").and_then(|n| n.as_str()).map(String::from);
            let ip = v.get("ipaddrs").and_then(|i| i.as_array()).and_then(|a| a.first()).and_then(|i| i.as_str()).map(String::from);
            hints.insert(mac, (name, ip));
        }
    }

    // dhcp leases
    if let Ok(l) = dhcp_res {
        if let Some(leases) = l.get("dhcp_leases").and_then(|d| d.as_array()) {
            for lease in leases {
                let mac = match lease.get("macaddr").and_then(|m| m.as_str()) {
                    Some(m) => m.to_uppercase().replace('-', ":"),
                    None => continue,
                };
                let entry = hints.entry(mac).or_insert((None, None));
                if entry.0.is_none() {
                    entry.0 = lease.get("hostname").and_then(|n| n.as_str()).map(String::from);
                }
                if entry.1.is_none() {
                    entry.1 = lease.get("ipaddr").and_then(|i| i.as_str()).map(String::from);
                }
            }
        }
    }

    // blocked MACs from uci firewall rules named oc-block-*
    let mut blocked_macs: Vec<String> = Vec::new();
    if let Ok(fw) = fw_res {
        if let Some(values) = fw.get("values").and_then(|v| v.as_object()) {
            for (_, sec) in values {
                let stype = sec.get(".type").and_then(|t| t.as_str()).unwrap_or("");
                let name = sec.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if stype == "rule" && name.starts_with("oc-block") {
                    let macs = sec.get("src_mac").and_then(|m| m.as_str()).map(String::from)
                        .or_else(|| sec.get("src_mac").and_then(|m| m.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")));
                    if let Some(m) = macs {
                        blocked_macs.push(m.to_uppercase().replace('-', ":"));
                    }
                }
            }
        }
    }

    // per-LAN-host traffic counters via nftables accounting (installed and
    // read over SSH with the saved credentials; best effort). Wi-Fi clients
    // use the richer assoclist station counters instead.
    // Ok = SSH worked; Err/None = fall back to plain ARP reachability
    let lan_res = if arp_clients.is_empty() { None } else { Some(lan_accounting(&state).await) };
    let (lan_counters, lan_reachable) = match &lan_res {
        Some(Ok(v)) => v.clone(),
        _ => (HashMap::new(), HashSet::new()),
    };
    let use_neigh = matches!(&lan_res, Some(Ok(_)));

    // devices
    let mut devices: Vec<Device> = Vec::new();
    for (mac, (name, ip)) in &hints {
        // hide the router itself and WAN-side hosts (upstream modem etc.)
        if other_macs.contains(mac) {
            continue;
        }
        if let Some(ip) = ip {
            if own_ips.contains(ip) {
                continue;
            }
        }
        let wifi = wifi_clients.contains_key(mac);
        // with SSH neigh data: only hosts the router actually saw recently are
        // online; stale ARP entries (device left minutes ago) drop off
        let online = wifi
            || (if use_neigh {
                lan_reachable.contains(mac)
            } else {
                arp_clients.contains(mac)
            });
        let (signal, iface, radio, _, _) = wifi_clients.get(mac).cloned().unwrap_or((None, String::new(), String::new(), None, None));
        let band = (!radio.is_empty()).then(|| {
            radios.iter().find(|r| r.device == radio).map(|r| r.band.clone()).unwrap_or_default()
        });
        let alias = aliases.get(mac).cloned();
        // traffic counters: Wi-Fi -> station stats; LAN -> nft per-IP accounting
        let (_, _, _, mut rx_bytes, mut tx_bytes) = wifi_clients.get(mac).cloned().unwrap_or((None, String::new(), String::new(), None, None));
        if !wifi {
            if let Some(ip_str) = ip.as_deref() {
                if let Some(&(r, t)) = lan_counters.get(ip_str) {
                    rx_bytes = Some(r); // upload: traffic from the device (src rule)
                    tx_bytes = Some(t); // download: traffic to the device (dst rule)
                }
            }
        }
        let base_name = name.clone().unwrap_or_else(|| "Unknown device".into());
        let kind = kind_overrides
            .get(mac)
            .cloned()
            .unwrap_or_else(|| kind_from_name(&base_name));
        devices.push(Device {
            mac: mac.clone(),
            name: base_name,
            ip: ip.clone().unwrap_or_default(),
            signal,
            wifi,
            online,
            iface: (!iface.is_empty()).then_some(iface),
            radio: (!radio.is_empty()).then_some(radio),
            band: band.filter(|b| !b.is_empty()),
            kind,
            alias,
            blocked: blocked_macs.iter().any(|b| mac.contains(b.as_str())),
            rx_bytes,
            tx_bytes,
            pinned: pinned_list.iter().any(|m| m == mac),
        });
    }
    devices.sort_by(|a, b| {
        let pa = pinned_list.iter().position(|m| m == &a.mac).unwrap_or(usize::MAX);
        let pb = pinned_list.iter().position(|m| m == &b.mac).unwrap_or(usize::MAX);
        pa.cmp(&pb)
            .then_with(|| b.online.cmp(&a.online))
            .then_with(|| b.wifi.cmp(&a.wifi))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    // stats
    let mut stats = Stats::default();
    if let Ok(info) = info_res {
        stats.uptime = info.get("uptime").and_then(|u| u.as_u64());
        stats.mem_total = info.get("memory").and_then(|m| m.get("total")).and_then(|t| t.as_u64());
        stats.mem_free = info.get("memory").and_then(|m| m.get("free")).and_then(|t| t.as_u64());
        if let Some(load) = info.get("load").and_then(|l| l.as_array()) {
            stats.load = Some([
                load.get(0).and_then(|v| v.as_f64()).unwrap_or(0.0) / 65536.0,
                load.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0) / 65536.0,
                load.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0) / 65536.0,
            ]);
        }
    }
    // WAN detection from network.interface dump (network.interface.wan is
    // ACL-restricted on some builds). The wan* logical interface's l3 device
    // names the real uplink (wan / eth1 / wwan0 / usb0 / pppoe-wan ...).
    // Fallback: the interface carrying the default route.
    let wan_iface = iface_dump
        .as_ref()
        .and_then(|dump| dump.get("interface"))
        .and_then(|i| i.as_array())
        .and_then(|ifs| {
            // prefer an up wan* interface, then any wan*, then the default route
            ifs.iter()
                .find(|i| {
                    str_at(i, &["interface"]).map(|n| n.starts_with("wan")).unwrap_or(false)
                        && i.get("up").and_then(|u| u.as_bool()).unwrap_or(false)
                })
                .or_else(|| {
                    ifs.iter().find(|i| {
                        str_at(i, &["interface"]).map(|n| n.starts_with("wan")).unwrap_or(false)
                    })
                })
                .or_else(|| {
                    ifs.iter().find(|i| {
                        i.get("route").and_then(|r| r.as_array()).map(|routes| {
                            routes.iter().any(|rt| rt.get("target").and_then(|t| t.as_str()) == Some("0.0.0.0"))
                        }).unwrap_or(false)
                    })
                })
        });
    stats.wan_ip = wan_iface.and_then(|w| {
        w.get("ipv4-address").and_then(|a| a.as_array()).and_then(|a| a.first()).and_then(|a| a.get("address")).and_then(|a| a.as_str()).map(String::from)
    });
    // l3_device covers pppoe; plain device for dhcp/wwan uplinks
    let wan_device: Option<String> = wan_iface
        .and_then(|w| str_at(w, &["l3_device"]).or_else(|| str_at(w, &["device"])))
        .filter(|d| !d.is_empty());
    stats.wan_iface = wan_device.clone();
    if let Ok(nd) = client.call("network.device", "status", json!({})).await {
        // the result may be the device map directly, or wrapped in {"devices": {...}}
        let devs_obj = nd
            .get("devices")
            .and_then(|d| d.as_object())
            .or_else(|| nd.as_object())
            .filter(|o| o.iter().any(|(k, _)| k != "devices"));
        if let Some(devs) = devs_obj {
            // the auto-detected WAN device first, then legacy wan-ish names
            let mut keys: Vec<String> = Vec::new();
            if let Some(d) = &wan_device {
                keys.push(d.clone());
            }
            for k in ["wan", "eth0", "eth1"] {
                if !keys.iter().any(|x| x == k) {
                    keys.push(k.to_string());
                }
            }
            for key in &keys {
                // never treat LAN-side / virtual devices as WAN candidates
                let k = key.as_str();
                if k == "lo"
                    || k == "br-lan"
                    || k.starts_with("lan")
                    || k.starts_with("br-")
                    || k.starts_with("ifb")
                    || k.starts_with("phy")
                    || k.starts_with("wlan")
                    || k.starts_with("ra")
                {
                    continue;
                }
                if let Some(d) = devs.get(key) {
                    if let Some(st) = d.get("statistics") {
                        let rx = st.get("rx_bytes").and_then(|v| v.as_u64());
                        let tx = st.get("tx_bytes").and_then(|v| v.as_u64());
                        if let (Some(rx), Some(tx)) = (rx, tx) {
                            stats.wan_counters.push(WanCounter { iface: key.clone(), rx_bytes: rx, tx_bytes: tx });
                        }
                        if stats.rx_bytes.is_none() {
                            stats.rx_bytes = rx;
                            stats.tx_bytes = tx;
                        }
                    }
                }
            }
        }
    }
    // flow offloading state (from /etc/config/firewall): with offload enabled,
    // forwarded traffic bypasses the DSA port counters
    if let Ok(ex) = client
        .call("file", "exec", json!({"command": "/bin/cat", "params": ["/etc/config/firewall"]}))
        .await
    {
        if let Some(txt) = ex.get("stdout").and_then(|s| s.as_str()) {
            let mut sw = false;
            let mut hw = false;
            for line in txt.lines() {
                let l = line.trim().to_lowercase();
                let enabled = l.contains("'1'") || l.contains("\"1\"");
                if l.starts_with("option flow_offloading_hw") {
                    hw = enabled;
                } else if l.starts_with("option flow_offloading") {
                    sw = enabled;
                }
            }
            if hw {
                stats.offload = Some("hw".into());
            } else if sw {
                stats.offload = Some("sw".into());
            }
        }
    }

    // SoC temperature. thermal_zone0 is NOT always the CPU sensor: on x86 it is
    // usually the ACPI ambient zone (acpitz), some SoC zones sit at a fixed
    // value or refresh rarely (see openwrt issue #6121). Read every thermal
    // zone + hwmon input and take the max — the CPU die is the hottest sensor.
    let mut temps: Vec<f64> = Vec::new();
    let zone_script = "for z in /sys/class/thermal/thermal_zone*/temp /sys/class/hwmon/hwmon*/temp*_input; do cat \"$z\" 2>/dev/null; done";
    if let Ok(ex) = client
        .call("file", "exec", json!({"command": "/bin/sh", "params": ["-c", zone_script]}))
        .await
    {
        if let Some(txt) = ex.get("stdout").and_then(|s| s.as_str()) {
            for v in txt.split_whitespace() {
                if let Ok(milli) = v.trim().parse::<f64>() {
                    if milli > 0.0 && milli < 150_000.0 { // plausible °C×1000
                        temps.push(milli);
                    }
                }
            }
        }
    }
    // fallback for limited ACLs (only /bin/cat exec is permitted): zone0 as before
    if temps.is_empty() {
        if let Ok(ex) = client
            .call("file", "exec", json!({"command": "/bin/cat", "params": ["/sys/class/thermal/thermal_zone0/temp"]}))
            .await
        {
            if let Some(txt) = ex.get("stdout").and_then(|s| s.as_str()) {
                if let Ok(milli) = txt.trim().parse::<f64>() {
                    if milli > 0.0 {
                        temps.push(milli);
                    }
                }
            }
        }
    }
    if let Some(milli) = temps.iter().cloned().fold(None::<f64>, |a, b| Some(a.map_or(b, |m: f64| m.max(b)))) {
        stats.temp = Some((milli / 1000.0 * 10.0).round() / 10.0);
    }

    // limited ACLs are detected by the single iwinfo devices call above


    Ok(Snapshot {
        status: "ok".into(),
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        error: None,
        limited,
        devices,
        radios,
        stats,
        blocked_macs,
    })
}

/// The SSH command that installs the companion ACL and restarts rpcd.
/// Shared by the frontend hint and the automatic `ssh_fix_limited` command.
pub fn acl_setup_command() -> String {
    "cat > /usr/share/rpcd/acl.d/owrt-companion.json <<'EOF'\n\
     {\"companion\":{\"description\":\"OpenWrt Companion\",\"read\":{\"ubus\":{\"iwinfo\":[\"devices\",\"info\",\"assoclist\",\"scan\"],\"network.wireless\":[\"status\"],\"network.interface\":[\"status\",\"dump\"],\"network.device\":[\"status\"],\"luci-rpc\":[\"getHostHints\",\"getDHCPLeases\",\"getWirelessDevices\"],\"system\":[\"info\",\"board\"]},\"uci\":{\"firewall\":[\"*\"]}},\"write\":{\"ubus\":{\"hostapd.*\":[\"del_client\"],\"network.wireless\":[\"up\",\"down\"],\"uci\":[\"add\",\"set\",\"delete\",\"commit\",\"apply\"],\"file\":[\"exec\"]},\"uci\":{\"firewall\":[\"*\"]},\"file\":{\"/bin/cat\":[\"exec\"]}}}}\n\
     EOF\n/etc/init.d/rpcd restart"
        .to_string()
}
