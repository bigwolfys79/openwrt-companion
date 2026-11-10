# OpenWrt Companion

**English** | [Русский](README.md)

![Version](https://img.shields.io/badge/version-1.2.1-blue) ![Platform](https://img.shields.io/badge/platform-Windows%20x64-lightgrey) ![License](https://img.shields.io/badge/license-CC%20BY--NC%204.0-orange)

A compact desktop companion for your **OpenWrt** router: a live "who's online and how the router is doing" panel + an animated tray icon. No cloud, no web UI — everything straight from your router.

430×700 px window, always-on-top, hides to tray — sits in the corner of your screen and shows your network in real time.

## ✨ Features

### Monitoring
- **Stats tiles**: uptime, internet speed ↓/↑ with a sparkline, RAM, temperature (all thermal zones + hwmon, hottest sensor wins)
- **Real-time device list**: IP, name, type (PC/phone/TV/IoT), Wi-Fi signal, per-client speed ↓/↑
- **Tabs**: Active / Wi-Fi / LAN / Blocked / Archive
- **Animated tray icon**: 4 bars by traffic level, green/red flash when devices connect/disconnect, top-10 device menu with live speeds + Wi-Fi/router reboot right from the tray
- **Windows notifications**: device connected/disconnected, router unreachable

### Interface
- **Two languages — English and Russian**: switcher in settings, choice is persisted; everything is translated, including the tray menu
- **Dark & light themes**: toggle in settings, applied instantly
- **Bilingual installer**: language selection at setup
- About section: version, license, project page link

### Control
- Device renaming (aliases), 📌 pinning, manual icon type
- ⏏ **Disconnect a client from Wi-Fi** (deauth) and 🚫 **firewall blocking** (per-MAC rule)
- Restart a radio (2.4/5 GHz separately) or the whole router
- One-click Wi-Fi channel change

### Air scan
- List of neighbouring networks with signal and security, co-channel/overlap highlighting
- Verdict on your channel (busy / overlapped / clean) with a **freer-channel recommendation**
- Full 5 GHz channel set including **DFS** (52–64, 100–140) with CAC hints (~1 min radar check)
- Correct overlap math for 20/40/80/160 MHz widths

### Diagnostics & maintenance
- Ping gateway / 8.8.8.8, `logread`, memory & disk
- **Config backup** (`sysupgrade -b`) to tar.gz in one click → Downloads folder
- "Limited mode" diagnostics with one-click ACL auto-fix (writes `/usr/share/rpcd/acl.d/` for you)

### Security
- Router password stored locally, encrypted with **Windows DPAPI** (bound to your account)
- **SSH host key pinning**: the router's key fingerprint is learned on first connect and verified on every next one; key change (reflash/intercept) blocks the connection. "Forget SSH key" button for reflashes
- Zero cloud: the app talks only to your router

### Multi-router
- Multiple profiles (home / dacha / …): switch in two clicks, each with its own name, host, username and password

## 🚀 Installation

1. Download `OpenWrt Companion_1.2.1_x64-setup.exe` from the [Releases](../../releases) page
2. Pick the installer language (english / русский) and install — settings open on first launch
3. Enter the router address, username and password → "Test"

### Router requirements
- OpenWrt with **LuCI** installed (the app uses its RPC endpoint `/ubus`)
- A user with access to the `luci` ubus groups (usually root)
- For extended data (client signal, air scan, LAN speeds) — `iwinfo` access; if the ACL denies it, the app shows a banner and offers a **one-click auto-fix**
- For SSH features (diagnostics, backup, ACL fix) — dropbear enabled with password login

## 🛠 Building from source

```bash
# requires Node.js 18+, Rust (stable), Windows: MSVC Build Tools + WebView2
npm install
npm run tauri build
# installer: src-tauri/target/release/bundle/nsis/*.exe
```

## 🧩 How it works

| Channel | What it collects |
|---|---|
| **ubus via LuCI RPC** (`/ubus`, JSON-RPC) | Wi-Fi devices, interfaces, WAN counters, ARP, DHCP, uci, system board/info, iwinfo |
| **SSH** (russh, password + key pinning) | diagnostics, `sysupgrade -b` backup, ACL auto-fix |

Polling runs in the background (every 5 s by default, configurable 2–30 s); speeds are computed as counter differentials between polls.

**Stack:** [Tauri 2](https://v2.tauri.app/) (Rust) + React + TypeScript, russh, DPAPI, bilingual NSIS installer. Release build ~3.7 MB, ~50 MB RAM.

## 🔒 Security note

The app is meant for **your own router on your home network**. The password is stored encrypted (DPAPI), the SSH host key is pinned, but: never expose LuCI RPC or SSH to WAN, use a strong root password.

## License

[CC BY-NC 4.0](LICENSE) (Attribution-NonCommercial):

- **Non-commercial use** — free: copy, modify, distribute for any non-commercial purpose
- **Commercial use and resale prohibited** without written permission from the author
- **Attribution required** — even in modified versions you must keep the author credit and a link to the original repo [bigwolfys79/openwrt-companion](https://github.com/bigwolfys79/openwrt-companion), and state that changes were made
