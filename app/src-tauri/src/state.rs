use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64};
use std::sync::{Arc, Mutex};
use spidernotes_core::{DeviceIdentity, TrustStore, Vault};
use spidernotes_p2p::discovery::PeerInfo;
use spidernotes_p2p::server_client::ServerHandle;

pub const DEFAULT_P2P_PORT: u16 = 8788;
/// Default interval of the background auto-sync ("keep alive") loop.
pub const AUTO_SYNC_INTERVAL_SECS: u64 = 30;
/// Debounce window before pushing local changes to trusted peers.
pub const PUSH_DEBOUNCE_MS: u64 = 800;
/// STATIC period between mDNS daemon recreations (a fixed timer in the
/// discovery loop, not dependent on mDNS event flow): a fresh daemon sends
/// a new PTR query (a one-shot announce can be lost on Wi-Fi) and
/// re-registers our service with the current IP (A records go stale after
/// a DHCP change).
pub const MDNS_REFRESH_SECS: u64 = 60;

/// Relay-server settings chosen in the frontend. `None` means server sync is
/// off. The secret lives only here (and in the frontend's localStorage); it
/// is never returned by any command.
#[derive(Clone)]
pub struct ServerCfg {
    pub host: String,
    pub port: u16,
    pub room: String,
    pub secret: String,
}

#[derive(Clone)]
pub struct AppState {
    pub identity: DeviceIdentity,
    pub vault: Arc<Mutex<Option<Vault>>>,
    pub peers_cache: Arc<Mutex<Vec<PeerInfo>>>,
    /// Where the peer list is persisted, so a (re)started app can reconnect
    /// to the last known peers before mDNS re-discovers them (Android kills
    /// the process in the background, wiping the in-memory cache).
    pub peers_path: Arc<PathBuf>,
    pub p2p_running: Arc<AtomicBool>,
    /// Actual bound P2P port (0 until started).
    pub p2p_port: Arc<AtomicU16>,
    /// Serializes every sync (initiator, responder, server): clone -> sync -> put-back
    /// must not interleave, otherwise concurrent put-backs lose index updates.
    pub sync_gate: Arc<tokio::sync::Mutex<()>>,
    /// Allowlist of device ids allowed to exchange notes with this one.
    /// Arc (not a value): AppState is cloned per command, and a plain field
    /// would give every clone its own copy — trust_device would mutate the
    /// clone while sync_* kept reading a stale list. TrustStore itself has
    /// interior mutability, so sharing one instance is safe.
    pub trust: Arc<TrustStore>,
    /// Auto-sync ("keep alive") interval in seconds, adjustable at runtime.
    pub autosync_secs: Arc<AtomicU64>,
    /// Desired P2P port; applied on (re)start of the P2P listener.
    pub p2p_port_setting: Arc<AtomicU16>,
    /// Push-on-change: a background sync of local edits is already pending.
    pub push_pending: Arc<AtomicBool>,
    /// Monotonic count of local note changes (used to coalesce push-on-change).
    pub change_seq: Arc<AtomicU64>,
  /// Stop handle for the running P2P loops (accept / discovery / ticker).
  pub p2p_stop: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,
  /// On-demand mDNS daemon refresh (counter). The discovery loop recreates
  /// the daemon whenever this changes — e.g. when the app returns to the
  /// foreground on Android, where backgrounding kills the multicast socket.
  pub mdns_refresh_tx: tokio::sync::watch::Sender<u64>,
  /// Relay-server settings (None = off).
  pub server_cfg: Arc<Mutex<Option<ServerCfg>>>,
  /// Peers seen in our relay room (replaced on every roster update).
  pub server_peers: Arc<Mutex<Vec<PeerInfo>>>,
  /// Live relay handle (open sessions while connected), None when down.
  pub server_handle: Arc<Mutex<Option<ServerHandle>>>,
  /// Stop handle for the relay client loop.
  pub server_stop: Arc<Mutex<Option<tokio::sync::watch::Sender<bool>>>>,
  /// Human-readable relay status line for the UI.
  pub server_status: Arc<Mutex<String>>,
}

impl AppState {
    pub fn new(identity: DeviceIdentity, trust: TrustStore, peers_path: PathBuf) -> Self {
        let (mdns_refresh_tx, _mdns_refresh_rx) = tokio::sync::watch::channel(0u64);
        Self {
            identity,
            vault: Arc::new(Mutex::new(None)),
            peers_cache: Arc::new(Mutex::new(load_peers(&peers_path))),
            peers_path: Arc::new(peers_path),
            p2p_running: Arc::new(AtomicBool::new(false)),
            p2p_port: Arc::new(AtomicU16::new(0)),
            sync_gate: Arc::new(tokio::sync::Mutex::new(())),
            trust: Arc::new(trust),
            autosync_secs: Arc::new(AtomicU64::new(AUTO_SYNC_INTERVAL_SECS)),
            p2p_port_setting: Arc::new(AtomicU16::new(DEFAULT_P2P_PORT)),
            push_pending: Arc::new(AtomicBool::new(false)),
            change_seq: Arc::new(AtomicU64::new(0)),
            p2p_stop: Arc::new(Mutex::new(None)),
            mdns_refresh_tx,
            server_cfg: Arc::new(Mutex::new(None)),
            server_peers: Arc::new(Mutex::new(Vec::new())),
            server_handle: Arc::new(Mutex::new(None)),
            server_stop: Arc::new(Mutex::new(None)),
            server_status: Arc::new(Mutex::new("server sync is off".into())),
        }
    }

    /// mDNS peers + relay peers, deduplicated by device id. Direct (mDNS)
    /// wins: a device we can reach without the relay is always preferred.
    /// Our own device id is never a peer (a stale persisted entry or a
    /// server that lists the member in its own roster must not turn us
    /// into a sync target for ourselves).
    pub fn merged_peers(&self) -> Vec<PeerInfo> {
        let mut map: std::collections::HashMap<String, PeerInfo> = std::collections::HashMap::new();
        for p in self.server_peers.lock().unwrap().iter().chain(self.peers_cache.lock().unwrap().iter())
        {
            if p.device_id == self.identity.id {
                continue;
            }
            map.insert(p.device_id.clone(), p.clone());
        }
        map.into_values().collect()
    }

    pub fn take_vault(&self) -> Option<Vault> {
        self.vault.lock().unwrap().clone()
    }

    pub fn put_vault(&self, vault: Vault) {
        *self.vault.lock().unwrap() = Some(vault);
    }
}

/// Last known peers, loaded at startup (best-effort: a missing or broken
/// file just means an empty cache until mDNS re-discovers).
pub fn load_peers(path: &Path) -> Vec<PeerInfo> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).unwrap_or_default())
        .unwrap_or_default()
}

pub fn persist_peers(path: &Path, peers: &[PeerInfo]) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(peers) {
        let _ = std::fs::write(path, json);
    }
}
