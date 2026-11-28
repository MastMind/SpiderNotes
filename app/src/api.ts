import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type {
  AssetReport,
  Conflict,
  DeviceInfo,
  Note,
  Backlink,
  NoteSummary,
  OpenResult,
  PeerInfo,
  P2pSettings,
  P2pSyncReport,
  ServerSyncStatus,
  TagCount,
  TrustedDevice,
} from "./types";

export const openVault = (path: string, p2pPort?: number) =>
  invoke<OpenResult>("open_vault", { path, p2pPort: p2pPort ?? null });
/** Native folder picker (Tauri dialog plugin); null when cancelled. */
export const pickDirectory = (): Promise<string | null> =>
  openDialog({ directory: true, multiple: false });
export const defaultVaultPath = () => invoke<string>("default_vault_path");
export const vaultStatus = () => invoke<OpenResult | null>("vault_status");
export const listNotes = () => invoke<NoteSummary[]>("list_notes");
export const readNote = (uuid: string) => invoke<Note>("read_note", { uuid });
export const saveNote = (path: string, content: string) =>
  invoke<NoteSummary>("save_note", { path, content });
export const deleteNote = (uuid: string) => invoke<NoteSummary>("delete_note", { uuid });
export const renameNote = (uuid: string, newPath: string, newTitle?: string) =>
  invoke<NoteSummary>("rename_note", {
    uuid,
    newPath,
    newTitle: newTitle ?? null,
  });
/** Store a dropped media file in the vault's hidden .assets/ dir; returns
 *  its relative path (what goes into the ![alt](src) markdown). `b64` is
 *  the base64 of the file bytes (a JSON number array would be 5-6x larger). */
export const importMediaFile = (name: string, b64: string) =>
  invoke<string>("import_media_file", { name, b64 });
/** Orphan-asset scan (settings "Storage"); run after autosave. */
export const assetReport = () => invoke<AssetReport>("asset_report");
/** Delete asset files (guarded to .assets/ in Rust); returns freed bytes. */
export const deleteAssets = (paths: string[]) =>
  invoke<number>("delete_assets", { paths });
 // Encrypted notes: the password is verified in Rust (a wrong one fails the
 // GCM tag); the key is held in app memory only while the note is unlocked.
 export const createEncryptedNote = (path: string, password: string) =>
  invoke<NoteSummary>("create_encrypted_note", { path, password });
 /** Returns the plaintext for the editor; rejects on a wrong password. */
 export const unlockNote = (uuid: string, password: string) =>
  invoke<string>("unlock_note", { uuid, password });
 export const lockNote = (uuid: string) => invoke<void>("lock_note", { uuid });
export const listFolders = () => invoke<string[]>("list_folders");
export const createFolder = (path: string) => invoke<string>("create_folder", { path });
export const renameFolder = (oldPath: string, newPath: string) =>
  invoke<number>("rename_folder", { oldPath, newPath });
export const deleteFolder = (path: string) =>
  invoke<NoteSummary[]>("delete_folder", { path });
export const searchNotes = (query: string) =>
  invoke<NoteSummary[]>("search_notes", { query });
export const backlinks = (uuid: string) => invoke<Backlink[]>("backlinks", { uuid });
export const allTags = () => invoke<TagCount[]>("all_tags");
export const notesWithTag = (tag: string) =>
  invoke<NoteSummary[]>("notes_with_tag", { tag });
export const deviceInfo = () => invoke<DeviceInfo>("device_info");
export const setDeviceName = (name: string) =>
  invoke<DeviceInfo>("set_device_name", { name });
export const listPeers = () => invoke<PeerInfo[]>("list_peers");
export const listTrusted = () => invoke<TrustedDevice[]>("list_trusted");
// Tauri v2 maps command args by camelCase keys (device_id -> deviceId).
export const trustDevice = (device_id: string, name: string) =>
  invoke<TrustedDevice[]>("trust_device", { deviceId: device_id, name });
export const untrustDevice = (device_id: string) =>
  invoke<TrustedDevice[]>("untrust_device", { deviceId: device_id });
// Local display-name override for a peer (this device only). The peer list
// refreshes through the peers-updated event the command emits.
export const renamePeer = (device_id: string, name: string) =>
  invoke<void>("rename_peer", { deviceId: device_id, name });
export const syncWith = (peer: PeerInfo) => invoke<P2pSyncReport>("sync_with", { peer });
export const syncAll = () => invoke<P2pSyncReport[]>("sync_all");
export const p2pSettings = () => invoke<P2pSettings>("p2p_settings");
export const setBgSync = (enabled: boolean) => invoke<void>("set_bg_sync", { enabled });
export const setAutosyncSecs = (secs: number) =>
  invoke<number>("set_autosync_secs", { secs });
export const setP2pPortSetting = (port: number) =>
  invoke<number>("set_p2p_port_setting", { port });
export const setDownloadMedia = (enabled: boolean) =>
  invoke<boolean>("set_download_media", { enabled });
export const restartP2p = () => invoke<number>("restart_p2p");
export const mdnsRefresh = () => invoke<void>("mdns_refresh");
// Tauri v2 maps command args by camelCase keys (device_id -> deviceId).
export const setServerSync = (
  enabled: boolean,
  host: string,
  port: number,
  room: string,
  secret: string
) => invoke<ServerSyncStatus>("set_server_sync", { enabled, host, port, room, secret });
export const serverSyncStatus = () => invoke<ServerSyncStatus>("server_sync_status");
// Confirms the close-request flush (see the CloseRequested handler in
// lib.rs); the Rust side force-destroys the window after a timeout if
// this never comes.
export const forceClose = () => invoke<void>("force_close");
export const listConflicts = () =>
  invoke<Conflict[]>("list_conflicts");
export const conflictContent = (id: string) =>
  invoke<string>("conflict_content", { id });
export const resolveConflict = (id: string, keepTheirs: boolean) =>
  invoke<void>("resolve_conflict", { id, keepTheirs });
