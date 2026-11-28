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
  /** True when the body on disk is ciphertext: the title comes from the
   * frontmatter, the preview is empty, and opening/moving/deleting the note
   * requires its password. */
  encrypted?: boolean;
  /** Media files the note references that exist in this vault (path + hash).
   * Sync uses it to know which .assets/ bytes a peer is still missing. */
  assets?: { path: string; hash: string }[];
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
  /** Media files stored on this side from the peer (0 when none). */
  assets?: number;
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
  /**
   * Local media-server port (127.0.0.1), 0 when it is not running.
   * <video>/<audio> resolve to http://127.0.0.1:<port>/<rel> because
   * WebKit's media pipeline cannot play large files from asset:/blob:.
   */
  media_port: number;
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
  /** Download media files from peers (notes sync regardless). */
  download_media: boolean;
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

/** One media file under the vault's hidden .assets/ dir (vault-relative). */
export interface AssetFile {
  path: string;
  size: number;
}

/** Orphan-asset scan result (the "Storage" section of settings). */
export interface AssetReport {
  files: AssetFile[];
  /** Paths (subset of files) referenced by no readable note / conflict. */
  orphans: string[];
  /** Encrypted notes whose content could not be read (no key held). */
  encrypted_unchecked: number;
  total_bytes: number;
  orphan_bytes: number;
}

export interface Backlink {
  note: NoteSummary;
  count: number;
}

export interface TagCount {
  tag: string;
  count: number;
}
