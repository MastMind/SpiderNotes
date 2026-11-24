//! End-to-end relay-sync tests: two full app-equivalent nodes over the REAL
//! relay server (`spidernotes_server::Server`).
//!
//! `Node` mirrors the app's relay orchestration 1:1 (same primitives, same
//! gate discipline, same triggers), so these tests exercise the actual
//! deadlock-prone mechanisms:
//!   * `relay_loop` — connect with backoff; the initial roster is applied
//!     WITHOUT a sync trigger; a RosterUpdated event triggers auto-sync for
//!     trusted peers only when the trusted set actually changed (dedupe);
//!     relay loss clears peers and reconnects.
//!   * responder — `try_lock` the gate (never await: the peer's initiator
//!     may be blocked on us); busy -> `reject_busy` (Noise handshake +
//!     "busy" protocol error, because a dropped relay session is not
//!     visible to the peer as EOF); otherwise `capped_responder`.
//!   * initiator — `capped_initiator` (10s cap); on a "busy" error the
//!     auto-sync path retries once after a 1-2s stagger, the manual path up
//!     to 3 attempts (same as the app).
//!   * push-on-change — debounced, under the gate, re-runs if edits land
//!     mid-push.
//!   * auto-sync ticker (2s here, 30s in the app).

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spidernotes_core::{DeviceIdentity, TrustStore, Vault};
use spidernotes_p2p::agent::{
    capped_initiator, capped_responder, is_busy_error, reject_busy, SyncReport,
};
use spidernotes_p2p::discovery::PeerInfo;
use spidernotes_p2p::relay::{RelayStream, RosterPeer};
use spidernotes_p2p::server_client::{ServerClient, ServerEvent, ServerHandle};
use spidernotes_server::Server;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const SECRET: &str = "test-secret";
const ROOM: &str = "test-room";
/// Cap of one sync exchange (the app uses 30s; 10s keeps the suite fast —
/// healthy exchanges finish in well under a second).
const SYNC_TIMEOUT: Duration = Duration::from_secs(10);
const PUSH_DEBOUNCE: Duration = Duration::from_millis(100);

// ---------- helpers ----------

fn log(events: &Arc<Mutex<Vec<String>>>, line: &str) {
    tracing::info!("{line}");
    events.lock().unwrap().push(line.to_string());
}

fn roster_to_peers(roster: &[RosterPeer]) -> Vec<PeerInfo> {
    roster
        .iter()
        .map(|p| PeerInfo {
            name: p.name.clone(),
            device_id: p.device_id.clone(),
            x25519_pub: p.x25519_pub,
            address: String::new(),
            port: 0,
            via_server: true,
        })
        .collect()
}

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("sn-relaytest-{tag}-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn wait_until(cond: impl Fn() -> bool, deadline: Duration) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn assert_waited(ok: bool, what: &str) {
    assert!(ok, "timed out waiting for: {what}");
}

fn snapshot(v: &Vault) -> BTreeMap<String, (Uuid, String)> {
    let mut m = BTreeMap::new();
    for s in v.live_notes() {
        if let Ok(note) = v.read(&s.uuid) {
            m.insert(s.path.clone(), (s.uuid, note.content.clone()));
        }
    }
    m
}

async fn wait_converged(a: &Node, b: &Node, deadline: Duration) {
    let start = Instant::now();
    loop {
        let sa = snapshot(&a.vault.lock().unwrap());
        let sb = snapshot(&b.vault.lock().unwrap());
        if sa == sb {
            return;
        }
        if start.elapsed() > deadline {
            let only_a: Vec<String> = sa.keys().filter(|k| !sb.contains_key(*k)).cloned().collect();
            let only_b: Vec<String> = sb.keys().filter(|k| !sa.contains_key(*k)).cloned().collect();
            let diff: Vec<String> = sa
                .keys()
                .filter(|k| sb.contains_key(*k) && sb[*k] != sa[*k])
                .cloned()
                .collect();
            let ea: Vec<String> = a.events.lock().unwrap().clone();
            let eb: Vec<String> = b.events.lock().unwrap().clone();
            panic!(
                "vaults not converged within {deadline:?}\n only-a={only_a:?}\n only-b={only_b:?}\n content-diff={diff:?}\n--- A events ---\n{}\n--- B events ---\n{}",
                ea.join("\n"),
                eb.join("\n")
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn start_server(secret: &str) -> (SocketAddr, Arc<Server>, tokio::task::JoinHandle<()>) {
    let server = Arc::new(Server::new(secret.to_string()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let s = (*server).clone();
    let task = tokio::spawn(async move {
        let _ = s.run(listener).await;
    });
    (addr, server, task)
}

/// A fresh server bound to the SAME address (for the server-restart test).
async fn restart_server_on(
    addr: SocketAddr,
    secret: &str,
) -> (Arc<Server>, tokio::task::JoinHandle<()>) {
    let server = Arc::new(Server::new(secret.to_string()));
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let s = (*server).clone();
    let task = tokio::spawn(async move {
        let _ = s.run(listener).await;
    });
    (server, task)
}

// ---------- the node (app-equivalent) ----------

pub struct Node {
    pub id: String,
    pub name: String,
    identity: DeviceIdentity,
    pub vault: Arc<Mutex<Vault>>,
    pub trust: Arc<TrustStore>,
    pub gate: Arc<tokio::sync::Mutex<()>>,
    pub peers: Arc<Mutex<Vec<PeerInfo>>>,
    pub handle: Arc<Mutex<Option<ServerHandle>>>,
    run_task: Arc<Mutex<Option<tokio::task::JoinHandle<anyhow::Result<()>>>>>,
    stop_tx: tokio::sync::watch::Sender<bool>,
    loop_task: Option<tokio::task::JoinHandle<()>>,
    jitter_ms: Arc<AtomicU64>,
    pub change_seq: Arc<AtomicU64>,
    push_pending: Arc<AtomicBool>,
    pub events: Arc<Mutex<Vec<String>>>,
    dir: std::path::PathBuf,
}

impl Node {
    fn new(name: &str, dir: std::path::PathBuf) -> anyhow::Result<Self> {
        let identity = DeviceIdentity::load_or_create_in(name, &dir.join("id"))?;
        let vault = Arc::new(Mutex::new(Vault::open(dir.join("vault"), identity.clone())?));
        let id = identity.id.clone();
        let nm = identity.name.clone();
        Ok(Self {
            id,
            name: nm,
            identity,
            vault,
            trust: Arc::new(TrustStore::memory()),
            gate: Arc::new(tokio::sync::Mutex::new(())),
            peers: Arc::new(Mutex::new(Vec::new())),
            handle: Arc::new(Mutex::new(None)),
            run_task: Arc::new(Mutex::new(None)),
            stop_tx: tokio::sync::watch::channel(false).0,
            loop_task: None,
            jitter_ms: Arc::new(AtomicU64::new(1500)),
            change_seq: Arc::new(AtomicU64::new(0)),
            push_pending: Arc::new(AtomicBool::new(false)),
            events: Arc::new(Mutex::new(Vec::new())),
            dir,
        })
    }

    pub fn trust(&self, other: &Node) {
        self.trust.add(&other.id, &other.name).unwrap();
    }

    /// Trust our OWN id — the worst-case contaminated trust list (both
    /// devices show up as identically-named "device", so a user could
    /// easily have approved their own). Regression hook for self-sync.
    pub fn trust_self(&self) {
        self.trust.add(&self.id, &self.name).unwrap();
    }

    /// 0 = deterministic (no stagger) — used to FORCE collisions.
    pub fn set_jitter_ms(&self, ms: u64) {
        self.jitter_ms.store(ms, Ordering::SeqCst);
    }

    /// (Re)start the relay loops. On restart the vault is re-opened from
    /// disk, like the app at process start.
    pub async fn start(&mut self, addr: SocketAddr) {
        *self.vault.lock().unwrap() =
            Vault::open(self.dir.join("vault"), self.identity.clone()).unwrap();
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        self.stop_tx = stop_tx;
        let identity = self.identity.clone();
        let vault = self.vault.clone();
        let trust = self.trust.clone();
        let gate = self.gate.clone();
        let peers = self.peers.clone();
        let handle = self.handle.clone();
        let run_task = self.run_task.clone();
        let jitter_ms = self.jitter_ms.clone();
        let events = self.events.clone();
        let name = self.name.clone();
        let task = tokio::spawn(relay_loop(
            name,
            addr,
            ROOM,
            SECRET,
            identity,
            vault,
            trust,
            gate,
            peers,
            handle,
            run_task,
            jitter_ms,
            events,
            stop_rx,
        ));
        self.loop_task = Some(task);
    }

    pub async fn stop(&mut self) {
        let _ = self.stop_tx.send(true);
        if let Some(t) = self.loop_task.take() {
            let _ = t.await;
        }
        self.run_task.lock().unwrap().take();
    }

    /// Simulate a process crash: abort everything, no clean leave. The
    /// server notices the socket close and drops the member.
    pub fn crash(&mut self) {
        if let Some(t) = self.loop_task.take() {
            t.abort();
        }
        if let Some(t) = self.run_task.lock().unwrap().take() {
            t.abort();
        }
        *self.handle.lock().unwrap() = None;
    }

    /// Local edit + push-on-change (mirrors the app: the save command takes
    /// the sync gate before touching the vault, so a concurrent sync's
    /// put-back can never overwrite a fresh edit).
    pub async fn edit(&self, path: &str, content: &str) {
        let _gate = self.gate.lock().await;
        self.vault.lock().unwrap().write(path, content).unwrap();
        drop(_gate);
        self.schedule_push();
    }

    pub async fn delete_note(&self, uuid: &Uuid) {
        let _gate = self.gate.lock().await;
        self.vault.lock().unwrap().delete(uuid).unwrap();
        drop(_gate);
        self.schedule_push();
    }

    fn schedule_push(&self) {
        self.change_seq.fetch_add(1, Ordering::SeqCst);
        if self.push_pending.swap(true, Ordering::SeqCst) {
            return; // a push is already pending; it reads the latest vault
        }
        let name = self.name.clone();
        let identity = self.identity.clone();
        let vault = self.vault.clone();
        let trust = self.trust.clone();
        let gate = self.gate.clone();
        let handle = self.handle.clone();
        let peers = self.peers.clone();
        let change_seq = self.change_seq.clone();
        let push_pending = self.push_pending.clone();
        let events = self.events.clone();
        tokio::spawn(push_loop(
            name,
            identity,
            vault,
            trust,
            gate,
            handle,
            peers,
            change_seq,
            push_pending,
            events,
        ));
    }

    /// Manual "sync with peer" (mirrors the app's `sync_with`): up to 3
    /// attempts, staggered retry on "busy".
    pub async fn sync_with(&self, peer: &PeerInfo) -> Result<SyncReport, String> {
        if !self.trust.is_trusted(&peer.device_id) {
            return Err(format!("device {} is not trusted", peer.name));
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            let _gate = self.gate.lock().await;
            let mut v = self.vault.lock().unwrap().clone();
            let h = self.handle.lock().unwrap().clone();
            let stream = h
                .and_then(|h| h.open_session(&peer.device_id))
                .ok_or_else(|| "relay server is not connected".to_string())?;
            match capped_initiator(stream, &self.identity, peer.x25519_pub, &mut v, SYNC_TIMEOUT).await
            {
                Ok(r) => {
                    *self.vault.lock().unwrap() = v;
                    return Ok(r);
                }
                Err(e) if is_busy_error(&e) && attempt < 3 => {
                    // release the gate during the staggered wait (see
                    // sync_vault_with_peers)
                    drop(_gate);
                    log(
                        &self.events,
                        &format!("{}: manual sync: peer busy, retry {attempt}/2", self.name),
                    );
                    tokio::time::sleep(
                        Duration::from_secs(attempt)
                            + Duration::from_millis(rand::random::<u16>() as u64 % 1000),
                    )
                    .await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Fire the auto-sync trigger right now with the current roster (test
    /// hook; the app fires it from roster events and the ticker).
    pub fn kick(&self) {
        let peers: Vec<PeerInfo> = self.peers.lock().unwrap().clone();
        spawn_auto_sync(
            self.name.clone(),
            self.identity.clone(),
            self.vault.clone(),
            self.trust.clone(),
            self.gate.clone(),
            self.handle.clone(),
            self.jitter_ms.clone(),
            self.events.clone(),
            peers,
        );
    }

    pub fn peer_of(&self, other_id: &str) -> PeerInfo {
        self.peers
            .lock()
            .unwrap()
            .clone()
            .into_iter()
            .find(|p| p.device_id == other_id)
            .unwrap_or_else(|| panic!("{} does not see {other_id} in its roster", self.name))
    }
}

/// Mirror of the app's `server_loop` (relay part).
async fn relay_loop(
    name: String,
    addr: SocketAddr,
    room: &str,
    secret: &str,
    identity: DeviceIdentity,
    vault: Arc<Mutex<Vault>>,
    trust: Arc<TrustStore>,
    gate: Arc<tokio::sync::Mutex<()>>,
    peers: Arc<Mutex<Vec<PeerInfo>>>,
    handle: Arc<Mutex<Option<ServerHandle>>>,
    run_task: Arc<Mutex<Option<tokio::task::JoinHandle<anyhow::Result<()>>>>>,
    jitter_ms: Arc<AtomicU64>,
    events: Arc<Mutex<Vec<String>>>,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut backoff = Duration::from_millis(300);
    // Dedupe state lives for the whole loop (across reconnects), like the app.
    let mut last_trusted: BTreeSet<String> = BTreeSet::new();
    loop {
        if *stop_rx.borrow() {
            return;
        }
        let res = tokio::select! {
            _ = stop_rx.changed() => return,
            r = ServerClient::connect(addr, room, secret, &identity) => r,
        };
        match res {
            Ok((client, roster)) => {
                backoff = Duration::from_millis(300);
                *handle.lock().unwrap() = Some(client.handle());
                // Initial roster: applied for the UI, but does NOT trigger a
                // sync (the joiner's own JOIN must not cause a self-race).
                *peers.lock().unwrap() = roster_to_peers(&roster);
                log(
                    &events,
                    &format!("{name}: in room '{room}', {} peer(s)", roster.len()),
                );
                let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<ServerEvent>();
                let rt = tokio::spawn(client.run(ev_tx));
                *run_task.lock().unwrap() = Some(rt);
                loop {
                    tokio::select! {
                        _ = stop_rx.changed() => {
                            if let Some(rt) = run_task.lock().unwrap().take() {
                                rt.abort();
                            }
                            *handle.lock().unwrap() = None;
                            return;
                        }
                        ev = ev_rx.recv() => match ev {
                            Some(ServerEvent::RosterUpdated(r)) => {
                                *peers.lock().unwrap() = roster_to_peers(&r);
                                let trusted: Vec<PeerInfo> = roster_to_peers(&r)
                                    .into_iter()
                                    .filter(|p| trust.is_trusted(&p.device_id))
                                    .collect();
                                let ids: BTreeSet<String> =
                                    trusted.iter().map(|p| p.device_id.clone()).collect();
                                // Only a CHANGED trusted set triggers a sync:
                                // every re-JOIN re-broadcasts the full roster,
                                // and re-triggering on unchanged rosters just
                                // creates collision opportunities.
                                if ids != last_trusted {
                                    last_trusted = ids;
                                    if !trusted.is_empty() {
                                        log(&events, &format!("{name}: roster changed -> auto-sync"));
                                        spawn_auto_sync(
                                            name.clone(),
                                            identity.clone(),
                                            vault.clone(),
                                            trust.clone(),
                                            gate.clone(),
                                            handle.clone(),
                                            jitter_ms.clone(),
                                            events.clone(),
                                            trusted,
                                        );
                                    }
                                }
                            }
                            Some(ServerEvent::Incoming { devid, stream }) => {
                                spawn_responder(
                                    name.clone(),
                                    identity.clone(),
                                    vault.clone(),
                                    trust.clone(),
                                    gate.clone(),
                                    events.clone(),
                                    devid,
                                    stream,
                                );
                            }
                            None => break, // relay connection lost
                        },
                    }
                }
                let rt = {
                    let mut g = run_task.lock().unwrap();
                    g.take()
                };
                if let Some(rt) = rt {
                    let _ = rt.await;
                }
                *handle.lock().unwrap() = None;
                *peers.lock().unwrap() = Vec::new();
                log(&events, &format!("{name}: relay connection lost, reconnecting"));
            }
            Err(e) => {
                log(&events, &format!("{name}: relay connect failed: {e}"));
                tokio::select! {
                    _ = stop_rx.changed() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(2));
            }
        }
    }
}

/// Mirror of the app's `spawn_server_responder`.
fn spawn_responder(
    name: String,
    identity: DeviceIdentity,
    vault: Arc<Mutex<Vault>>,
    trust: Arc<TrustStore>,
    gate: Arc<tokio::sync::Mutex<()>>,
    events: Arc<Mutex<Vec<String>>>,
    devid: String,
    stream: RelayStream,
) {
    tokio::spawn(async move {
        let Ok(_gate) = gate.try_lock() else {
            // Busy: our own initiator (or another sync) holds the gate.
            log(&events, &format!("{name}: busy-reject session from {devid}"));
            let _ = reject_busy(stream, &identity).await;
            return;
        };
        let mut v = vault.lock().unwrap().clone();
        if let Err(e) = v.scan_and_repair() {
            log(&events, &format!("{name}: pre-sync scan: {e}"));
        }
        match capped_responder(stream, &identity, &mut v, &trust, SYNC_TIMEOUT).await {
            Ok(r) => {
                log(
                    &events,
                    &format!(
                        "{name}: responder {}: sent {} applied {} skipped {}",
                        r.peer_name, r.sent, r.applied, r.skipped
                    ),
                );
                *vault.lock().unwrap() = v;
            }
            Err(e) => log(&events, &format!("{name}: responder {devid} failed: {e}")),
        }
    });
}

/// Mirror of the app's `spawn_auto_sync`.
fn spawn_auto_sync(
    name: String,
    identity: DeviceIdentity,
    vault: Arc<Mutex<Vault>>,
    trust: Arc<TrustStore>,
    gate: Arc<tokio::sync::Mutex<()>>,
    handle: Arc<Mutex<Option<ServerHandle>>>,
    jitter_ms: Arc<AtomicU64>,
    events: Arc<Mutex<Vec<String>>>,
    peers: Vec<PeerInfo>,
) {
    if peers.is_empty() {
        return;
    }
    tokio::spawn(async move {
        // Random 0..jitter stagger (the app: 0..1500ms). When the same
        // roster broadcast reaches both devices, the stagger lets one side
        // answer as the responder.
        let jitter =
            Duration::from_millis(rand::random::<u64>() % jitter_ms.load(Ordering::SeqCst).max(1));
        tokio::time::sleep(jitter).await;
        let reports =
            sync_vault_with_peers(&name, &identity, &trust, &gate, &vault, &handle, &events, &peers)
                .await;
        for r in &reports {
            log(
                &events,
                &format!(
                    "{name}: auto-sync {}: sent {} applied {} skipped {}",
                    r.peer_name, r.sent, r.applied, r.skipped
                ),
            );
        }
    });
}

/// Mirror of the app's `sync_vault_with_peers` (relay peers).
///
/// Gate discipline: the gate is acquired per attempt and RELEASED during the
/// busy-retry waits. Holding it while sleeping would make our own retry hit
/// the peer's still-held gate (and vice versa) — a collision chain that
/// keeps both devices diverged for a whole ticker period.
async fn sync_vault_with_peers(
    name: &str,
    identity: &DeviceIdentity,
    trust: &TrustStore,
    gate: &Arc<tokio::sync::Mutex<()>>,
    vault: &Arc<Mutex<Vault>>,
    handle: &Arc<Mutex<Option<ServerHandle>>>,
    events: &Arc<Mutex<Vec<String>>>,
    peers: &[PeerInfo],
) -> Vec<SyncReport> {
    let mut out = Vec::new();
    for p in peers {
        if !trust.is_trusted(&p.device_id) {
            continue;
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            let _gate = gate.lock().await;
            let mut v = vault.lock().unwrap().clone();
            if let Err(e) = v.scan_and_repair() {
                log(events, &format!("{name}: pre-sync scan: {e}"));
            }
            let h = handle.lock().unwrap().clone();
            let res = match h.and_then(|h| h.open_session(&p.device_id)) {
                None => {
                    log(events, &format!("{name}: relay not connected, skipping {}", p.name));
                    break;
                }
                Some(stream) => capped_initiator(stream, identity, p.x25519_pub, &mut v, SYNC_TIMEOUT).await,
            };
            match res {
                Ok(r) => {
                    *vault.lock().unwrap() = v;
                    out.push(r);
                    break;
                }
                // The peer is mid-sync with someone else and answered fast
                // with "busy": drop the gate, wait a staggered (and
                // increasing) moment, retry. Up to 3 attempts: with the
                // gate released during the waits, the staggered retries
                // break the symmetry even when both sides retry at exactly
                // the same instant.
                Err(e) if is_busy_error(&e) && attempt <= 2 => {
                    drop(_gate);
                    log(events, &format!("{name}: {} busy, attempt {attempt}/3", p.name));
                    tokio::time::sleep(Duration::from_millis(
                        1000 * attempt + rand::random::<u16>() as u64 % 1000,
                    ))
                    .await;
                }
                Err(e) => {
                    log(events, &format!("{name}: sync with {} failed: {e}", p.name));
                    break;
                }
            }
        }
    }
    out
}

/// Mirror of the app's `schedule_push` background loop.
async fn push_loop(
    name: String,
    identity: DeviceIdentity,
    vault: Arc<Mutex<Vault>>,
    trust: Arc<TrustStore>,
    gate: Arc<tokio::sync::Mutex<()>>,
    handle: Arc<Mutex<Option<ServerHandle>>>,
    peers: Arc<Mutex<Vec<PeerInfo>>>,
    change_seq: Arc<AtomicU64>,
    push_pending: Arc<AtomicBool>,
    events: Arc<Mutex<Vec<String>>>,
) {
    loop {
        tokio::time::sleep(PUSH_DEBOUNCE).await;
        let before = change_seq.load(Ordering::SeqCst);
        let ps: Vec<PeerInfo> = peers.lock().unwrap().clone();
        let reports =
            sync_vault_with_peers(&name, &identity, &trust, &gate, &vault, &handle, &events, &ps)
                .await;
        for r in &reports {
            log(
                &events,
                &format!(
                    "{name}: push {}: sent {} applied {} skipped {}",
                    r.peer_name, r.sent, r.applied, r.skipped
                ),
            );
        }
        let after = change_seq.load(Ordering::SeqCst);
        push_pending.store(false, Ordering::SeqCst);
        if after == before {
            break;
        }
        // edits landed mid-push: loop to push them
        push_pending.store(true, Ordering::SeqCst);
    }
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn push_on_change_over_relay() {
    let (addr, _srv, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("a");
    let dir_b = tmp_dir("b");
    let mut a = Node::new("alpha", dir_a.clone()).unwrap();
    let mut b = Node::new("beta", dir_b.clone()).unwrap();
    a.trust(&b);
    b.trust(&a);
    a.start(addr).await;
    assert_waited(
        wait_until(|| a.handle.lock().unwrap().is_some(), Duration::from_secs(5)).await,
        "A in room",
    );
    b.start(addr).await;
    assert_waited(
        wait_until(
            || {
                b.handle.lock().unwrap().is_some()
                    && a.peers.lock().unwrap().iter().any(|p| p.device_id == b.id)
            },
            Duration::from_secs(5),
        )
        .await,
        "B in room, A sees B",
    );

    // A edits; the push-on-change (100ms debounce) must reach B.
    a.edit("hello.md", "synced over relay").await;
    assert_waited(
        wait_until(
            || snapshot(&b.vault.lock().unwrap()).contains_key("hello.md"),
            Duration::from_secs(15),
        )
        .await,
        "hello.md pushed to B",
    );
    let sb = snapshot(&b.vault.lock().unwrap());
    let (uuid, content) = &sb["hello.md"];
    assert!(
        content.contains("synced over relay"),
        "content mismatch: {content:?}"
    );
    assert_eq!(
        a.vault.lock().unwrap().get(uuid).unwrap().uuid, *uuid,
        "B must store A's uuid (same note, not a copy)"
    );

    a.stop().await;
    b.stop().await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// A single device in a room must NEVER sync with itself. Regression: the
/// old server included a member in its own roster broadcast, and with a
/// trust list contaminated by the device's own id (easy to do — both
/// devices list as the same name) every auto-sync round became a handshake
/// towards itself: msg1 out, the server routes it back, the initiator reads
/// its own 32-byte frame as the peer's reply and fails. The device then
/// spends every tick "syncing" with nobody, and — if the real peer is not
/// in the trust list — nothing ever syncs.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn single_device_room_no_self_sync() {
    let (addr, server, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("sd");
    let mut a = Node::new("solo", dir_a.clone()).unwrap();
    a.trust_self(); // worst case: own id in the trust list
    a.start(addr).await;
    assert_waited(
        wait_until(
            || server
                .room_lines()
                .iter()
                .any(|l| l.contains(&format!("{ROOM}: 1 member"))),
            Duration::from_secs(5),
        )
        .await,
        "solo node in room",
    );

    // Let several JOIN broadcasts / trigger windows pass.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let peers = a.peers.lock().unwrap().clone();
    assert!(
        !peers.iter().any(|p| p.device_id == a.id),
        "A sees itself as a peer: {peers:?}"
    );
    // Nothing may have been relayed: a self-sync would have sent a Noise
    // msg1 (32 B) through the server.
    let stats = server.stats_line();
    assert!(
        stats.contains("relayed 0 frames"),
        "a self-sync went through the relay: {stats}"
    );
    let evs = a.events.lock().unwrap().clone();
    assert!(
        !evs.iter().any(|e| e.contains("auto-sync") || e.contains("responder")),
        "A attempted a sync: {evs:?}"
    );

    a.stop().await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
}

/// THE regression test for the cross-device attractor: both devices start at
/// the same moment (each JOIN re-broadcasts the roster to both), both fire
/// the auto-sync trigger at the same instant (jitter forced to 0), both
/// become initiators. The old code deadlocked here forever; now each side
/// gets a fast "busy" answer, retries, and the round completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn simultaneous_startup_collisions_resolve() {
    let (addr, _srv, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("sa");
    let dir_b = tmp_dir("sb");
    let mut a = Node::new("alpha", dir_a.clone()).unwrap();
    let mut b = Node::new("beta", dir_b.clone()).unwrap();
    // Each device has a local note BEFORE starting.
    a.vault.lock().unwrap().write("only-a.md", "A note").unwrap();
    b.vault.lock().unwrap().write("only-b.md", "B note").unwrap();
    a.trust(&b);
    b.trust(&a);
    // Worst case: no stagger, triggers fire at the same instant.
    a.set_jitter_ms(0);
    b.set_jitter_ms(0);

    tokio::join!(a.start(addr), b.start(addr));

    // Despite the collision window, both notes must be on both sides.
    wait_converged(&a, &b, Duration::from_secs(25)).await;
    let sa = snapshot(&a.vault.lock().unwrap());
    assert_eq!(sa.len(), 2, "both notes on A: {sa:?}");
    let sb = snapshot(&b.vault.lock().unwrap());
    assert_eq!(sb.len(), 2, "both notes on B: {sb:?}");
    assert_eq!(sa, sb);

    // And the gates must be free (no stuck sync holding them).
    assert!(
        tokio::time::timeout(Duration::from_secs(2), a.gate.lock()).await.is_ok(),
        "A's gate is stuck"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), b.gate.lock()).await.is_ok(),
        "B's gate is stuck"
    );

    a.stop().await;
    b.stop().await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Repeated forced collisions (jitter 0, both sides kicked at the same
/// instant, 5 rounds): every round must converge, and the gates must never
/// stay held.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn forced_collisions_resolve() {
    let (addr, _srv, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("fc-a");
    let dir_b = tmp_dir("fc-b");
    let mut a = Node::new("alpha", dir_a.clone()).unwrap();
    let mut b = Node::new("beta", dir_b.clone()).unwrap();
    a.trust(&b);
    b.trust(&a);
    a.set_jitter_ms(0);
    b.set_jitter_ms(0);
    a.start(addr).await;
    b.start(addr).await;
    assert_waited(
        wait_until(
            || {
                a.peers.lock().unwrap().iter().any(|p| p.device_id == b.id)
                    && b.peers.lock().unwrap().iter().any(|p| p.device_id == a.id)
            },
            Duration::from_secs(5),
        )
        .await,
        "mutual visibility",
    );

    for round in 1..=5 {
        a.edit(&format!("a-{round}.md"), &format!("A {round}")).await;
        b.edit(&format!("b-{round}.md"), &format!("B {round}")).await;
        // Both auto-sync triggers at the same instant: both become
        // initiators; each must survive the other's "busy" and the round
        // must still complete.
        a.kick();
        b.kick();
        wait_converged(&a, &b, Duration::from_secs(20)).await;
    }

    let sa = snapshot(&a.vault.lock().unwrap());
    assert_eq!(sa.len(), 10, "all 10 notes on A: {sa:?}");
    assert_eq!(sa, snapshot(&b.vault.lock().unwrap()));
    assert!(
        tokio::time::timeout(Duration::from_secs(2), a.gate.lock()).await.is_ok(),
        "A's gate stuck after collisions"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), b.gate.lock()).await.is_ok(),
        "B's gate stuck after collisions"
    );

    a.stop().await;
    b.stop().await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Manual "sync with peer" while the peer's gate is held by its own sync:
/// the peer answers "busy", the manual sync retries (staggered) and
/// succeeds once the peer is free — instead of erroring out or hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn manual_sync_retries_through_busy() {
    let (addr, _srv, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("mb-a");
    let dir_b = tmp_dir("mb-b");
    let mut a = Node::new("alpha", dir_a.clone()).unwrap();
    let mut b = Node::new("beta", dir_b.clone()).unwrap();
    a.trust(&b);
    b.trust(&a);
    a.start(addr).await;
    b.start(addr).await;
    assert_waited(
        wait_until(
            || a.peers.lock().unwrap().iter().any(|p| p.device_id == b.id),
            Duration::from_secs(5),
        )
        .await,
        "A sees B",
    );

    // Hold B's gate for 2.5s (B is "syncing with someone else").
    let gate_b = b.gate.clone();
    let holder = tokio::spawn(async move {
        let _g = gate_b.lock().await;
        tokio::time::sleep(Duration::from_millis(2500)).await;
    });

    let t0 = Instant::now();
    let res = a.sync_with(&a.peer_of(&b.id)).await;
    let elapsed = t0.elapsed();
    let _ = holder.await;

    assert!(res.is_ok(), "manual sync must succeed after busy retries: {res:?}");
    assert!(
        elapsed < Duration::from_secs(15),
        "manual sync took {elapsed:?} (busy retries should land within a few seconds)"
    );

    a.stop().await;
    b.stop().await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// A peer crashes (no clean leave). The server must drop it from the room
/// promptly (socket close); the other side keeps working; when the peer
/// comes back (same identity, vault re-opened from disk) everything
/// re-converges.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn peer_crash_and_restart() {
    let (addr, server, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("cr-a");
    let dir_b = tmp_dir("cr-b");
    let mut a = Node::new("alpha", dir_a.clone()).unwrap();
    let mut b = Node::new("beta", dir_b.clone()).unwrap();
    a.trust(&b);
    b.trust(&a);
    a.vault.lock().unwrap().write("pre-a.md", "A pre").unwrap();
    b.vault.lock().unwrap().write("pre-b.md", "B pre").unwrap();
    a.start(addr).await;
    b.start(addr).await;
    wait_converged(&a, &b, Duration::from_secs(20)).await;

    // B crashes hard: no leave, just a dead socket.
    b.crash();
    assert_waited(
        wait_until(
            || server
                .room_lines()
                .iter()
                .any(|l| l.contains(&format!("{ROOM}: 1 member"))),
            Duration::from_secs(10),
        )
        .await,
        "server drops the crashed member",
    );

    // A keeps working in the meantime.
    a.edit("a-after.md", "A after restart").await;

    // B comes back: same identity (from disk), vault re-opened from disk.
    b.start(addr).await;
    b.edit("b-after.md", "B after restart").await;
    wait_converged(&a, &b, Duration::from_secs(25)).await;

    let sa = snapshot(&a.vault.lock().unwrap());
    let sb = snapshot(&b.vault.lock().unwrap());
    for key in ["pre-a.md", "pre-b.md", "a-after.md", "b-after.md"] {
        assert!(sa.contains_key(key), "A lost {key}: {sa:?}");
        assert!(sb.contains_key(key), "B lost {key}: {sb:?}");
    }

    a.stop().await;
    b.stop().await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// The relay server itself dies (and is restarted on the same address):
/// both nodes must notice the loss, reconnect, re-JOIN, and sync again.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn relay_restart_recovers() {
    let (addr, _srv, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("rr-a");
    let dir_b = tmp_dir("rr-b");
    let mut a = Node::new("alpha", dir_a.clone()).unwrap();
    let mut b = Node::new("beta", dir_b.clone()).unwrap();
    a.trust(&b);
    b.trust(&a);
    a.start(addr).await;
    b.start(addr).await;
    assert_waited(
        wait_until(
            || a.peers.lock().unwrap().iter().any(|p| p.device_id == b.id),
            Duration::from_secs(5),
        )
        .await,
        "A sees B",
    );
    a.edit("before.md", "before restart").await;
    wait_converged(&a, &b, Duration::from_secs(20)).await;

    // Kill the relay server.
    server_task.abort();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (_srv2, server_task2) = restart_server_on(addr, SECRET).await;

    // A new edit must reach B through the re-established relay.
    a.edit("post-restart.md", "survived the relay restart").await;
    assert_waited(
        wait_until(
            || snapshot(&b.vault.lock().unwrap()).contains_key("post-restart.md"),
            Duration::from_secs(25),
        )
        .await,
        "post-restart note reached B",
    );

    a.stop().await;
    b.stop().await;
    server_task2.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Regression: a transient server `Err` frame ("peer not in room" for a
/// session towards a device the server no longer knows) must NOT tear down
/// the whole relay connection — the offending session simply times out,
/// and the connection keeps working for real peers.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn err_frame_does_not_kill_connection() {
    let (addr, _srv, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("ef-a");
    let dir_b = tmp_dir("ef-b");
    let mut a = Node::new("alpha", dir_a.clone()).unwrap();
    let mut b = Node::new("beta", dir_b.clone()).unwrap();
    a.trust(&b);
    b.trust(&a);
    a.start(addr).await;
    b.start(addr).await;
    assert_waited(
        wait_until(
            || a.peers.lock().unwrap().iter().any(|p| p.device_id == b.id),
            Duration::from_secs(5),
        )
        .await,
        "A sees B",
    );

    // Open a session towards a device that is NOT in the room: the server
    // answers with a one-off Err frame.
    let h = a.handle.lock().unwrap().clone().expect("A connected");
    let mut ghost = h
        .open_session("ghost-device-0000")
        .expect("open ghost session");
    ghost.write_all(b"x").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await; // let the Err arrive

    assert!(
        a.handle.lock().unwrap().is_some(),
        "relay connection must survive a transient Err frame"
    );
    // And a real sync still works on the same connection.
    a.edit("after-err.md", "still alive").await;
    assert_waited(
        wait_until(
            || snapshot(&b.vault.lock().unwrap()).contains_key("after-err.md"),
            Duration::from_secs(15),
        )
        .await,
        "sync still works after the Err frame",
    );

    a.stop().await;
    b.stop().await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Chaos: 8 rounds of edits + a mix of manual syncs and simultaneous kicks
/// (jitter 0, so collisions are forced), plus a deletion. Every manual call
/// must return (no hang), the gates must never be starved, and the vaults
/// must end up identical.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn hammer_mixed_syncs_converge_no_starvation() {
    let (addr, _srv, server_task) = start_server(SECRET).await;
    let dir_a = tmp_dir("hm-a");
    let dir_b = tmp_dir("hm-b");
    let mut a = Node::new("alpha", dir_a.clone()).unwrap();
    let mut b = Node::new("beta", dir_b.clone()).unwrap();
    a.trust(&b);
    b.trust(&a);
    a.set_jitter_ms(0);
    b.set_jitter_ms(0);
    a.start(addr).await;
    b.start(addr).await;
    assert_waited(
        wait_until(
            || {
                a.peers.lock().unwrap().iter().any(|p| p.device_id == b.id)
                    && b.peers.lock().unwrap().iter().any(|p| p.device_id == a.id)
            },
            Duration::from_secs(5),
        )
        .await,
        "mutual visibility",
    );

    // Gate-hold probes: fail the test if either gate is ever held > 12s.
    let max_hold_a = Arc::new(AtomicU64::new(0));
    let max_hold_b = Arc::new(AtomicU64::new(0));
    let probe_deadline = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let probe_a = {
        let gate = a.gate.clone();
        let max = max_hold_a.clone();
        let stop = probe_deadline.clone();
        tokio::spawn(async move {
            gate_probe("A", gate, max, stop).await
        })
    };
    let probe_b = {
        let gate = b.gate.clone();
        let max = max_hold_b.clone();
        let stop = probe_deadline.clone();
        tokio::spawn(async move {
            gate_probe("B", gate, max, stop).await
        })
    };

    let mut manual_results = Vec::new();
    for round in 0..8u32 {
        let path = format!("note-{round}.md");
        if round % 2 == 0 {
            a.edit(&path, &format!("from A {round}")).await;
        } else {
            b.edit(&path, &format!("from B {round}")).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        if round % 2 == 0 {
            let res = a.sync_with(&a.peer_of(&b.id)).await;
            manual_results.push((round, "A->B", res.is_ok()));
        } else {
            let res = b.sync_with(&b.peer_of(&a.id)).await;
            manual_results.push((round, "B->A", res.is_ok()));
        }
        // Every third round: force another collision on top.
        if round % 3 == 1 {
            a.kick();
            b.kick();
        }
        // Round 6: delete the alphabetically first note on A.
        if round == 6 {
            let sa = snapshot(&a.vault.lock().unwrap());
            if let Some((_, (uuid, _))) = sa.iter().next() {
                a.delete_note(uuid).await;
            }
        }
    }

    probe_deadline.store(true, Ordering::SeqCst);
    let _ = probe_a.await;
    let _ = probe_b.await;

    assert!(
        manual_results.iter().all(|(_, _, ok)| *ok),
        "a manual sync failed or hung: {manual_results:?}"
    );
    wait_converged(&a, &b, Duration::from_secs(25)).await;
    let sa = snapshot(&a.vault.lock().unwrap());
    // 8 notes created, 1 deleted on round 6 -> 7
    assert_eq!(sa.len(), 7, "expected 7 notes, got {sa:?}");
    assert_eq!(sa, snapshot(&b.vault.lock().unwrap()));
    assert!(
        max_hold_a.load(Ordering::SeqCst) < 12_000,
        "A's gate was held {}ms (starvation)",
        max_hold_a.load(Ordering::SeqCst)
    );
    assert!(
        max_hold_b.load(Ordering::SeqCst) < 12_000,
        "B's gate was held {}ms (starvation)",
        max_hold_b.load(Ordering::SeqCst)
    );

    a.stop().await;
    b.stop().await;
    server_task.abort();
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

/// Probe: repeatedly acquire the gate; if a single acquisition ever waits
/// more than 12s, record starvation (u64::MAX) and stop.
async fn gate_probe(
    name: &str,
    gate: Arc<tokio::sync::Mutex<()>>,
    max_hold: Arc<AtomicU64>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let t0 = Instant::now();
        match tokio::time::timeout(Duration::from_secs(12), gate.lock()).await {
            Ok(_g) => {
                let held = t0.elapsed().as_millis() as u64;
                let mut cur = max_hold.load(Ordering::SeqCst);
                while held > cur {
                    match max_hold.compare_exchange_weak(cur, held, Ordering::SeqCst, Ordering::SeqCst) {
                        Ok(_) => break,
                        Err(c) => cur = c,
                    }
                }
            }
            Err(_) => {
                max_hold.store(u64::MAX, Ordering::SeqCst);
                tracing::error!("{name}: gate probe gave up after 12s");
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
