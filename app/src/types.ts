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
  /** List-row title: first `# heading`, else first non-empty line, else ""
   * (the row falls back to the file name). */
  title: string;
  /** List-row preview: first line after the title ("" when none). */
  preview: string;
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
  /** Unix seconds of the last successful sync (null = never). */
  last_sync: number | null;
}

export interface TrustedDevice {
  device_id: string;
  name: string;
  added_at_ms: number;
}

/** A sync conflict: the peer's version of a note that could not be
 * auto-merged. Our version stays live in the note file; the peer's version
 * is kept (content via conflict_content) until it is resolved. */
export interface Conflict {
  id: string;
  note_uuid: string;
  /** Note path when the conflict was recorded (display only). */
  note_path: string;
  /** Friendly name of the device that sent the conflicting version. */
  peer_name: string;
  /** Short device id of the conflicting version (from its HLC). */
  peer_id: string;
  created_ms: number;
  their_hlc: Hlc;
  /** Title/preview of the peer's version, for the row. */
  their_title: string;
  their_preview: string;
}

export interface Backlink {
  note: NoteSummary;
  count: number;
}

export interface TagCount {
  tag: string;
  count: number;
}
