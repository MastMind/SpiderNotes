use crate::server_sync::restart_server_sync;
use crate::state::{persist_peers, AppState, MDNS_REFRESH_SECS, PUSH_DEBOUNCE_MS};
use base64::Engine;
use spidernotes_core::{AssetReport, Note, NoteSummary, Vault};
use spidernotes_p2p::agent::{
    capped_initiator, capped_responder, is_busy_error, SyncReport as P2pSyncReport,
};
use spidernotes_p2p::discovery::{Discovery, PeerEvent, PeerInfo};
use spidernotes_p2p::transport::SyncListener;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::TcpStream;
use uuid::Uuid;

#[derive(serde::Serialize)]
pub struct OpenResult {
    pub path: String,
    pub p2p_port: u16,
    pub device_id: String,
    pub device_name: String,
    pub notes: Vec<NoteSummary>,
    pub peers: Vec<PeerInfo>,
    /// Vault folders (relative slash-separated paths), including empty ones.
    pub folders: Vec<String>,
    /// Local media-server port (127.0.0.1), 0 when it is not running.
    /// `<video>`/`<audio>` resolve to http://127.0.0.1:<port>/<rel> because
    /// WebKit's media pipeline cannot play large files from asset:/blob:.
    pub media_port: u16,
}

#[derive(serde::Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
}

#[derive(serde::Serialize)]
pub struct P2pSettings {
    /// Auto-sync ("keep alive") interval in seconds.
    pub autosync_secs: u64,
    /// Port the P2P listener is currently bound to (0 if not running).
    pub p2p_port: u16,
    /// Desired port, applied on (re)start of P2P.
    pub p2p_port_setting: u16,
    pub p2p_running: bool,
    /// Unix seconds of the last successful sync, `None` when never.
    pub last_sync: Option<u64>,
    /// Download media files from peers (notes sync regardless).
    pub download_media: bool,
}

/// Relay-server state for the UI. The secret is intentionally never
/// returned: it only travels frontend -> backend.
#[derive(serde::Serialize)]
pub struct ServerSyncStatus {
    pub enabled: bool,
    pub connected: bool,
    pub host: String,
    pub port: u16,
    pub room: String,
    pub peer_count: usize,
    pub status: String,
}

#[derive(serde::Serialize)]
pub struct Backlink {
    pub note: NoteSummary,
    pub count: usize,
}

#[derive(serde::Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: usize,
}

fn vault_or_err(st: &AppState) -> Result<Vault, String> {
    st.take_vault().ok_or_else(|| "vault is not open".to_string())
}

/// Destructive operations on an ENCRYPTED note (delete / move) require the
/// note to be unlocked first — the frontend catches this error, asks for
/// the password, calls `unlock_note`, and retries. Plain notes pass through.
fn require_unlocked(v: &Vault, st: &AppState, u: &Uuid) -> Result<(), String> {
    if v.get(u).is_some_and(|s| s.encrypted && !s.deleted) && st.note_key(u).is_none() {
        return Err("note is locked: enter its password first".to_string());
    }
    Ok(())
}

fn expand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    p.to_string()
}

/// `ip:port` for IPv4, `[ipv6]:port` for IPv6.
fn parse_peer_addr(addr: &str, port: u16) -> Option<SocketAddr> {
    let s = if addr.contains(':') {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    };
    s.parse::<SocketAddr>().ok()
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Hard cap on a full sync exchange. Without it a half-open connection
/// (peer accepts TCP but never speaks) would hold `sync_gate` forever and
/// freeze every save/delete/rename behind it.
pub(crate) const SYNC_PROTOCOL_TIMEOUT: Duration = Duration::from_secs(30);

/// Push the merged peer view (mDNS + relay) to the UI.
pub fn emit_peers(app: &AppHandle, st: &AppState) {
    let _ = app.emit("peers-updated", st.merged_peers());
}

/// Update the peer cache and the UI. Only a NON-EMPTY mDNS view replaces the
/// cache: a freshly (re)created daemon starts empty and would otherwise wipe
/// the restored (persisted) peers before it re-discovers them. The UI always
/// gets the merged view (mDNS + relay) through the event.
fn broadcast_peers(app: &AppHandle, st: &AppState, peers: &[PeerInfo]) {
    if !peers.is_empty() {
        *st.peers_cache.lock().unwrap() = peers.to_vec();
        persist_peers(&st.peers_path, peers);
    }
    emit_peers(app, st);
}

/// Connect to each *trusted* peer and run the initiator-side sync.
/// Untrusted peers are skipped (and logged) so notes never leave for unknown devices.
///
/// Gate discipline: the sync gate is taken per attempt and RELEASED during
/// busy-retry waits. Holding it while waiting would make our own retry hit
/// the peer's still-held gate (and vice versa) — a collision chain that
/// keeps both devices diverged for a whole autosync period.
async fn sync_vault_with_peers(
    st: &AppState,
    app: &AppHandle,
    vault: &Arc<Mutex<Option<Vault>>>,
    peers: &[PeerInfo],
) -> Vec<P2pSyncReport> {
    let mut out = Vec::new();
    for p in peers {
        if !st.trust.is_trusted(&p.device_id) {
            tracing::debug!("sync: skipping untrusted peer {} ({})", p.name, p.device_id);
            continue;
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            let _gate = st.sync_gate.lock().await;
            let Some(mut v) = vault.lock().unwrap().clone() else {
                tracing::warn!("sync: no vault is open");
                break;
            };
            if let Err(e) = v.scan_and_repair() {
                tracing::warn!("pre-sync scan: {e}");
            }
            // Relay peers: no direct route, the sync protocol runs over a
            // relay session stream (same frames, end-to-end encrypted).
            let res = if p.via_server {
                let handle = st.server_handle.lock().unwrap().clone();
                let Some(stream) = handle.and_then(|h| h.open_session(&p.device_id)) else {
                    tracing::warn!("sync: relay not connected, skipping {}", p.name);
                    break;
                };
                capped_initiator(stream, &st.identity, p.x25519_pub, &mut v, SYNC_PROTOCOL_TIMEOUT, st.download_media.load(Ordering::SeqCst))
                    .await
            } else {
                let Some(addr) = parse_peer_addr(&p.address, p.port) else {
                    tracing::warn!("sync: bad address for {}: {}", p.name, p.address);
                    break;
                };
                let stream =
                    match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
                        Ok(Ok(s)) => s,
                        Ok(Err(e)) => {
                            tracing::warn!("sync: cannot reach {}: {e}", p.name);
                            break;
                        }
                        Err(_) => {
                            tracing::warn!("sync: connect timeout for {} ({addr})", p.name);
                            break;
                        }
                    };
                capped_initiator(stream, &st.identity, p.x25519_pub, &mut v, SYNC_PROTOCOL_TIMEOUT, st.download_media.load(Ordering::SeqCst))
                    .await
            };
            match res {
                Ok(r) => {
                    st.put_vault(v);
                    out.push(r);
                    break;
                }
                Err(e) if p.via_server && is_busy_error(&e) && attempt <= 2 => {
                    // The peer is mid-sync with someone else and answered
                    // fast with "busy": release the gate, wait a staggered
                    // (and increasing) moment, retry. With the gate free
                    // during the wait, the staggered retries break the
                    // symmetry even when both devices retry at the same
                    // instant.
                    drop(_gate);
                    tracing::info!(
                        "sync: {} (via server) busy, attempt {attempt}/3",
                        p.name
                    );
                    tokio::time::sleep(Duration::from_millis(
                        1000 * attempt + rand::random::<u16>() as u64 % 1000,
                    ))
                    .await;
                }
                Err(e) => {
                    tracing::warn!("sync: {} failed: {e}", p.name);
                    break;
                }
            }
        }
    }
    if !out.is_empty() {
        // something actually moved: record it for the UI's "last synced"
        // line (an empty report = no reachable trusted peer, not a sync)
        st.last_sync.store(now_secs(), Ordering::SeqCst);
        let _ = app.emit("notes-changed", ());
    }
    out
}

/// Fire-and-forget background sync (auto-sync timer / peer-joined trigger).
pub fn spawn_auto_sync(app: AppHandle, st: AppState, peers: Vec<PeerInfo>) {
    if peers.is_empty() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        // Random 0-1.5s stagger: when both devices trigger at the same
        // instant (the same roster broadcast reaches both, or the timers
        // are aligned) both would become initiators and reject each other's
        // sessions. The stagger lets one side answer as the responder.
        let jitter = Duration::from_millis((rand::random::<u16>() % 1500) as u64);
        tokio::time::sleep(jitter).await;
        let reports = sync_vault_with_peers(&st, &app, &st.vault, &peers).await;
        for r in &reports {
            tracing::info!(
                "auto-sync {}: sent {}, applied {}, skipped {}",
                r.peer_name,
                r.sent,
                r.applied,
                r.skipped
            );
        }
    });
}

/// Debounced push of local edits to all *trusted* peers.
///
/// Called after every save/delete/rename. A burst of edits is coalesced into a
/// single delta-sync per debounce window: the first change spawns a task, later
/// changes only bump `change_seq`, and the task re-runs if edits arrive while it
/// is syncing so nothing is dropped.
fn schedule_push(app: AppHandle, st: AppState) {
    st.change_seq.fetch_add(1, Ordering::SeqCst);
    if st.push_pending.swap(true, Ordering::SeqCst) {
        return; // a push is already pending; it reads the latest vault state
    }
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(PUSH_DEBOUNCE_MS)).await;
            // Snapshot the change count *before* the sync clones the vault
            // (the clone happens inside sync_vault_with_peers, after this
            // read): any change with seq <= `before` is guaranteed to be
            // inside the cloned vault.
            let before = st.change_seq.load(Ordering::SeqCst);
            if st.vault.lock().unwrap().is_none() {
                st.push_pending.store(false, Ordering::SeqCst);
                return;
            }
            let peers = st.merged_peers();
            let reports = sync_vault_with_peers(&st, &app, &st.vault, &peers).await;
            let _ = app.emit("notes-changed", ());
            for r in &reports {
                tracing::info!(
                    "push-sync {}: sent {}, applied {}, skipped {}",
                    r.peer_name,
                    r.sent,
                    r.applied,
                    r.skipped
                );
            }
            let after = st.change_seq.load(Ordering::SeqCst);
            st.push_pending.store(false, Ordering::SeqCst);
            if after == before {
                break; // nothing changed while we were pushing
            }
            // edits landed mid-sync: loop to push them
            st.push_pending.store(true, Ordering::SeqCst);
        }
    });
}

/// Recreate the mDNS daemon in place (dropping the old one shuts its daemon
/// down) and keep the previous peer list visible until the fresh daemon
/// re-confirms the peers (avoids UI flicker).
fn recreate_discovery(
    discovery: &mut Discovery,
    identity: &spidernotes_core::DeviceIdentity,
    port: u16,
    peers_cache: &Arc<Mutex<Vec<PeerInfo>>>,
) {
    let old = discovery.peers();
    match Discovery::start(identity, port) {
        Ok(new_disc) => {
            *discovery = new_disc;
            *peers_cache.lock().unwrap() = old;
        }
        Err(e) => tracing::warn!("mDNS restart failed: {e}"),
    }
}

fn start_p2p(
    app: AppHandle,
    st: &AppState,
    listener: SyncListener,
    port: u16,
) -> Result<(), String> {
    // A single stop handle lets `restart_p2p` tear down all loops of this
    // P2P generation (accept / discovery / ticker) on demand.
    let (stop_tx, _initial) = tokio::sync::watch::channel(false);
    *st.p2p_stop.lock().unwrap() = Some(stop_tx.clone());

    // accept loop
    let (conn_tx, mut conn_rx) = tokio::sync::mpsc::channel::<TcpStream>(4);
    let mut stop_accept = stop_tx.subscribe();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::select! {
                _ = stop_accept.changed() => break,
                r = listener.accept() => match r {
                    Ok(stream) => {
                        if conn_tx.send(stream).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("P2P accept: {e}");
                        break;
                    }
                },
            }
        }
        tracing::info!("P2P accept loop stopped");
    });

    // responder loop (ends on its own once the accept loop drops `conn_tx`)
    let identity = st.identity.clone();
    let vaults_r = st.vault.clone();
    let gate_r = st.sync_gate.clone();
    let app_r = app.clone();
    let trust_r = st.trust.clone();
    let download_media_r = st.download_media.clone();
    let last_r = st.last_sync.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(stream) = conn_rx.recv().await {
            // try_lock, never await: if our own initiator holds the gate, the
            // remote side is (almost certainly) blocked on US waiting for this
            // responder to answer its initiator — awaiting the same gate here
            // is a circular wait across devices that freezes every save on
            // both ends. Rejecting instead breaks the cycle; the peer sees a
            // clean EOF and retries on its next auto-sync/push round.
            let Ok(_gate) = gate_r.try_lock() else {
                tracing::info!("P2P: local sync in progress, rejecting incoming connection");
                continue;
            };
            let Some(mut vault) = vaults_r.lock().unwrap().clone() else {
                tracing::warn!("P2P connection received, but no vault is open");
                continue;
            };
            if let Err(e) = vault.scan_and_repair() {
                tracing::warn!("pre-sync scan: {e}");
            }
            match capped_responder(
                stream,
                &identity,
                &mut vault,
                &trust_r,
                SYNC_PROTOCOL_TIMEOUT,
                download_media_r.load(Ordering::SeqCst),
            )
            .await
            {
                Ok(report) => {
                    tracing::info!(
                        "P2P sync with {}: sent {}, applied {}, skipped {}",
                        report.peer_name,
                        report.sent,
                        report.applied,
                        report.skipped
                    );
                    *vaults_r.lock().unwrap() = Some(vault);
                    // an incoming sync is a sync too: refresh the footer's
                    // "last synced" moment
                    last_r.store(now_secs(), Ordering::SeqCst);
                    let _ = app_r.emit("notes-changed", ());
                }
                Err(e) => tracing::warn!("P2P sync failed: {e}"),
            }
        }
    });

    // discovery loop
    let mut discovery = Discovery::start(&st.identity, port)
        .map_err(|e| format!("mDNS start failed: {e}"))?;
    let mut stop_disc = stop_tx.subscribe();
    let mut mdns_req_rx = st.mdns_refresh_tx.subscribe();
    let peers_cache = st.peers_cache.clone();
    let app_d = app.clone();
    let st_d = st.clone();
    let identity_d = st.identity.clone();
    tauri::async_runtime::spawn(async move {
        // STATIC refresh schedule: a fixed N-second period, independent of
        // the mDNS event flow (mdns-sd's own re-browse delay grows
        // exponentially 1s,2s,4s,...256s, so an elapsed-time check outside
        // the select could sleep through its deadline for minutes).
        let mut refresh_tick = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(MDNS_REFRESH_SECS),
            Duration::from_secs(MDNS_REFRESH_SECS),
        );
        refresh_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        broadcast_peers(&app_d, &st_d, &discovery.peers());
        loop {
            tokio::select! {
                _ = stop_disc.changed() => break,
                _ = refresh_tick.tick() => {
                    // The daemon now running has had a full cycle (~60s) to
                    // re-discover live peers. If it saw nothing while the
                    // cache is non-empty, the cached mDNS peers left the
                    // subnet: drop them. An empty view never replaces the
                    // cache by itself, so a stale direct entry would
                    // otherwise survive forever and shadow the relay entry
                    // for the same device in merged_peers (syncs would then
                    // target a dead address instead of the relay).
                    let fresh = discovery.peers();
                    if fresh.is_empty() && !peers_cache.lock().unwrap().is_empty() {
                        tracing::info!("mDNS: no peers seen for a full cycle — dropping stale cached peers");
                        *peers_cache.lock().unwrap() = Vec::new();
                        persist_peers(&st_d.peers_path, &[]);
                        broadcast_peers(&app_d, &st_d, &fresh);
                    }
                    // Recreate the mDNS daemon on the static schedule: a
                    // fresh daemon sends a new PTR query (a late peer's
                    // one-shot announce can be lost on Wi-Fi) and
                    // re-registers our service with the current IP (A
                    // records are filled once at registration and go stale
                    // after a DHCP change).
                    tracing::info!("mDNS daemon refresh (every {MDNS_REFRESH_SECS}s)");
                    recreate_discovery(
                        &mut discovery,
                        &identity_d,
                        port,
                        &peers_cache,
                    );
                },
                _ = mdns_req_rx.changed() => {
                    // On-demand refresh: the frontend fires this when the
                    // app returns to the foreground (on Android,
                    // backgrounding kills the multicast socket, and the
                    // static 60s tick would otherwise be the only recovery).
                    tracing::info!("mDNS daemon refresh (on demand)");
                    recreate_discovery(
                        &mut discovery,
                        &identity_d,
                        port,
                        &peers_cache,
                    );
                },
                ev = discovery.next_event() => match ev {
                    Some(PeerEvent::Joined(peer)) => {
                        broadcast_peers(&app_d, &st_d, &discovery.peers());
                        spawn_auto_sync(app_d.clone(), st_d.clone(), vec![peer]);
                    }
                    Some(PeerEvent::Left(_)) => {
                        broadcast_peers(&app_d, &st_d, &discovery.peers());
                    }
                    None => tokio::time::sleep(Duration::from_millis(200)).await,
                },
            }
        }
        tracing::info!("P2P discovery loop stopped");
    });

    // auto-sync ("keep alive") ticker; the interval is read each round so it
    // can be changed at runtime via `set_autosync_secs`.
    let mut stop_tick = stop_tx.subscribe();
    let st_t = st.clone();
    let app_t = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let secs = st_t.autosync_secs.load(Ordering::SeqCst).max(1);
            tokio::select! {
                _ = stop_tick.changed() => break,
                _ = tokio::time::sleep(Duration::from_secs(secs)) => {}
            }
            let peers = st_t.merged_peers();
            spawn_auto_sync(app_t.clone(), st_t.clone(), peers);
        }
    });
    Ok(())
}

pub async fn do_open_vault(
    app: AppHandle,
    path: String,
    p2p_port: Option<u16>,
) -> Result<OpenResult, String> {
    let st = (*app.state::<AppState>()).clone();
    // Serialize the vault replacement with in-flight syncs (their put-back
    // would otherwise restore the previous vault over the freshly opened one).
    let _gate = st.sync_gate.lock().await;
    let path = expand_tilde(&path);
    // The port set in advanced settings is the default when the caller does not
    // pass an explicit one.
    let want_port = p2p_port.unwrap_or_else(|| st.p2p_port_setting.load(Ordering::SeqCst));
    let p2p_port = if !st.p2p_running.load(Ordering::SeqCst) {
        let listener = SyncListener::bind(want_port)
            .await
            .map_err(|e| format!("cannot listen on P2P port {want_port}: {e}"))?;
        let actual = listener.port();
        start_p2p(app.clone(), &st, listener, actual)?;
        st.p2p_running.store(true, Ordering::SeqCst);
        st.p2p_port.store(actual, Ordering::SeqCst);
        actual
    } else {
        st.p2p_port.load(Ordering::SeqCst)
    };

    let mut vault = Vault::open(&path, st.identity.clone())
        .map_err(|e| format!("cannot open vault: {e}"))?;
    if let Err(e) = vault.scan_and_repair() {
        tracing::warn!("vault scan: {e}");
    }
    let notes = vault.visible_notes();
    let folders = vault.list_folders();
    let root = vault.root().to_string_lossy().to_string();
    // make the vault's files reachable through the asset protocol so the
    // editor can render markdown images/media with local paths
    let _ = app.asset_protocol_scope().allow_directory(&root, true);
    // the scope's glob matching uses require_literal_leading_dot (default
    // on unix: dotfiles are not exposed by default), so `<root>/**` does NOT
    // match dot-prefixed components — without this extra pattern every
    // asset: request for the hidden media dir answers 403
    let _ = app
        .asset_protocol_scope()
        .allow_directory(vault.root().join(".assets"), true);
    // a fresh vault session invalidates any previously held note keys
    st.lock_all_notes();
    // Serve the vault's hidden media dir over loopback HTTP: WebKit's media
    // pipeline cannot play large files from the asset: scheme (it probes
    // with a Range request that the protocol answers and then gives up) nor
    // from a large blob: URL — plain HTTP works (see media_server.rs).
    if let Some(old) = st.media_server.lock().unwrap().take() {
        old.stop();
    }
    let media_port = match crate::media_server::start(&vault.root().join(".assets")).await {
        Ok(s) => {
            let port = s.port;
            *st.media_server.lock().unwrap() = Some(s);
            port
        }
        Err(e) => {
            tracing::warn!("media server: {e}");
            0
        }
    };
    let peers = st.merged_peers();
    st.put_vault(vault);
    Ok(OpenResult {
        path: root,
        p2p_port,
        device_id: st.identity.id.clone(),
        device_name: st.identity.name.clone(),
        notes,
        peers,
        folders,
        media_port,
    })
}

#[tauri::command]
pub async fn open_vault(
    app: AppHandle,
    path: String,
    p2p_port: Option<u16>,
) -> Result<OpenResult, String> {
    do_open_vault(app, path, p2p_port).await
}

/// A sensible default vault location: `Documents/SpiderNotes` on desktop,
/// the app's internal data dir on mobile (always writable, private).
#[tauri::command]
pub fn default_vault_path(app: AppHandle) -> String {
    #[cfg(mobile)]
    {
        let base = app
            .path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        base.join("vault").to_string_lossy().to_string()
    }
    #[cfg(not(mobile))]
    {
        let _ = &app;
        let home = dirs::document_dir().or_else(dirs::home_dir).unwrap_or_default();
        home.join("SpiderNotes").to_string_lossy().to_string()
    }
}

/// Current vault state for the frontend (e.g. after `--open <path>`).
#[tauri::command]
pub fn vault_status(app: AppHandle) -> Result<Option<OpenResult>, String> {
    let st = (*app.state::<AppState>()).clone();
    {
        let guard = st.vault.lock().unwrap();
        let Some(v) = guard.as_ref() else {
            return Ok(None);
        };
        let peers = st.merged_peers();
        let media_port = st
            .media_server
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.port)
            .unwrap_or(0);
        Ok(Some(OpenResult {
            path: v.root().to_string_lossy().to_string(),
            p2p_port: st.p2p_port.load(Ordering::SeqCst),
            device_id: st.identity.id.clone(),
            device_name: st.identity.name.clone(),
            notes: v.visible_notes(),
            peers,
            folders: v.list_folders(),
            media_port,
        }))
    }
}

#[tauri::command]
pub fn list_notes(app: AppHandle) -> Result<Vec<NoteSummary>, String> {
    let st = (*app.state::<AppState>()).clone();
    Ok(vault_or_err(&st)?.visible_notes())
}

#[tauri::command]
pub fn read_note(app: AppHandle, uuid: String) -> Result<Note, String> {
    let st = (*app.state::<AppState>()).clone();
    let v = vault_or_err(&st)?;
    let u: Uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
    let n = v.read(&u).map_err(|e| e.to_string())?;
    // Encrypted notes: the file holds ciphertext. Return the plaintext only
    // while the note is unlocked (its key is held in memory); otherwise the
    // frontend must ask for the password (unlock_note) first.
    if v.get(&u).is_some_and(|s| s.encrypted) {
        let key = st
            .note_key(&u)
            .ok_or_else(|| "note is locked: enter its password first".to_string())?;
        let body = spidernotes_core::crypto::body_bytes(&n.content)
            .map_err(|e| e.to_string())?;
        let plain = spidernotes_core::crypto::decrypt_payload(&body, &key)
            .map_err(|e| e.to_string())?;
        return Ok(Note {
            content: plain,
            ..n
        });
    }
    Ok(n)
}

/// Create a NEW ENCRYPTED note. Derives the key from the password, writes
/// the ciphertext file, and keeps the key in memory (the note is created
/// already "unlocked", so the editor can show the plaintext immediately).
#[tauri::command]
pub async fn create_encrypted_note(
    app: AppHandle,
    path: String,
    password: String,
) -> Result<NoteSummary, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    let rel = spidernotes_core::vault::normalize_rel_path(&path).map_err(|e| e.to_string())?;
    let name = rel
        .trim_end_matches(".md")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    let plain = format!("# {name}\n");
    let (file, key) = spidernotes_core::crypto::create(&plain, &name, &password)
        .map_err(|e| e.to_string())?;
    let s = v.write(&path, &file).map_err(|e| e.to_string())?;
    st.unlock_note(s.uuid, key);
    st.put_vault(v);
    // clone: `_gate` borrows `st` until end of scope
    schedule_push(app, st.clone());
    Ok(s)
}

/// Unlock an encrypted note: verify the password (a wrong one fails the
/// GCM tag) and return the PLAINTEXT for the editor, keeping the derived
/// key in memory until `lock_note`.
#[tauri::command]
pub fn unlock_note(app: AppHandle, uuid: String, password: String) -> Result<String, String> {
    let st = (*app.state::<AppState>()).clone();
    let v = vault_or_err(&st)?;
    let u: Uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
    let n = v.read(&u).map_err(|e| e.to_string())?;
    let (plain, key) =
        spidernotes_core::crypto::unlock(&n.content, &password).map_err(|e| e.to_string())?;
    st.unlock_note(u, key);
    Ok(plain)
}

/// Lock an encrypted note: drop its held key. Subsequent reads/saves of it
/// fail until it is unlocked again.
#[tauri::command]
pub fn lock_note(app: AppHandle, uuid: String) -> Result<(), String> {
    let st = (*app.state::<AppState>()).clone();
    let u: Uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
    st.lock_note(&u);
    Ok(())
}

// Mutations (save/delete/rename) must hold `sync_gate`: an in-flight sync
// works on a CLONE of the vault and puts it back when done. Without the gate
// a save landing in that window is silently overwritten by the put-back —
// the edit's HLC is never bumped, so it is never propagated: permanent
// desync (hit reliably on Android, where the app is frozen in background).

#[tauri::command]
pub async fn save_note(
    app: AppHandle,
    path: String,
    content: String,
) -> Result<NoteSummary, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    // Encrypted notes: the editor holds plaintext, the file must hold
    // ciphertext. Re-encrypt with the held key (the note must be unlocked).
    // The title metadata is re-derived from the plaintext's heading.
    let rel = spidernotes_core::vault::normalize_rel_path(&path).map_err(|e| e.to_string())?;
    let existing = v.live_notes().into_iter().find(|n| n.path == rel);
    let file = match existing.filter(|s| s.encrypted) {
        Some(s) => {
            let key = st
                .note_key(&s.uuid)
                .ok_or_else(|| "note is locked: enter its password first".to_string())?;
            let raw = v.read(&s.uuid).map_err(|e| e.to_string())?;
            let (title, _) = spidernotes_core::note::title_preview(&content);
            spidernotes_core::crypto::reencrypt(&raw.content, &title, &content, &key)
                .map_err(|e| e.to_string())?
        }
        None => content.clone(),
    };
    let s = v.write(&path, &file).map_err(|e| e.to_string())?;
    st.put_vault(v);
    // clone: `_gate` borrows `st` until end of scope
    schedule_push(app, st.clone());
    Ok(s)
}

#[tauri::command]
pub async fn delete_note(app: AppHandle, uuid: String) -> Result<NoteSummary, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    let u: Uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
    // deleting an encrypted note requires its password (the frontend asks
    // and unlocks first) — a deliberate confirmation, not a security wall
    require_unlocked(&v, &st, &u)?;
    let s = v.delete(&u).map_err(|e| e.to_string())?;
    st.lock_note(&u);
    st.put_vault(v);
    // clone: `_gate` borrows `st` until end of scope
    schedule_push(app, st.clone());
    Ok(s)
}

#[tauri::command]
pub async fn rename_note(
    app: AppHandle,
    uuid: String,
    new_path: String,
    new_title: Option<String>,
) -> Result<NoteSummary, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    let u: Uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
    // moving an encrypted note requires its password first (the core also
    // rejects title rewrites of encrypted notes outright)
    require_unlocked(&v, &st, &u)?;
    let s = v
        .rename(&u, &new_path, new_title.as_deref())
        .map_err(|e| e.to_string())?;
    st.lock_note(&u);
    st.put_vault(v);
    // clone: `_gate` borrows `st` until end of scope
    schedule_push(app, st.clone());
    Ok(s)
}

/// Store a dropped media file (image / audio / video) into the vault's
 /// hidden `.assets/` dir and return its relative slash-separated path —
 /// the value the frontend puts into the `![alt](src)` markdown. Files are
 /// NOT notes: they are not indexed, scanned or synced (only the markdown
 /// referencing them travels with the note).
#[tauri::command]
pub async fn import_media_file(
    app: AppHandle,
    name: String,
    b64: String,
) -> Result<String, String> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(&b64)
        .map_err(|e| format!("invalid file data: {e}"))?;
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let root = st
        .take_vault()
        .map(|v| v.root().to_path_buf())
        .ok_or_else(|| "vault is not open".to_string())?;
    let assets = root.join(".assets");
    std::fs::create_dir_all(&assets).map_err(|e| e.to_string())?;
    let path = unique_media_path(&assets, &media_file_name(&name), &data);
    std::fs::write(&path, &data).map_err(|e| e.to_string())?;
    // ISO-BMFF files (mp4/mov/m4a/...) whose index (`moov`) sits at the
    // END of the file cannot be played by the web media pipeline — it
    // consumes the source as a non-seekable stream and stalls after the
    // first frame. Move `moov` to the front (faststart, as
    // `ffmpeg -movflags +faststart` does) so playback starts right away.
    if data.get(4..8) == Some(b"ftyp".as_slice()) {
        if let Err(e) = spidernotes_core::faststart::make_faststart(&path) {
            tracing::warn!("faststart rewrite failed for {}: {e}", path.display());
        }
    }
    Ok(format!(
        ".assets/{}",
        path.file_name().unwrap().to_string_lossy()
    ))
}

/// Orphan-asset report for the settings panel (scanned after autosave).
/// Encrypted notes whose key we currently hold are decrypted and checked;
/// the rest count into `encrypted_unchecked` (the UI warns about them).
#[tauri::command]
pub fn asset_report(app: AppHandle) -> Result<AssetReport, String> {
    let st = (*app.state::<AppState>()).clone();
    let v = vault_or_err(&st)?;
    let mut decrypted: Vec<(Uuid, String)> = Vec::new();
    for s in v.live_notes() {
        if !s.encrypted {
            continue;
        }
        let Some(key) = st.note_key(&s.uuid) else { continue };
        let Ok(n) = v.read(&s.uuid) else { continue };
        let plain = spidernotes_core::crypto::body_bytes(&n.content)
            .ok()
            .and_then(|b| spidernotes_core::crypto::decrypt_payload(&b, &key).ok());
        if let Some(p) = plain {
            decrypted.push((s.uuid, p));
        }
    }
    Ok(v.asset_report(&decrypted))
}

/// Delete asset files (the core guards to paths under assets/). Called by
/// the "clean up unused assets" dialog after an explicit user confirmation.
#[tauri::command]
pub async fn delete_assets(app: AppHandle, paths: Vec<String>) -> Result<u64, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let v = vault_or_err(&st)?;
    v.delete_assets(&paths).map_err(|e| e.to_string())
}

/// A dropped file's name reduced to a safe single path component
/// (drag-and-drop may deliver a full path; control/Windows-illegal
/// characters are dropped; an empty result becomes "file").
fn media_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `dir/name`, or `dir/name-N.ext` when a DIFFERENT file already holds the
/// name (identical bytes reuse the existing file — dropping the same image
/// twice must not create a copy).
fn unique_media_path(dir: &std::path::Path, name: &str, data: &[u8]) -> std::path::PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    if std::fs::read(&candidate)
        .map(|b| b == data)
        .unwrap_or(false)
    {
        return candidate;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let mut n = 1u32;
    loop {
        let candidate = dir.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

#[tauri::command]
pub fn list_folders(app: AppHandle) -> Result<Vec<String>, String> {
    let st = (*app.state::<AppState>()).clone();
    Ok(vault_or_err(&st)?.list_folders())
}

/// Create a folder. Empty folders are local-only (folders have no index
/// entries, so nothing to push); the gate still serializes with in-flight
/// syncs' pre-sync scans.
#[tauri::command]
pub async fn create_folder(app: AppHandle, path: String) -> Result<String, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    let p = v.create_folder(&path).map_err(|e| e.to_string())?;
    st.put_vault(v);
    Ok(p)
}

#[tauri::command]
pub async fn rename_folder(
    app: AppHandle,
    old_path: String,
    new_path: String,
) -> Result<usize, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    let n = v
        .rename_folder(&old_path, &new_path)
        .map_err(|e| e.to_string())?;
    st.put_vault(v);
    // moved notes got fresh HLCs: push them like any edit
    schedule_push(app, st.clone());
    Ok(n)
}

#[tauri::command]
pub async fn delete_folder(app: AppHandle, path: String) -> Result<Vec<NoteSummary>, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    let deleted = v.delete_folder(&path).map_err(|e| e.to_string())?;
    st.put_vault(v);
    // tombstones must reach the peers
    schedule_push(app, st.clone());
    Ok(deleted)
}

#[tauri::command]
pub fn search_notes(app: AppHandle, query: String) -> Result<Vec<NoteSummary>, String> {
    let st = (*app.state::<AppState>()).clone();
    Ok(vault_or_err(&st)?.search(&query))
}

#[tauri::command]
pub async fn rescan(app: AppHandle) -> Result<Vec<NoteSummary>, String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    v.scan_and_repair().map_err(|e| e.to_string())?;
    let notes = v.visible_notes();
    st.put_vault(v);
    Ok(notes)
}

// ---- sync conflicts (three-way merge leftovers) ----

/// Pending conflicts: peer versions that could not be auto-merged. Each one
/// keeps the peer's version so nothing is silently lost.
#[tauri::command]
pub fn list_conflicts(app: AppHandle) -> Result<Vec<spidernotes_core::Conflict>, String> {
    let st = (*app.state::<AppState>()).clone();
    Ok(vault_or_err(&st)?.conflicts())
}

/// The peer's version stored with a conflict (for the resolution panel).
#[tauri::command]
pub fn conflict_content(app: AppHandle, id: String) -> Result<String, String> {
    let st = (*app.state::<AppState>()).clone();
    vault_or_err(&st)?.conflict_content(&id).map_err(|e| e.to_string())
}

/// Resolve a conflict. `keepTheirs = false` keeps our version (the peer's is
/// discarded and suppressed, so it does not come back on the next sync);
/// `keepTheirs = true` adopts the peer's version as a local edit — a fresh
/// HLC that beats the peer's, pushed to the peers like any save.
#[tauri::command]
pub async fn resolve_conflict(
    app: AppHandle,
    id: String,
    keep_theirs: bool,
) -> Result<(), String> {
    let st = (*app.state::<AppState>()).clone();
    let _gate = st.sync_gate.lock().await;
    let mut v = vault_or_err(&st)?;
    if keep_theirs {
        v.resolve_conflict_keep_theirs(&id).map_err(|e| e.to_string())?;
    } else {
        v.resolve_conflict_keep_ours(&id).map_err(|e| e.to_string())?;
    }
    st.put_vault(v);
    // the note (or the conflict list) changed for the UI
    let _ = app.emit("notes-changed", ());
    // "keep theirs" is a local edit: push it to the peers
    if keep_theirs {
        schedule_push(app, st.clone());
    }
    Ok(())
}

#[tauri::command]
pub fn backlinks(app: AppHandle, uuid: String) -> Result<Vec<Backlink>, String> {
    let st = (*app.state::<AppState>()).clone();
    let v = vault_or_err(&st)?;
    let u: Uuid = uuid.parse::<Uuid>().map_err(|e| e.to_string())?;
    Ok(spidernotes_core::wikilink::backlinks(&v, &u)
        .into_iter()
        .map(|(note, count)| Backlink { note, count })
        .collect())
}

#[tauri::command]
pub fn all_tags(app: AppHandle) -> Result<Vec<TagCount>, String> {
    let st = (*app.state::<AppState>()).clone();
    let v = vault_or_err(&st)?;
    Ok(spidernotes_core::wikilink::all_tags(&v)
        .into_iter()
        .map(|(tag, count)| TagCount { tag, count })
        .collect())
}

#[tauri::command]
pub fn notes_with_tag(app: AppHandle, tag: String) -> Result<Vec<NoteSummary>, String> {
    let st = (*app.state::<AppState>()).clone();
    let v = vault_or_err(&st)?;
    Ok(spidernotes_core::wikilink::notes_with_tag(&v, &tag))
}

#[tauri::command]
pub fn device_info(app: AppHandle) -> Result<DeviceInfo, String> {
    let st = (*app.state::<AppState>()).clone();
    Ok(DeviceInfo {
        id: st.identity.id.clone(),
        name: st.identity.name.clone(),
    })
}

/// Rename this device. Persists to the identity file; peers see the new
/// name in the p2p Hello of the next app launch (the running P2P agent
/// keeps its startup identity until then).
#[tauri::command]
pub fn set_device_name(app: AppHandle, name: String) -> Result<DeviceInfo, String> {
    let clean = name.trim().to_string();
    if clean.is_empty() || clean.chars().count() > 40 {
        return Err("device name must be 1-40 characters".to_string());
    }
    let st = (*app.state::<AppState>()).clone();
    let mut identity = st.identity.clone();
    identity.name = clean.clone();
    #[cfg(mobile)]
    {
        // Must match where lib.rs loads it: `load_or_create_in(&name, &dir)`
        // joins "spidernotes" onto the app data dir, so the file lives at
        // `<app_data>/spidernotes/identity.json`. Saving to `<app_data>`
        // directly (the old code) wrote a file the startup never reads, so the
        // rename was lost on restart and peers kept seeing the old name.
        let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        identity.save_in(&dir.join("spidernotes")).map_err(|e| e.to_string())?;
    }
    #[cfg(not(mobile))]
    {
        identity.save().map_err(|e| e.to_string())?;
    }
    Ok(DeviceInfo {
        id: identity.id.clone(),
        name: clean,
    })
}

#[tauri::command]
 pub fn list_peers(app: AppHandle) -> Result<Vec<PeerInfo>, String> {
    let st = (*app.state::<AppState>()).clone();
    let peers = st.merged_peers();
    Ok(peers)
 }

 #[tauri::command]
 pub fn list_trusted(app: AppHandle) -> Result<Vec<spidernotes_core::TrustedDevice>, String> {
    let st = (*app.state::<AppState>()).clone();
    Ok(st.trust.list())
 }

 #[tauri::command]
 pub fn trust_device(
    app: AppHandle,
    device_id: String,
    name: String,
) -> Result<Vec<spidernotes_core::TrustedDevice>, String> {
    let st = (*app.state::<AppState>()).clone();
     if device_id == st.identity.id {
          return Err("that is this device — trust the OTHER device's id instead".into());
      }
      if let Err(e) = st.trust.add(&device_id, &name) {
          tracing::error!("trust_device failed for {} ({}): {e}", name, device_id);
          return Err(format!("cannot save trust list: {e}"));
      }
      tracing::info!("trusted device {} ({})", name, device_id);
      Ok(st.trust.list())
  }

 #[tauri::command]
  pub fn untrust_device(app: AppHandle, device_id: String)
  -> Result<Vec<spidernotes_core::TrustedDevice>, String> {
    let st = (*app.state::<AppState>()).clone();
     if let Err(e) = st.trust.remove(&device_id) {
          tracing::error!("untrust_device failed for {device_id}: {e}");
          return Err(format!("cannot save trust list: {e}"));
      }
    tracing::info!("untrusted device {device_id}");
      Ok(st.trust.list())
    }

 /// Set a LOCAL display name for a peer (this device only: the override never
 /// travels over the wire, the peer keeps advertising its own name). The UI
 /// list refreshes through the peers-updated event; it applies regardless of
 /// whether the peer is trusted.
  #[tauri::command]
  pub fn rename_peer(app: AppHandle, device_id: String, name: String) -> Result<(), String> {
    let clean = name.trim().to_string();
    if clean.is_empty() || clean.chars().count() > 40 {
      return Err("peer name must be 1-40 characters".to_string());
    }
    if device_id == app.state::<AppState>().identity.id {
      return Err("that is this device — rename it in My device instead".into());
    }
    let st = (*app.state::<AppState>()).clone();
    if let Err(e) = st.peer_names.set(&device_id, &clean) {
      tracing::error!("rename_peer failed for {device_id}: {e}");
      return Err(format!("cannot save peer names: {e}"));
    }
    tracing::info!("peer {device_id} shows as {clean}");
    emit_peers(&app, &st);
    Ok(())
  }

#[tauri::command]
pub fn p2p_settings(app: AppHandle) -> Result<P2pSettings, String> {
    let st = (*app.state::<AppState>()).clone();
    let last = st.last_sync.load(Ordering::SeqCst);
    Ok(P2pSettings {
        autosync_secs: st.autosync_secs.load(Ordering::SeqCst),
        p2p_port: st.p2p_port.load(Ordering::SeqCst),
        p2p_port_setting: st.p2p_port_setting.load(Ordering::SeqCst),
        p2p_running: st.p2p_running.load(Ordering::SeqCst),
        last_sync: (last != 0).then_some(last),
        download_media: st.download_media.load(Ordering::SeqCst),
    })
}

/// Allow/refuse storing media files that arrive attached to synced notes.
/// Notes themselves always sync; this only gates the .assets/ bytes. The
/// choice is per-device and takes effect from the next sync round.
#[tauri::command]
pub fn set_download_media(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let st = (*app.state::<AppState>()).clone();
    st.download_media.store(enabled, Ordering::SeqCst);
    tracing::info!("download media {enabled}");
    Ok(enabled)
}

#[tauri::command]
pub fn set_autosync_secs(app: AppHandle, secs: u64) -> Result<u64, String> {
    let st = (*app.state::<AppState>()).clone();
    let v = secs.clamp(1, 3600);
    st.autosync_secs.store(v, Ordering::SeqCst);
    tracing::info!("auto-sync interval set to {v}s");
    Ok(v)
}

#[tauri::command]
pub fn set_p2p_port_setting(app: AppHandle, port: u16) -> Result<u16, String> {
    let st = (*app.state::<AppState>()).clone();
    if port == 0 {
        return Err("port 0 is not allowed".to_string());
    }
    st.p2p_port_setting.store(port, Ordering::SeqCst);
  tracing::info!("P2P port setting set to {port} (applied on P2P restart)");
    Ok(port)
 }

/// Background sync (Android only): start/stop the foreground service that
 /// keeps the process — and with it the mDNS/TCP sync loops — alive while
 /// the app is backgrounded. No-op on desktop.
 #[tauri::command]
 pub async fn set_bg_sync(app: AppHandle, enabled: bool) -> Result<(), String> {
     #[cfg(target_os = "android")]
     {
         let Some(st) = app.try_state::<spidernotes_fgservice::FgState<tauri::Wry>>() else {
             return Err("background sync unavailable (plugin failed to register)".into());
         };
         let cmd = if enabled { "start" } else { "stop" };
         st.handle
             .run_mobile_plugin_async::<()>(cmd, ())
             .await
             .map_err(|e| e.to_string())
     }
     #[cfg(not(target_os = "android"))]
     {
         let _ = (app, enabled);
         Ok(())
     }
 }

  /// Ask the discovery loop to recreate the mDNS daemon immediately.
/// Fired by the frontend when the app returns to the foreground: on
/// Android, backgrounding kills the multicast socket, so the daemon must be
/// rebuilt before peers (and with them sync) come back.
#[tauri::command]
 pub fn mdns_refresh(app: AppHandle) -> Result<(), String> {
    let st = (*app.state::<AppState>()).clone();
    if !st.p2p_running.load(Ordering::SeqCst) {
        return Ok(()); // nothing to refresh; P2P not started
    }
    let next = *st.mdns_refresh_tx.borrow() + 1;
    let _ = st.mdns_refresh_tx.send_replace(next);
    Ok(())
 }

 /// Enable/disable (or reconfigure) the relay server. With `enabled = false`
 /// the relay loop is stopped and the room peers are cleared.
 #[tauri::command]
 pub async fn set_server_sync(
    app: AppHandle,
    enabled: bool,
    host: String,
    port: u16,
    room: String,
    secret: String,
 ) -> Result<ServerSyncStatus, String> {
    let st = (*app.state::<AppState>()).clone();
    if enabled {
        if host.trim().is_empty() {
            return Err("server host is empty".into());
        }
        if port == 0 {
            return Err("server port is 0".into());
        }
        if room.trim().is_empty() {
            return Err("room id is empty".into());
        }
        if secret.trim().is_empty() {
            return Err("secret is empty".into());
        }
        *st.server_cfg.lock().unwrap() = Some(crate::state::ServerCfg {
            host: host.trim().to_string(),
            port,
            room: room.trim().to_string(),
            secret,
        });
    } else {
        *st.server_cfg.lock().unwrap() = None;
    }
    restart_server_sync(app.clone(), &st);
    Ok(server_sync_status_inner(&st))
 }

 fn server_sync_status_inner(st: &AppState) -> ServerSyncStatus {
    let cfg = st.server_cfg.lock().unwrap().clone();
    let connected = st.server_handle.lock().unwrap().as_ref().is_some_and(|h| h.connected());
    let peer_count = st.server_peers.lock().unwrap().len();
    match cfg {
        Some(c) => ServerSyncStatus {
            enabled: true,
            connected,
            host: c.host,
            port: c.port,
            room: c.room,
            peer_count,
            status: st.server_status.lock().unwrap().clone(),
        },
        None => ServerSyncStatus {
            enabled: false,
            connected: false,
            host: String::new(),
            port: 0,
            room: String::new(),
            peer_count: 0,
            status: st.server_status.lock().unwrap().clone(),
        },
    }
 }

 #[tauri::command]
 pub fn server_sync_status(app: AppHandle) -> ServerSyncStatus {
    let st = (*app.state::<AppState>()).clone();
    server_sync_status_inner(&st)
 }

#[tauri::command]
pub async fn restart_p2p(app: AppHandle) -> Result<u16, String> {
    let st = (*app.state::<AppState>()).clone();
    if !st.p2p_running.load(Ordering::SeqCst) {
        return Err("P2P is not running; open a vault first".to_string());
    }
    let old_port = st.p2p_port.load(Ordering::SeqCst);
    let want = st.p2p_port_setting.load(Ordering::SeqCst);
    // Always restart (even with the same port): this is also the manual
    // escape hatch for a stuck discovery daemon.
    // tear down the current P2P generation (accept / discovery / ticker loops)
    if let Some(tx) = st.p2p_stop.lock().unwrap().take() {
        let _ = tx.send(true);
    }
    st.p2p_running.store(false, Ordering::SeqCst);
    // give the old loops a moment to drop the mDNS registration and sockets
    tokio::time::sleep(Duration::from_millis(300)).await;
    let bound = match SyncListener::bind(want).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("bind port {want} failed ({e}); falling back to {old_port}");
            SyncListener::bind(old_port)
                .await
                .map_err(|e2| format!("cannot bind port {want} or fallback {old_port}: {e2}"))?
        }
    };
    let actual = bound.port();
    start_p2p(app, &st, bound, actual)?;
    st.p2p_running.store(true, Ordering::SeqCst);
    st.p2p_port.store(actual, Ordering::SeqCst);
    Ok(actual)
}

#[tauri::command]
pub async fn sync_with(app: AppHandle, peer: PeerInfo) -> Result<P2pSyncReport, String> {
    let st = (*app.state::<AppState>()).clone();
    if !st.trust.is_trusted(&peer.device_id) {
        return Err(format!(
            "device {} is not trusted; approve it first",
            peer.name
        ));
    }
    let mut attempt = 0;
    loop {
        attempt += 1;
        let _gate = st.sync_gate.lock().await;
        let mut v = vault_or_err(&st)?;
        let res = if peer.via_server {
            let handle = st.server_handle.lock().unwrap().clone();
            let stream = handle
                .and_then(|h| h.open_session(&peer.device_id))
                .ok_or_else(|| "relay server is not connected".to_string())?;
            capped_initiator(stream, &st.identity, peer.x25519_pub, &mut v, SYNC_PROTOCOL_TIMEOUT, st.download_media.load(Ordering::SeqCst))
                .await
        } else {
            let addr = parse_peer_addr(&peer.address, peer.port)
                .ok_or_else(|| format!("bad address for {}: {}", peer.name, peer.address))?;
            let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
                .await
                .map_err(|_| format!("connect timeout to {addr}"))?
                .map_err(|e| format!("cannot connect to {addr}: {e}"))?;
            capped_initiator(stream, &st.identity, peer.x25519_pub, &mut v, SYNC_PROTOCOL_TIMEOUT, st.download_media.load(Ordering::SeqCst))
                .await
        };
        match res {
            Ok(r) => {
                st.put_vault(v);
                st.last_sync.store(now_secs(), Ordering::SeqCst);
                let _ = app.emit("notes-changed", ());
                return Ok(r);
            }
            // The peer is mid-sync with someone else and answered fast with
            // a "busy" error — release the gate, wait a staggered moment and
            // retry instead of surfacing the error (bounded: 3 attempts
            // total).
            Err(e) if is_busy_error(&e) && attempt < 3 => {
                drop(_gate);
                tracing::info!(
                    "sync: {} is busy, attempt {attempt}/3 in ~{}s",
                    peer.name,
                    attempt
                );
                tokio::time::sleep(
                    Duration::from_secs(attempt as u64)
                        + Duration::from_millis(rand::random::<u16>() as u64 % 1000),
                )
                .await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Current unix time in seconds (0 if the clock is before the epoch).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[tauri::command]
pub async fn sync_all(app: AppHandle) -> Result<Vec<P2pSyncReport>, String> {
    let st = (*app.state::<AppState>()).clone();
    let peers = st.merged_peers();
    let out = sync_vault_with_peers(&st, &app, &st.vault, &peers).await;
    Ok(out)
}

/// Destroy the main window. The frontend calls this after flushing pending
/// edits in response to the "sn-before-close" event (emitted from the
/// CloseRequested handler in lib.rs, which also force-destroys the window
/// after a timeout as the safety net if the frontend never confirms).
#[tauri::command]
pub fn force_close(app: AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.destroy();
    }
}


