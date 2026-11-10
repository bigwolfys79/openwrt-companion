export interface Device {
  mac: string;
  name: string;
  ip: string;
  signal: number | null;
  wifi: boolean;
  online: boolean;
  iface: string | null;
  radio: string | null;
  band: string | null;
  kind: string;
  alias: string | null;
  blocked: boolean;
  pinned: boolean;
  rxBytes: number | null;
  txBytes: number | null;
}

export interface NetInfo {
  ifname: string;
  ssid: string | null;
}

export interface Radio {
  device: string;
  band: string;
  channel: number | null;
  width: number | null;
  up: boolean;
  networks: NetInfo[];
}

export interface Stats {
  uptime: number | null;
  load: [number, number, number] | null;
  wanIp: string | null;
  rxBytes: number | null;
  txBytes: number | null;
  memTotal: number | null;
  memFree: number | null;
  temp: number | null;
  wanIface: string | null;
  wanCounters: { iface: string; rxBytes: number; txBytes: number }[] | null;
  offload: string | null;
}

export interface Snapshot {
  status: "ok" | "offline";
  ts: number;
  error: string | null;
  limited: boolean;
  devices: Device[];
  radios: Radio[];
  stats: Stats;
  blockedMacs: string[];
}

export interface RouterProfile {
  name: string;
  host: string;
  username: string;
  passwordB64: string;
  hasPassword: boolean;
}

export interface Settings {
  host: string;
  username: string;
  passwordB64: string;
  hasPassword: boolean;
  pollIntervalSecs: number;
  statsRows: number;
  aliases: Record<string, string>;
  notify: boolean;
  pinned: string[];
  profiles: RouterProfile[];
  activeProfile: number;
  language: string;
  theme: string;
}
