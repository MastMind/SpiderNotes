import { invoke } from "@tauri-apps/api/core";
import type {
  DeviceInfo,
  Note,
  Backlink,
  NoteSummary,
  OpenResult,
  PeerInfo,
  P2pSettings,
  P2pSyncReport,
  TagCount,
  TrustedDevice,
} from "./types";

export const openVault = (path: string, p2pPort?: number) =>
  invoke<OpenResult>("open_vault", { path, p2pPort: p2pPort ?? null });
export const defaultVaultPath = () => invoke<string>("default_vault_path");
export const vaultStatus = () => invoke<OpenResult | null>("vault_status");
export const listNotes = () => invoke<NoteSummary[]>("list_notes");
export const readNote = (uuid: string) => invoke<Note>("read_note", { uuid });
export const saveNote = (path: string, content: string) =>
  invoke<NoteSummary>("save_note", { path, content });
export const deleteNote = (uuid: string) => invoke<NoteSummary>("delete_note", { uuid });
export const renameNote = (uuid: string, newPath: string) =>
  invoke<NoteSummary>("rename_note", { uuid, newPath });
export const searchNotes = (query: string) =>
  invoke<NoteSummary[]>("search_notes", { query });
export const backlinks = (uuid: string) => invoke<Backlink[]>("backlinks", { uuid });
export const allTags = () => invoke<TagCount[]>("all_tags");
export const notesWithTag = (tag: string) =>
  invoke<NoteSummary[]>("notes_with_tag", { tag });
export const deviceInfo = () => invoke<DeviceInfo>("device_info");
export const listPeers = () => invoke<PeerInfo[]>("list_peers");
export const listTrusted = () => invoke<TrustedDevice[]>("list_trusted");
// Tauri v2 maps command args by camelCase keys (device_id -> deviceId).
export const trustDevice = (device_id: string, name: string) =>
  invoke<TrustedDevice[]>("trust_device", { deviceId: device_id, name });
export const untrustDevice = (device_id: string) =>
  invoke<TrustedDevice[]>("untrust_device", { deviceId: device_id });
export const syncWith = (peer: PeerInfo) => invoke<P2pSyncReport>("sync_with", { peer });
export const syncAll = () => invoke<P2pSyncReport[]>("sync_all");
export const p2pSettings = () => invoke<P2pSettings>("p2p_settings");
export const setBgSync = (enabled: boolean) => invoke<void>("set_bg_sync", { enabled });
export const setAutosyncSecs = (secs: number) =>
  invoke<number>("set_autosync_secs", { secs });
export const setP2pPortSetting = (port: number) =>
  invoke<number>("set_p2p_port_setting", { port });
export const restartP2p = () => invoke<number>("restart_p2p");
export const mdnsRefresh = () => invoke<void>("mdns_refresh");
