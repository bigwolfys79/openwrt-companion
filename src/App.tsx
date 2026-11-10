import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { isEnabled as autostartIsEnabled, enable as autostartEnable, disable as autostartDisable } from "@tauri-apps/plugin-autostart";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { Device, Radio, Settings, Snapshot } from "./types";
import { t, setLang, getLang, normalizeLang, REPO_URL, type Lang } from "./i18n";

/* ---------- helpers ---------- */

// speed in bits/s, formatted like Windows Task Manager
function fmtBits(bytesPerSec: number | null): string {
  if (bytesPerSec == null || !Number.isFinite(bytesPerSec)) return "—";
  const bits = bytesPerSec * 8;
  if (bits < 1_000_000) return `${(bits / 1000).toFixed(bits < 10_000 ? 1 : 0)} ${t("u.kbit")}`;
  if (bits < 1_000_000_000) return `${(bits / 1_000_000).toFixed(1)} ${t("u.mbit")}`;
  return `${(bits / 1_000_000_000).toFixed(2)} ${t("u.gbit")}`;
}

// compact rate for device cards ("1.2М", "350к", "—")
function fmtRate(bytesPerSec: number | null): string {
  if (bytesPerSec == null || !Number.isFinite(bytesPerSec)) return "—";
  const bits = bytesPerSec * 8;
  if (bits < 1_000_000) return `${Math.max(1, Math.round(bits / 1000))}к`;
  if (bits < 1_000_000_000) return `${(bits / 1_000_000).toFixed(1)}М`;
  return `${(bits / 1_000_000_000).toFixed(2)}Г`;
}

function fmtUptime(sec: number | null): string {
  if (sec == null) return "—";
  const d = Math.floor(sec / 86400);
  const h = Math.floor((sec % 86400) / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return d > 0 ? `${d}${t("u.d")} ${h}${t("u.h")}` : h > 0 ? `${h}${t("u.h")} ${m}${t("u.m")}` : `${m}${t("u.m")}`;
}

function signalQuality(dbm: number | null): number {
  if (dbm == null) return 0;
  return Math.max(0, Math.min(100, Math.round(((dbm + 90) / 60) * 100)));
}

const KIND_ICONS: Record<string, string> = {
  phone: "📱",
  tablet: "💻",
  laptop: "💼",
  tv: "📺",
  iot: "💡",
  printer: "🖨️",
  watch: "⌚",
  pc: "🖥️",
};

const KIND_KEYS = ["auto", "pc", "laptop", "phone", "tablet", "tv", "printer", "watch", "iot"];

function bandLabel(band: string | null): string {
  if (band === "2G") return "2.4 ГГц";
  if (band === "5G") return "5 ГГц";
  if (band === "6G") return "6 ГГц";
  return band || "Wi-Fi";
}

/* ---------- components ---------- */

function TitleBar({ onSettings, settingsOpen }: { onSettings: () => void; settingsOpen: boolean }) {
  return (
    <div className="titlebar" data-tauri-drag-region>
      <div className="brand">
        <span className="brand-dot" />
        <span data-tauri-drag-region>OpenWrt Companion</span>
      </div>
      <div className="titlebar-actions">
        <button className="icon-btn" title={t("tb.settings")} onClick={onSettings}>
          {settingsOpen ? "✕" : "⚙"}
        </button>
        <button className="icon-btn" title={t("tb.hide")} onClick={() => getCurrentWindow().hide()}>
          ─
        </button>
      </div>
    </div>
  );
}

function Sparkline({ hist }: { hist: { d: number; u: number }[] }) {
  const W = 120;
  const H = 26;
  if (hist.length < 2) return <svg className="spark" viewBox={`0 0 ${W} ${H}`} />;
  const max = Math.max(1, ...hist.map((p) => Math.max(p.d, p.u * 3)));
  const line = (arr: number[]) =>
    arr.map((v, i) => `${(i / (hist.length - 1)) * W},${H - 2 - Math.min(1, v) * (H - 4)}`).join(" ");
  const down = hist.map((p) => p.d / max);
  const up = hist.map((p) => p.u / max);
  return (
    <svg className="spark" viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none">
      <polyline points={line(down)} fill="none" stroke="var(--spark-down)" strokeWidth="1.5" opacity="0.95" />
      <polyline points={line(up)} fill="none" stroke="var(--spark-up)" strokeWidth="1" opacity="0.7" />
    </svg>
  );
}

function StatsBar({ snap, prev, intervalSecs, rows }: { snap: Snapshot; prev: React.RefObject<Snapshot | null>; intervalSecs: number; rows: number }) {
  const s = snap.stats;
  // actual time between snapshots (build takes extra time on top of the poll sleep)
  const realInterval = (() => {
    const prevTs = prev.current?.ts;
    if (snap.ts && prevTs && snap.ts > prevTs) return (snap.ts - prevTs) / 1000;
    return intervalSecs;
  })();
  const speed = (cur: number | null, old: number | null | undefined): number | null => {
    if (cur == null || old == null || old === undefined) return null;
    const v = (cur - old) / Math.max(1, realInterval);
    return Number.isFinite(v) ? Math.max(0, v) : null; // counter reset / bad data -> hide
  };
  const rxSpeed = speed(s.rxBytes, prev.current?.stats?.rxBytes);
  const txSpeed = speed(s.txBytes, prev.current?.stats?.txBytes);
  // pick the busiest WAN-side device: on some routers (e.g. DSA builds)
  // forwarded traffic is counted on the eth master, not the logical "wan" port
  const wanCandidates = s.wanCounters ?? [];
  const prevCandidates = prev.current?.stats?.wanCounters ?? [];
  const rates = wanCandidates
    .map((c) => {
      const p = prevCandidates.find((x) => x.iface === c.iface);
      if (!p) return null;
      const r = speed(c.rxBytes, p.rxBytes);
      const t = speed(c.txBytes, p.txBytes);
      if (r == null || t == null) return null;
      return { iface: c.iface, r, t };
    })
    .filter((x): x is { iface: string; r: number; t: number } => x != null);
  rates.sort((a, b) => b.r + b.t - (a.r + a.t));
  // a "split" device reports down/up separately (up much smaller than down);
  // an aggregated CPU port reports rx ≈ tx ≈ total, directions are inseparable.
  // prefer split-capable devices (e.g. the wan port) over aggregated ones (eth0)
  const splitRates = rates.filter((x) => Math.min(x.r, x.t) < 0.75 * Math.max(x.r, x.t));
  const busiest = splitRates[0] ?? rates[0] ?? null;
  const isSplit = busiest != null && splitRates.includes(busiest);
  let downSpeed: number | null;
  let upSpeed: number | null;
  if (busiest == null) {
    downSpeed = rxSpeed;
    upSpeed = txSpeed;
  } else if (isSplit) {
    downSpeed = busiest.r;
    upSpeed = busiest.t;
  } else {
    // aggregate mode: one direction holds the whole link traffic
    downSpeed = busiest.r;
    upSpeed = null;
  }
  const speedLabel = busiest == null ? null : isSplit ? busiest.iface : `⇅ ${busiest.iface}`;
  // aggregated port + offload: explain why directions can't be split
  const aggregateHint = !isSplit && busiest != null && s.offload
    ? t("stats.offloadHint", { offload: s.offload, iface: busiest.iface })
    : null;

  // traffic history ring buffer for the WAN sparkline (last ~72 polls ≈ 6 мин при 5с)
  const histRef = useRef<{ d: number; u: number }[]>([]);
  const [, bumpHist] = useState(0);
  const histPushed = useRef(0);
  useEffect(() => {
    if (histPushed.current === snap.ts) return; // same snapshot already recorded
    histPushed.current = snap.ts;
    histRef.current.push({ d: downSpeed ?? 0, u: upSpeed ?? 0 });
    if (histRef.current.length > 72) histRef.current.shift();
    bumpHist((n) => n + 1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [snap.ts]);
  const memUsed = s.memTotal != null && s.memFree != null ? s.memTotal - s.memFree : null;
  const memPct = memUsed != null && s.memTotal ? Math.round((memUsed / s.memTotal) * 100) : null;

  return (
    <div className={`stats-grid rows-${rows}`}>
      <div className="stat">
        <div className="stat-label">{t("stats.uptime")}</div>
        <div className="stat-value">{fmtUptime(s.uptime)}</div>
      </div>
      <div className="stat" title={aggregateHint ?? (speedLabel ? t("stats.link", { v: speedLabel }) : undefined)}>
        <div className="stat-label">↓ / ↑{speedLabel ? ` · ${speedLabel}` : ""}</div>
        <div className="stat-value">
          {fmtBits(downSpeed)}
          <span className="stat-dim"> / {fmtBits(upSpeed)}</span>
        </div>
        {aggregateHint && <div className="stat-hint">{t("stats.offload")}</div>}
        <Sparkline hist={histRef.current} />
      </div>
      <div className="stat">
        <div className="stat-label">{t("stats.ram")}</div>
        <div className="stat-value">{memPct != null ? `${memPct}%` : "—"}</div>
      </div>
      <div className="stat">
        <div className="stat-label">{t("stats.temp")}</div>
        <div className="stat-value">{s.temp != null ? `${Math.round(s.temp)}°` : "—"}</div>
      </div>
    </div>
  );
}

function RadioControls({
  radios, busy, onReboot, onScan,
}: {
  radios: Radio[];
  busy: string | null;
  onReboot: (device: string) => void;
  onScan: (device: string, radio: string, channel: number | null, width: number, band: string) => void;
}) {
  if (!radios.length) return null;
  return (
    <div className="radios">
      {radios.map((r) => {
        const ssid = r.networks[0]?.ssid ?? "—";
        const busyHere = busy === r.device;
        return (
          <div className="radio-card" key={r.device}>
            <div className="radio-info">
              <div className="radio-title">
                <span className={`radio-led ${r.up ? "on" : "off"}`} />
                <b>{bandLabel(r.band)}</b>
                <span className="radio-dim">{r.channel != null ? t("radio.ch", { n: r.channel }) : ""}</span>
              </div>
              <div className="radio-ssid">{ssid}</div>
            </div>
            <button
              className="icon-btn"
              disabled={!r.up}
              onClick={() => onScan(r.networks[0]?.ifname ?? "", r.device, r.channel, r.width ?? 20, r.band)}
              title={t("radio.scan")}
            >
              📡
            </button>
            <button
              className="btn btn-ghost"
              disabled={busyHere || !r.up}
              onClick={() => onReboot(r.device)}
              title={t("radio.reboot", { band: bandLabel(r.band) })}
            >
              {busyHere ? <span className="spinner" /> : "⟳"}
            </button>
          </div>
        );
      })}
    </div>
  );
}

function LimitedBanner({ onFixed }: { onFixed: () => void }) {
  const cmd =
    `cat > /usr/share/rpcd/acl.d/owrt-companion.json <<'EOF'\n` +
    `{"companion":{"description":"OpenWrt Companion","read":{"ubus":{"iwinfo":["devices","info","assoclist"],"network.wireless":["status"],"network.interface":["status"],"network.device":["status"],"luci-rpc":["getHostHints","getDHCPLeases","getWirelessDevices"],"system":["info","board"]}},"write":{"ubus":{"hostapd.*":["del_client"],"network.wireless":["up","down"],"uci":["add","set","delete","commit"],"file":["exec"]},"file":{"/bin/cat":["exec"]}}}}\n` +
    `EOF\n/etc/init.d/rpcd restart`;
  const [copied, setCopied] = useState(false);
  const [fixing, setFixing] = useState(false);
  const [fixErr, setFixErr] = useState<string | null>(null);

  const runFix = async () => {
    setFixing(true);
    setFixErr(null);
    try {
      await invoke<string>("ssh_fix_limited");
      onFixed();
    } catch (e) {
      setFixErr(String(e));
    } finally {
      setFixing(false);
    }
  };

  return (
    <div className="limited-banner">
      <div className="limited-text">
        ⚠️ {t("limited.text1")}<code>iwinfo</code>/<code>hostapd</code>{t("limited.text2")}
      </div>
      <button className="btn btn-primary" disabled={fixing} onClick={runFix}>
        {fixing ? <span className="spinner" /> : t("limited.fix")}
      </button>
      {fixErr && <div className="limited-hint err">{t("limited.err")}: {fixErr}</div>}
      <div className="limited-hint">{t("limited.manual")}</div>
      <div className="limited-cmd" onClick={() => { navigator.clipboard.writeText(cmd); setCopied(true); setTimeout(() => setCopied(false), 1500); }} title={t("limited.copyHint")}>
        {cmd.split("\n").map((l, i) => (
          <div key={i}>{l}</div>
        ))}
      </div>
      <div className="limited-hint">{copied ? t("limited.copied") : t("limited.copyHint")}</div>
    </div>
  );
}

function SignalBars({ dbm }: { dbm: number | null }) {
  const q = signalQuality(dbm);
  const level = q >= 70 ? 4 : q >= 50 ? 3 : q >= 30 ? 2 : q > 0 ? 1 : 0;
  return (
    <div className="sig" title={dbm != null ? `${dbm} dBm` : ""}>
      {[0, 1, 2, 3].map((i) => (
        <span key={i} className={`sig-bar ${i < level ? "lit" : ""}`} style={{ height: 4 + i * 3 }} />
      ))}
    </div>
  );
}

function DeviceCard({
  dev, renaming, speedDown, speedUp, onStartRename, onSaveRename, onKick, onBlock, onSetKind, onPin,
}: {
  dev: Device;
  renaming: string | null;
  speedDown: number | null;
  speedUp: number | null;
  onStartRename: (mac: string) => void;
  onSaveRename: (mac: string, name: string) => void;
  onKick: (mac: string) => void;
  onBlock: (mac: string, block: boolean, wifi?: boolean) => void;
  onSetKind: (mac: string, kind: string) => void;
  onPin: (mac: string) => void;
}) {
  const [draft, setDraft] = useState(dev.alias ?? "");
  const [kindMenu, setKindMenu] = useState(false);
  // close the kind picker when clicking anywhere outside it
  useEffect(() => {
    if (!kindMenu) return;
    const close = () => setKindMenu(false);
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [kindMenu]);
  const isRenaming = renaming === dev.mac;
  const display = dev.alias ?? dev.name;
  const icon = KIND_ICONS[dev.kind] ?? (dev.wifi ? "📶" : "🔌");

  return (
    <div className={`device ${dev.wifi ? "is-wifi" : "is-lan"} ${dev.online ? "is-online" : "is-offline"}`}>
      <div
        className="device-icon"
        title={t("device.changeKind")}
        onMouseDown={(e) => { e.stopPropagation(); setKindMenu((v) => !v); }}
        onClick={(e) => e.stopPropagation()}
      >
        {icon}
        {kindMenu && (
          <div className="kind-menu" onMouseDown={(e) => e.stopPropagation()} onClick={(e) => e.stopPropagation()}>
            {KIND_KEYS.map((k) => (
              <button key={k} className={`kind-opt ${k === dev.kind ? "active" : ""}`} onClick={() => { setKindMenu(false); onSetKind(dev.mac, k); }}>
                {t(`kinds.${k}`)}
              </button>
            ))}
          </div>
        )}
      </div>
      <div className="device-main">
        {isRenaming ? (
          <form
            onSubmit={(e) => { e.preventDefault(); onSaveRename(dev.mac, draft); }}
            className="rename-row"
          >
            <input
              autoFocus
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onBlur={() => onSaveRename(dev.mac, draft)}
              placeholder={t("device.namePh")}
            />
          </form>
        ) : (
          <div className="device-name" title={dev.name}>
            {display}
            {dev.blocked && <span className="badge badge-blocked">{t("device.blockBadge")}</span>}
            {dev.alias && <span className="badge">{dev.name}</span>}
          </div>
        )}
        <div className="device-meta">
          <span>{dev.ip || t("device.noIp")}</span>
          <span className="dot">·</span>
          <span>{dev.wifi ? bandLabel(dev.band) : t("device.lan")}</span>
          {dev.wifi && <SignalBars dbm={dev.signal} />}
          {(speedDown != null || speedUp != null) && (
            <span
              className="dev-speed"
              title={t("device.speedTitle", { down: fmtBits(speedDown), up: fmtBits(speedUp) })}
            >
              <span className="dev-speed-down">↓ {fmtRate(speedDown)}</span>
              <span className="dev-speed-up">↑ {fmtRate(speedUp)}</span>
            </span>
          )}
        </div>
      </div>
      <div className="device-actions">
        <button className={`icon-btn ${dev.pinned ? "pin-on" : ""}`} title={dev.pinned ? t("device.unpin") : t("device.pin")} onClick={() => onPin(dev.mac)}>
          📌
        </button>
        <button className="icon-btn" title={t("device.rename")} onClick={() => onStartRename(dev.mac)}>
          ✎
        </button>
        {dev.wifi && (
          <button className="icon-btn" title={t("device.kick")} onClick={() => onKick(dev.mac)}>
            ⏏
          </button>
        )}
        <button
          className="icon-btn"
          title={dev.blocked ? t("device.unblock") : t("device.block")}
          onClick={() => onBlock(dev.mac, !dev.blocked, dev.wifi)}
        >
          {dev.blocked ? "✓" : "🚫"}
        </button>
      </div>
    </div>
  );
}

function Modal({ title, onClose, children }: { title: string; onClose: () => void; children: React.ReactNode }) {
  return (
    <div className="modal-backdrop" onMouseDown={onClose}>
      <div className="modal" onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <b>{title}</b>
          <button className="icon-btn" onClick={onClose}>✕</button>
        </div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}

type ScanResult = {
  ssid?: string;
  channel?: number;
  signal?: number;
  encryption?: { enabled?: boolean };
  ht_operation?: { channel_width?: number };
};

// overlap depends on both channel widths: a W MHz channel spans +-W/10
// channel slots (20MHz -> +-2, 40MHz -> +-4, 80MHz -> +-8)
function overlapKind(our: number, their: number, ourW: number, theirW: number): "co" | "adj" | null {
  const diff = Math.abs(our - their);
  if (diff === 0) return "co";
  const reach = ourW / 10 + theirW / 10;
  if (diff < reach) return "adj";
  return null;
}

// DFS channels (radar detection): 52-64 and 100-144. Using one requires a
// CAC quiet period (~60 s) before the radio starts, and some clients skip
// them entirely — hence a small penalty in the recommendation score.
function isDfs(ch: number): boolean {
  return (ch >= 52 && ch <= 64) || (ch >= 100 && ch <= 144);
}

const CH_5G = [36, 40, 44, 48, 52, 56, 60, 64, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 149, 153, 157, 161];
const CH_2G = [1, 6, 11, 13];

// pick the least-occupied recommended channel for the band
function bestChannels(band: string, nets: ScanResult[], our: number | null, ourW: number): { ch: number; score: number }[] {
  const candidates = band === "2G" ? CH_2G : CH_5G;
  return candidates
    .map((ch) => {
      // penalty: co-channel with strong net is bad, overlapping is worse than far
      let score = 0;
      for (const n of nets) {
        if (n.channel == null) continue;
        const theirW = n.ht_operation?.channel_width ?? 20;
        const sig = Math.min(0, n.signal ?? -100); // 0..-100
        if (overlapKind(ch, n.channel, ourW, theirW) === "co") score += 30 + Math.abs(sig) / 2;
        else if (overlapKind(ch, n.channel, ourW, theirW) === "adj") score += 12 + Math.abs(sig) / 5;
      }
      if (isDfs(ch)) score += 4; // CAC delay + client compatibility
      if (ch === our) score += 5; // slight inertia against pointless switching
      return { ch, score };
    })
    .sort((a, b) => a.score - b.score);
}

function channelScore(ch: number, nets: ScanResult[], ourW: number): number {
  let score = 0;
  for (const n of nets) {
    if (n.channel == null) continue;
    const theirW = n.ht_operation?.channel_width ?? 20;
    const sig = Math.min(0, n.signal ?? -100);
    if (overlapKind(ch, n.channel, ourW, theirW) === "co") score += 30 + Math.abs(sig) / 2;
    else if (overlapKind(ch, n.channel, ourW, theirW) === "adj") score += 12 + Math.abs(sig) / 5;
  }
  return score;
}

function ScanModal({
  device, radio, ourChannel, ourWidth, band, onClose, showToast,
}: {
  device: string;
  radio: string;
  ourChannel: number | null;
  ourWidth: number;
  band: string;
  onClose: () => void;
  showToast: (m: string) => void;
}) {
  const [applying, setApplying] = useState<number | null>(null);
  const applyChannel = async (ch: number) => {
    setApplying(ch);
    try {
      await invoke("set_channel", { device: radio, channel: ch });
      showToast(
        isDfs(ch)
          ? t("scan.dfsApply", { ch })
          : t("scan.applied", { ch }),
      );
      onClose();
    } catch (e) {
      showToast(t("scan.chErr", { e: String(e) }));
    } finally {
      setApplying(null);
    }
  };
  const [nets, setNets] = useState<ScanResult[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    invoke<unknown>("wifi_scan", { device })
      .then((raw) => {
        const v = raw as { results?: ScanResult[] } | ScanResult[];
        const arr = Array.isArray(v) ? v : (v.results ?? []);
        arr.sort((a: ScanResult, b: ScanResult) => (b.signal ?? -999) - (a.signal ?? -999));
        setNets(arr);
      })
      .catch((e) => { setErr(String(e)); showToast(t("scan.scanErr", { e: String(e) })); });
  }, [device, showToast]);
  const coChannel = nets?.filter((n) => ourChannel != null && n.channel === ourChannel).length ?? 0;
  const adjacent =
    nets?.filter(
      (n) => ourChannel != null && n.channel != null && overlapKind(ourChannel, n.channel, ourWidth, n.ht_operation?.channel_width ?? 20) === "adj",
    ).length ?? 0;
  const best = nets ? bestChannels(band, nets, ourChannel, ourWidth) : [];
  // alternatives must beat the current channel's score to be suggested
  const better = ourChannel != null && nets
    ? best.filter((b) => b.ch !== ourChannel && b.score < channelScore(ourChannel, nets, ourWidth) - 1)
    : [];
  const verdict = ourChannel == null ? null : coChannel > 0
    ? t("scan.verdictBusy", { ch: ourChannel, w: ourWidth, n: coChannel })
    : adjacent > 0
      ? t("scan.verdictAdj", { ch: ourChannel, w: ourWidth, n: adjacent })
      : t("scan.verdictClean", { ch: ourChannel, w: ourWidth });
  return (
    <Modal title={t("scan.title", { dev: device })} onClose={onClose}>
      {verdict && (
        <div className={`scan-verdict ${coChannel > 0 ? "err" : adjacent > 0 ? "warn" : "ok"}`}>
          {verdict}
          {coChannel + adjacent > 0 && (
            better.length > 0
              ? <>{t("scan.freeChannels")}<b>{better.slice(0, 2).map((b) => b.ch).join(" / ")}</b></>
              : <>{t("scan.noBetter")}</>
          )}
        </div>
      )}
      {verdict && (
        <div className="channel-actions">
          {better.length > 0 && (
            <button className="btn btn-primary" disabled={applying != null} onClick={() => applyChannel(better[0].ch)}>
              {applying === better[0].ch ? <span className="spinner" /> : t("scan.switchTo", { ch: better[0].ch })}
            </button>
          )}
          <select
            className="channel-select"
            value={ourChannel ?? ""}
            disabled={applying != null}
            onChange={(e) => {
              const ch = Number(e.target.value);
              if (ch !== ourChannel) applyChannel(ch);
            }}
          >
            {(band === "2G" ? [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13] : CH_5G).map((ch) => (
              <option key={ch} value={ch}>
                {ch}{isDfs(ch) ? " (DFS)" : ""}{ourChannel === ch ? t("scan.current") : ""}
              </option>
            ))}
          </select>
          {band !== "2G" && (
            <span className="hint" title={t("scan.dfsHint")}>
              {t("scan.dfsShort")}
            </span>
          )}
        </div>
      )}
      {err && <div className="status err">{t("scan.aclErr", { e: err })}</div>}
      {!err && !nets && <div className="loading"><span className="spinner" /> {t("scan.busy")}</div>}
      {nets && (
        <table className="scan-table">
          <thead><tr><th>{t("scan.ssid")}</th><th>{t("scan.channel")}</th><th>{t("scan.signal")}</th><th>{t("scan.enc")}</th></tr></thead>
          <tbody>
            {nets.map((n, i) => {
              const ov = ourChannel != null && n.channel != null ? overlapKind(ourChannel, n.channel, ourWidth, n.ht_operation?.channel_width ?? 20) : null;
              return (
                <tr key={i} className={ov === "co" ? "row-co" : ov === "adj" ? "row-adj" : ""}>
                  <td>{ov === "co" ? "🔴 " : ov === "adj" ? "🟡 " : ""}{n.ssid || t("scan.hidden")}</td>
                  <td>{n.channel != null ? `${n.channel}${n.channel === ourChannel ? t("scan.ours") : ""}${(n.ht_operation?.channel_width ?? 20) !== 20 ? ` · ${n.ht_operation?.channel_width}М` : ""}` : "—"}</td>
                  <td>{n.signal != null ? `${n.signal} dBm` : "—"}</td>
                  <td>{n.encryption?.enabled ? "🔒" : t("scan.open")}</td>
                </tr>
              );
            })}
            {!nets.length && <tr><td colSpan={4}>{t("scan.none")}</td></tr>}
          </tbody>
        </table>
      )}
    </Modal>
  );
}

const DIAG_PRESETS: [string, string, string][] = [
  ["ping_gw", "diag.pingGw", "diag.pingGwHint"],
  ["ping_dns", "diag.pingDns", "diag.pingDnsHint"],
  ["log", "diag.log", "diag.logHint"],
  ["mem", "diag.mem", "diag.memHint"],
];

function DiagModal({ onClose, showToast }: { onClose: () => void; showToast: (m: string) => void }) {
  const [out, setOut] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const run = async (kind: string) => {
    setBusy(kind);
    setOut(null);
    try {
      setOut(await invoke<string>("run_diag", { kind }));
    } catch (e) {
      const msg = t("act.err", { e: String(e) });
      setOut(msg);
      showToast(msg);
    } finally {
      setBusy(null);
    }
  };
  return (
    <Modal title={t("diag.title")} onClose={onClose}>
      <div className="diag-row">
        {DIAG_PRESETS.map(([k, label, hint]) => (
          <button key={k} className="btn btn-ghost" disabled={busy != null} title={t(hint)} onClick={() => run(k)}>
            {busy === k ? <span className="spinner" /> : t(label)}
          </button>
        ))}
      </div>
      {out && <pre className="diag-out">{out}</pre>}
    </Modal>
  );
}

function SettingsView({
  onSaved, onClose, lang, theme, onLang, onTheme,
}: {
  onSaved: () => void;
  onClose: () => void;
  lang: Lang;
  theme: string;
  onLang: (l: Lang) => void;
  onTheme: (th: string) => void;
}) {
  const [s, setS] = useState<Settings | null>(null);
  const [password, setPassword] = useState("");
  const [status, setStatus] = useState<{ ok: boolean; msg: string } | null>(null);
  const [testing, setTesting] = useState(false);
  const [autoStart, setAutoStart] = useState(false);
  const [profileName, setProfileName] = useState("");
  const [notify, setNotify] = useState(true);
  const [version, setVersion] = useState("");
  const reloadSettings = useCallback(() => {
    invoke<Settings>("get_settings").then((v) => {
      setS(v);
      const p = v.profiles?.[v.activeProfile];
      setProfileName(p?.name ?? "");
      setNotify(v.notify !== false);
    });
  }, []);

  useEffect(() => {
    reloadSettings();
    autostartIsEnabled().then(setAutoStart).catch(() => {});
    getVersion().then(setVersion).catch(() => {});
  }, [reloadSettings]);

  const switchProfile = async (idx: number) => {
    try {
      await invoke("switch_profile", { index: idx });
      reloadSettings();
      onSaved();
    } catch (e) {
      setStatus({ ok: false, msg: t("set.errPrefix", { ctx: t("set.routers"), e: String(e) }) });
    }
  };
  const addProfile = async () => {
    try {
      await invoke("add_profile", { name: "" });
      reloadSettings();
    } catch (e) {
      setStatus({ ok: false, msg: t("set.errPrefix", { ctx: t("set.routers"), e: String(e) }) });
    }
  };
  const deleteProfile = async () => {
    try {
      await invoke("delete_profile", { index: s!.activeProfile });
      reloadSettings();
    } catch (e) {
      setStatus({ ok: false, msg: t("set.errPrefix", { ctx: t("set.routers"), e: String(e) }) });
    }
  };

  const toggleAutoStart = async () => {
    try {
      if (autoStart) {
        await autostartDisable();
        setAutoStart(false);
      } else {
        await autostartEnable();
        setAutoStart(true);
      }
    } catch (e) {
      setStatus({ ok: false, msg: t("set.errPrefix", { ctx: t("set.autostart"), e: String(e) }) });
    }
  };

  if (!s) return <div className="loading">…</div>;

  const save = async (testOnly: boolean) => {
    setTesting(true);
    setStatus(null);
    try {
      await invoke("save_settings", {
        host: s.host,
        username: s.username,
        password,
        pollIntervalSecs: s.pollIntervalSecs,
        statsRows: s.statsRows ?? 2,
        profileName,
        notify,
      });
      const model = await invoke<string>("test_connection");
      setStatus({ ok: true, msg: t("set.connected", { model }) });
      if (!testOnly) {
        onSaved();
        onClose();
      }
    } catch (e) {
      setStatus({ ok: false, msg: String(e) });
    } finally {
      setTesting(false);
    }
  };

  return (
    <div className="settings">
      <h3>{t("set.routers")}</h3>
      <div className="profile-row">
        <select
          value={s.activeProfile}
          onChange={(e) => switchProfile(Number(e.target.value))}
        >
          {s.profiles?.map((p, i) => (
            <option key={i} value={i}>{p.name || p.host || t("set.routerN", { n: i + 1 })}</option>
          ))}
        </select>
        <button className="icon-btn" title={t("set.addRouter")} onClick={addProfile}>＋</button>
        <button className="icon-btn" title={t("set.delRouter")} disabled={(s.profiles?.length ?? 0) <= 1} onClick={deleteProfile}>🗑</button>
      </div>
      <label>
        {t("set.profileName")}
        <input
          value={profileName}
          onChange={(e) => setProfileName(e.target.value)}
          placeholder={t("set.profilePh")}
        />
      </label>
      <h3>{t("set.connection")}</h3>
      <label>
        {t("set.host")}
        <input
          value={s.host}
          onChange={(e) => setS({ ...s, host: e.target.value })}
          placeholder="192.168.1.1"
        />
      </label>
      <label>
        {t("set.login")}
        <input value={s.username} onChange={(e) => setS({ ...s, username: e.target.value })} />
      </label>
      <label>
        {t("set.password")}{" "}
        {password === "" && s.hasPassword && <span className="hint ok-hint">{t("set.pwSaved")}</span>}
        {password === "" && !s.hasPassword && <span className="hint">{t("set.pwNone")}</span>}
        <input
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder="••••••••"
        />
      </label>
      <label>
        {t("set.sshKey")}
        <div className="range-row">
          <button
            type="button"
            className="btn btn-ghost"
            title={t("set.forgetKeyTitle")}
            onClick={async () => {
              try {
                await invoke("ssh_forget_host_key");
                setStatus({ ok: true, msg: t("set.keyForgotten") });
              } catch (e) {
                setStatus({ ok: false, msg: t("set.errPrefix", { ctx: "SSH", e: String(e) }) });
              }
            }}
          >
            {t("set.forgetKey")}
          </button>
        </div>
      </label>
      <label>
        {t("set.poll")}
        <div className="range-row">
          <input
            type="range"
            min={2}
            max={30}
            value={s.pollIntervalSecs}
            onChange={(e) => setS({ ...s, pollIntervalSecs: Number(e.target.value) })}
          />
          <span>{s.pollIntervalSecs} {t("set.sec")}</span>
        </div>
      </label>
      <label>
        {t("set.tiles")}
        <div className="range-row">
          <div className="seg-row">
            {[1, 2, 3].map((n) => (
              <button
                key={n}
                type="button"
                className={`seg ${((s.statsRows ?? 2) === n) ? "active" : ""}`}
                onClick={() => setS({ ...s, statsRows: n })}
              >
                {n}
              </button>
            ))}
          </div>
        </div>
      </label>
      <label>
        {t("set.language")}
        <div className="range-row">
          <select
            className="channel-select"
            value={lang}
            onChange={(e) => onLang(e.target.value as Lang)}
          >
            <option value="ru">Русский</option>
            <option value="en">English</option>
          </select>
        </div>
      </label>
      <label>
        {t("set.theme")}
        <div className="range-row">
          <button type="button" className={`btn ${theme === "dark" ? "btn-primary" : "btn-ghost"}`} onClick={() => onTheme("dark")}>
            {t("set.themeDark")}
          </button>
          <button type="button" className={`btn ${theme === "light" ? "btn-primary" : "btn-ghost"}`} onClick={() => onTheme("light")}>
            {t("set.themeLight")}
          </button>
        </div>
      </label>
      <label>
        {t("set.notify")}
        <div className="range-row">
          <button type="button" className={`btn ${notify ? "btn-primary" : "btn-ghost"}`} onClick={() => setNotify((v) => !v)}>
            {notify ? t("set.notifyOn") : t("set.notifyOff")}
          </button>
        </div>
      </label>
      <label>
        {t("set.autostart")}
        <div className="range-row">
          <button type="button" className={`btn ${autoStart ? "btn-primary" : "btn-ghost"}`} onClick={toggleAutoStart}>
            {autoStart ? t("set.autostartOn") : t("set.autostartOff")}
          </button>
        </div>
      </label>
      {status && (
        <div className={`status ${status.ok ? "ok" : "err"}`}>{status.msg}</div>
      )}
      <div className="settings-actions">
        <button className="btn btn-ghost" disabled={testing} onClick={() => save(true)}>
          {testing ? <span className="spinner" /> : t("set.test")}
        </button>
        <button className="btn btn-primary" disabled={testing} onClick={() => save(false)}>
          {t("set.save")}
        </button>
      </div>
      <p className="settings-note">
        {t("set.note1")}<code>/ubus</code>{t("set.note2")}
      </p>
      <div className="about-box">
        <div className="about-row"><span>{t("set.version")}</span><b>OpenWrt Companion{version ? ` v${version}` : ""}</b></div>
        <div className="about-row"><span>{t("set.license")}</span><b>{t("set.licenseVal")}</b></div>
        <div className="about-row">
          <span>{t("set.project")}</span>
          <a
            href={REPO_URL}
            target="_blank"
            rel="noreferrer"
            onClick={(e) => { e.preventDefault(); invoke("open_external", { url: REPO_URL }); }}
          >
            github.com/bigwolfys79/openwrt-companion
          </a>
        </div>
      </div>
    </div>
  );
}

/* ---------- app ---------- */

export default function App() {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [busyRadio, setBusyRadio] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [scanDevice, setScanDevice] = useState<string | null>(null);
  const [scanRadio, setScanRadio] = useState("");
  const [scanChannel, setScanChannel] = useState<number | null>(null);
  const [scanWidth, setScanWidth] = useState(20);
  const [scanBand, setScanBand] = useState("2G");
  const [diagOpen, setDiagOpen] = useState(false);
  const prevSnap = useRef<Snapshot | null>(null);
  const [filter, setFilter] = useState<"online" | "wifi" | "lan" | "archive" | "blocked">("online");
  const [pollSecs, setPollSecs] = useState(5);
  const [statsRows, setStatsRows] = useState(2);
  const [, setUiTick] = useState(0); // forces re-render when language changes

  useEffect(() => {
    invoke<Settings>("get_settings")
      .then((s) => {
        setPollSecs(s.pollIntervalSecs);
        setStatsRows(s.statsRows ?? 2);
        notifyRef.current = s.notify !== false;
        setLang(normalizeLang(s.language));
        document.documentElement.dataset.theme = s.theme === "light" ? "light" : "dark";
        setUiTick((n) => n + 1);
      })
      .catch(() => {});
  }, []);

  const showToast = useCallback((msg: string) => {
    setToast(msg);
    setTimeout(() => setToast(null), 2600);
  }, []);

  /* --- language & theme, saved immediately (no "Save" needed) --- */
  const changeLang = (l: Lang) => {
    setLang(l);
    setUiTick((n) => n + 1);
    invoke("set_ui_prefs", { language: l, theme: document.documentElement.dataset.theme ?? "dark" }).catch(() => {});
  };
  const changeTheme = (th: string) => {
    document.documentElement.dataset.theme = th === "light" ? "light" : "dark";
    invoke("set_ui_prefs", { language: getLang(), theme: th === "light" ? "light" : "dark" }).catch(() => {});
  };

  /* --- desktop notifications on device transitions --- */
  const notifyRef = useRef(true);
  const notifKnown = useRef<Map<string, { online: boolean; name: string }> | null>(null);
  const offlineNotified = useRef(false);
  const notifySend = useCallback(async (title: string, body: string) => {
    try {
      let granted = await isPermissionGranted();
      if (!granted) granted = (await requestPermission()) === "granted";
      if (granted) sendNotification({ title, body });
    } catch { /* notifications unavailable */ }
  }, []);
  const diffNotify = useCallback((snap: Snapshot) => {
    if (!notifyRef.current) return;
    if (snap.status !== "ok") {
      if (!offlineNotified.current) {
        offlineNotified.current = true;
        notifySend(t("ntf.routerDown"), snap.error ?? t("main.noConn"));
      }
      return;
    }
    offlineNotified.current = false;
    const cur = new Map(snap.devices.map((d) => [d.mac, { online: d.online, name: d.alias ?? d.name }]));
    const known = notifKnown.current;
    if (known) {
      for (const [mac, c] of cur) {
        const was = known.get(mac);
        if (!was || (!was.online && c.online)) {
          if (c.online) notifySend(t("ntf.devOnline"), c.name);
        } else if (was.online && !c.online) {
          notifySend(t("ntf.devOffline"), c.name);
        }
      }
    }
    notifKnown.current = cur;
  }, [notifySend]);

  useEffect(() => {
    invoke<Snapshot>("get_snapshot").then((s) => { setSnap(s); diffNotify(s); }).catch(() => {});
    const un = listen<Snapshot>("snapshot", (e) => {
      prevSnap.current = snapRef.current;
      setSnap(e.payload);
      diffNotify(e.payload);
    });
    return () => { un.then((f) => f()); };
  }, [diffNotify]);

  // keep a ref mirror so event handler can diff
  const snapRef = useRef<Snapshot | null>(null);
  useEffect(() => { snapRef.current = snap; }, [snap]);

  const doRebootRadio = async (device: string) => {
    setBusyRadio(device);
    try {
      await invoke("reboot_radio", { device });
      showToast(t("act.radioRebooting"));
    } catch (e) {
      showToast(t("act.err", { e: String(e) }));
    } finally {
      setBusyRadio(null);
    }
  };

  const doPin = async (mac: string) => {
    try {
      await invoke("toggle_pin", { mac });
      setSnap(await invoke<Snapshot>("get_snapshot"));
    } catch (e) {
      showToast(t("act.err", { e: String(e) }));
    }
  };

  const doBackup = async () => {
    showToast(t("act.backingUp"));
    try {
      const p = await invoke<string>("backup_config");
      showToast(t("act.backupDone", { p }));
    } catch (e) {
      showToast(t("act.backupErr", { e: String(e) }));
    }
  };

  const doSetKind = async (mac: string, kind: string) => {
    try {
      await invoke("set_kind", { mac, kind });
      const s = await invoke<Snapshot>("get_snapshot");
      setSnap(s);
    } catch (e) {
      showToast(t("act.err", { e: String(e) }));
    }
  };

  const doRename = async (mac: string, name: string) => {
    setRenaming(null);
    try {
      await invoke("set_alias", { mac, name });
      const s = await invoke<Snapshot>("get_snapshot");
      setSnap(s);
    } catch (e) {
      showToast(t("act.err", { e: String(e) }));
    }
  };

  const doKick = async (mac: string) => {
    try {
      await invoke("kick_client", { mac });
      showToast(t("act.kicked"));
    } catch (e) {
      showToast(t("act.err", { e: String(e) }));
    }
  };

  const doBlock = async (mac: string, block: boolean, wifi?: boolean) => {
    try {
      await invoke("block_device", { mac, block });
      if (block && wifi) {
        // firewall block alone leaves the client associated; deauth it too
        try {
          await invoke("kick_client", { mac });
        } catch { /* may already be gone / LAN device */ }
      }
      showToast(block ? t("act.blocked") : t("act.unblocked"));
      const s = await invoke<Snapshot>("get_snapshot");
      setSnap(s);
    } catch (e) {
      showToast(t("act.err", { e: String(e) }));
    }
  };

  const devices = useMemo(() => {
    if (!snap) return [];
    return snap.devices.filter((d) =>
      filter === "online" ? d.online
      : filter === "wifi" ? d.online && d.wifi
      : filter === "lan" ? d.online && !d.wifi
      : filter === "blocked" ? d.blocked
      : !d.online || d.blocked,
    );
  }, [snap, filter]);

  const onlineCount = snap?.devices.filter((d) => d.online).length ?? 0;
  const wifiCount = snap?.devices.filter((d) => d.online && d.wifi).length ?? 0;
  const lanCount = snap?.devices.filter((d) => d.online && !d.wifi).length ?? 0;
  const archiveCount = snap ? snap.devices.length - onlineCount : 0;
  const blockedCount = snap?.devices.filter((d) => d.blocked).length ?? 0;
  const offline = snap?.status === "offline";

  return (
    <div className="shell">
      <TitleBar onSettings={() => setSettingsOpen((v) => !v)} settingsOpen={settingsOpen} />

      {settingsOpen ? (
        <SettingsView
          lang={getLang()}
          theme={document.documentElement.dataset.theme ?? "dark"}
          onLang={changeLang}
          onTheme={changeTheme}
          onSaved={async () => {
            try {
              const s = await invoke<Settings>("get_settings");
              setPollSecs(s.pollIntervalSecs);
              setStatsRows(s.statsRows ?? 2);
            } catch { /* keep current */ }
          }}
          onClose={() => setSettingsOpen(false)}
        />
      ) : (
        <div className="content">
          {offline && (
            <div className="offline">
              <div className="offline-icon">📡</div>
              <b>{t("main.offline")}</b>
              <span>{snap?.error ?? t("main.noConn")}</span>
              <button className="btn btn-ghost" onClick={() => invoke("get_snapshot")}>
                {t("main.retry")}
              </button>
            </div>
          )}

          {snap && !offline && (
            <>
              {snap.limited && (
                <LimitedBanner
                  onFixed={async () => {
                    showToast(t("limited.applied"));
                    await new Promise((r) => setTimeout(r, 2000));
                    const s = await invoke<Snapshot>("get_snapshot");
                    setSnap(s);
                  }}
                />
              )}

              <StatsBar snap={snap} prev={prevSnap} intervalSecs={pollSecs} rows={statsRows} />

              <RadioControls radios={snap.radios} busy={busyRadio} onReboot={doRebootRadio} onScan={(dev, radio, ch, w, band) => { setScanDevice(dev); setScanRadio(radio); setScanChannel(ch); setScanWidth(w); setScanBand(band); }} />

              <div className="toolbar">
                <div className="toolbar-btns">
                  <button className="icon-btn" title={t("act.diag")} onClick={() => setDiagOpen(true)}>🩺</button>
                  <button className="icon-btn" title={t("act.backup")} onClick={doBackup}>💾</button>
                  <button
                    className="icon-btn"
                    title={t("act.reboot")}
                    onClick={async () => {
                      try {
                        await invoke("reboot_router");
                        showToast(t("act.rebooting"));
                      } catch (e) {
                        showToast(t("act.err", { e: String(e) }));
                      }
                    }}
                  >
                    ⏻
                  </button>
                </div>
                <div className="tabs">
                  {(["online", "wifi", "lan", "blocked", "archive"] as const).map((key) => {
                    const n = key === "online" ? onlineCount : key === "wifi" ? wifiCount : key === "lan" ? lanCount : key === "blocked" ? blockedCount : archiveCount;
                    return (
                      <button
                        key={key}
                        className={`tab ${filter === key ? "active" : ""}`}
                        onClick={() => setFilter(key)}
                      >
                        {t(`tabs.${key}`, { n })}
                      </button>
                    );
                  })}
                </div>
              </div>

              <div className="device-list">
                {devices.map((d) => {
                  // per-device speed: diff cumulative station byte counters
                  const prevDev = prevSnap.current?.devices.find((p) => p.mac === d.mac);
                  const dt = snap.ts && prevSnap.current?.ts && snap.ts > prevSnap.current.ts
                    ? (snap.ts - prevSnap.current.ts) / 1000
                    : pollSecs;
                  const rate = (cur: number | null, old: number | null | undefined): number | null =>
                    cur != null && old != null && cur >= old ? (cur - old) / Math.max(1, dt) : null;
                  // AP tx -> client download; AP rx -> client upload
                  const speedDown = rate(d.txBytes, prevDev?.txBytes);
                  const speedUp = rate(d.rxBytes, prevDev?.rxBytes);
                  return (
                    <DeviceCard
                      key={d.mac}
                      dev={d}
                      renaming={renaming}
                      speedDown={speedDown}
                      speedUp={speedUp}
                      onStartRename={setRenaming}
                      onSaveRename={doRename}
                      onKick={doKick}
                      onBlock={doBlock}
                      onSetKind={doSetKind}
                      onPin={doPin}
                    />
                  );
                })}
                {!devices.length && <div className="empty">{t("main.noDevices")}</div>}
              </div>
            </>
          )}
        </div>
      )}

      {toast && <div className="toast">{toast}</div>}

      {scanDevice && (
        <ScanModal
          device={scanDevice}
          radio={scanRadio}
          ourChannel={scanChannel}
          ourWidth={scanWidth}
          band={scanBand}
          onClose={() => setScanDevice(null)}
          showToast={showToast}
        />
      )}
      {diagOpen && <DiagModal onClose={() => setDiagOpen(false)} showToast={showToast} />}
    </div>
  );
}
