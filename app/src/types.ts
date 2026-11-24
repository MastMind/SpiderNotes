export interface Hlc {
  ms: number;
  counter: number;
  device: string;
}

export interface NoteSummary {
  uuid: string;
  path: string;
  hlc: Hlc;
  hash: string;
  deleted: boolean;
}

export interface Note {
  uuid: string;
  path: string;
  content: string;
  hlc: Hlc;
  deleted: boolean;
}

export interface PeerInfo {
  name: string;
  device_id: string;
  x25519_pub: number[];
  address: string;
  port: number;
  /** Discovered via the relay server (no direct mDNS address). */
  via_server?: boolean;
}

export interface ServerSyncStatus {
  enabled: boolean;
  connected: boolean;
  host: string;
  port: number;
  room: string;
  peer_count: number;
  status: string;
}

export interface P2pSyncReport {
  peer_name: string;
  sent: number;
  applied: number;
  skipped: number;
}

export interface OpenResult {
  path: string;
  p2p_port: number;
  device_id: string;
  device_name: string;
  notes: NoteSummary[];
  peers: PeerInfo[];
  /** Vault folders (relative slash-separated paths), including empty ones. */
  folders: string[];
}

export interface DeviceInfo {
  id: string;
  name: string;
}

export interface P2pSettings {
  autosync_secs: number;
  p2p_port: number;
  p2p_port_setting: number;
  p2p_running: boolean;
}

export interface TrustedDevice {
  device_id: string;
  name: string;
  added_at_ms: number;
}

export interface Backlink {
  note: NoteSummary;
  count: number;
}

export interface TagCount {
  tag: string;
  count: number;
}
