//! SpiderNotes relay server.
//!
//! A stateless TCP relay with dynamic rooms:
//! - ONE global secret (configured at startup) gates all access. It is
//!   proved with an HMAC challenge and is never transmitted.
//! - Rooms are created by the FIRST client that joins a room name and
//!   deleted automatically when the last member leaves. Clients in the same
//!   room see each other's roster; nothing is shared between rooms.
//! - Sync frames are end-to-end encrypted (Noise_XK) between clients; the
//!   server only relays opaque ciphertext and counts bytes.
//! - A local control socket (line protocol over TCP) answers `help`,
//!   `rooms`, `members [room]`, `stats`, `quit` — for inspecting what is
//!   happening on the server.
//!
//! Logging: join/leave/room lifecycle/auth failures at INFO (default),
//! per-frame relay details at DEBUG (`RUST_LOG=debug`).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use spidernotes_p2p::relay::{
    random_nonce, read_frame, verify_room_proof, write_frame, encode, RelayMsg, RosterPeer,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

pub struct Member {
    pub name: String,
    pub x25519_pub: [u8; 32],
    pub addr: SocketAddr,
    pub joined_at: SystemTime,
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    /// Feeds this member's socket writer task.
    tx: mpsc::UnboundedSender<Vec<u8>>,
}

struct Room {
    created_at: SystemTime,
    members: HashMap<String, Member>,
}

#[derive(Default)]
struct Stats {
    connections: AtomicU64,
    auth_failures: AtomicU64,
    frames_relayed: AtomicU64,
    bytes_relayed: AtomicU64,
    ctrl_queries: AtomicU64,
}

#[derive(Clone)]
pub struct Server {
    secret: Arc<String>,
    rooms: Arc<Mutex<HashMap<String, Room>>>,
    stats: Arc<Stats>,
}

impl Server {
    pub fn new(secret: String) -> Self {
        Self {
            secret: Arc::new(secret),
            rooms: Arc::new(Mutex::new(HashMap::new())),
            stats: Arc::new(Stats::default()),
        }
    }

    /// Data accept loop; runs until the listener errors.
    pub async fn run(self, listener: TcpListener) -> anyhow::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let _ = stream.set_nodelay(true);
            let s = self.clone();
            self.stats.connections.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                if let Err(e) = s.handle(stream, peer).await {
                    tracing::debug!("data connection {peer} ended: {e}");
                }
            });
        }
    }

    /// Control accept loop (inspection socket); runs until it errors.
    pub async fn run_ctrl(self, listener: TcpListener) -> anyhow::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let s = self.clone();
            tokio::spawn(async move {
                if let Err(e) = s.handle_ctrl(stream, peer).await {
                    tracing::debug!("ctrl connection {peer} ended: {e}");
                }
            });
        }
    }

    // ---------- inspection (used by the control socket and tests) ----------

    /// One line per room: name, member count, age.
    pub fn room_lines(&self) -> Vec<String> {
        let rooms = self.rooms.lock().unwrap();
        let now = SystemTime::now();
        let mut names: Vec<&String> = rooms.keys().collect();
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let r = &rooms[n];
                format!(
                    "{n}: {} member(s), created {} ago",
                    r.members.len(),
                    humanize(now.duration_since(r.created_at).unwrap_or_default())
                )
            })
            .collect()
    }

    /// One line per member of a room (or all rooms when `room` is None).
    pub fn member_lines(&self, room: Option<&str>) -> Vec<String> {
        let rooms = self.rooms.lock().unwrap();
        let now = SystemTime::now();
        let mut out = Vec::new();
        let mut names: Vec<&String> = rooms.keys().collect();
        names.sort();
        for n in names {
            if let Some(want) = room {
                if n != want {
                    continue;
                }
            }
            let r = &rooms[n];
            let mut ids: Vec<&String> = r.members.keys().collect();
            ids.sort();
            for d in ids {
                let m = &r.members[d];
                out.push(format!(
                    "{n}: {} [{}] @ {} — joined {} ago, {} frames, {} B",
                    m.name,
                    short_id(d),
                    m.addr,
                    humanize(now.duration_since(m.joined_at).unwrap_or_default()),
                    m.frames.load(Ordering::Relaxed),
                    m.bytes.load(Ordering::Relaxed),
                ));
            }
        }
        out
    }

    pub fn stats_line(&self) -> String {
        let s = &self.stats;
        format!(
            "connections {} (auth failures {}), relayed {} frames / {} B, ctrl queries {}",
            s.connections.load(Ordering::Relaxed),
            s.auth_failures.load(Ordering::Relaxed),
            s.frames_relayed.load(Ordering::Relaxed),
            s.bytes_relayed.load(Ordering::Relaxed),
            s.ctrl_queries.load(Ordering::Relaxed),
        )
    }

    /// Answer one control-socket command. Empty string = close the session.
    pub fn ctrl_command(&self, cmd: &str) -> String {
        self.stats.ctrl_queries.fetch_add(1, Ordering::Relaxed);
        let mut parts = cmd.split_whitespace();
        match parts.next() {
            Some("help") => {
                "commands: help, rooms, members [room], stats, quit\n".to_string()
            }
            Some("rooms") => {
                let lines = self.room_lines();
                if lines.is_empty() {
                    "(no rooms)\n".to_string()
                } else {
                    lines.join("\n") + "\n"
                }
            }
            Some("members") => {
                let want = parts.next();
                if let Some(w) = want {
                    if !self.rooms.lock().unwrap().contains_key(w) {
                        return format!("no such room: {w}\n");
                    }
                }
                let lines = self.member_lines(want);
                if lines.is_empty() {
                    "(no members)\n".to_string()
                } else {
                    lines.join("\n") + "\n"
                }
            }
            Some("stats") => self.stats_line() + "\n",
            Some("quit") | Some("") => String::new(),
            Some(other) => format!("unknown command {other:?} (try help)\n"),
            None => String::new(),
        }
    }

    // ---------- data connection ----------

    /// One client connection: join handshake + frame routing until drop.
    async fn handle(&self, stream: TcpStream, peer: SocketAddr) -> anyhow::Result<()> {
        let (rd, wr) = stream.into_split();
        let mut rd = rd;
        let mut wr = wr;

        // JOIN
        let frame = read_frame(&mut rd).await?;
        let RelayMsg::Join { room, device_id, name, x25519_pub, nonce: nc } =
            decode_frame(&frame)?
        else {
            anyhow::bail!("expected join");
        };

        // CHALLENGE + PROOF against the GLOBAL secret (never transmitted)
        let ns = random_nonce();
        write_frame(&mut wr, &RelayMsg::Challenge { nonce: ns }).await?;
        let frame = read_frame(&mut rd).await?;
        let RelayMsg::Proof { mac } = decode_frame(&frame)? else {
            anyhow::bail!("expected proof");
        };
        if !verify_room_proof(&self.secret, &room, &nc, &ns, &mac) {
            self.stats.auth_failures.fetch_add(1, Ordering::Relaxed);
            tracing::warn!("room {room}: auth FAILED from {peer} (bad secret)");
            let _ = write_frame(
                &mut wr,
                &RelayMsg::Err { message: "bad secret".into() },
            )
            .await;
            anyhow::bail!("bad secret from {peer}");
        }

        // everything the server sends to this member (roster updates, routed
        // frames, errors) goes through its tx; the writer task drains it to
        // the socket AFTER the welcome is sent
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        // get-or-create the room; a device may only sit in one room at a time
        let (created, roster) = {
            let mut rooms = self.rooms.lock().unwrap();
            if let Some(old) = rooms
                .iter()
                .find(|(_, r)| r.members.contains_key(&device_id))
                .map(|(n, _)| n.clone())
            {
                rooms.get_mut(&old).unwrap().members.remove(&device_id);
                tracing::info!("{name} ({device_id}) @ {peer} moved: room {old} -> {room}");
                maybe_drop_room(&mut rooms, &old);
                broadcast_locked(&rooms, &old);
            }
            let created = !rooms.contains_key(&room);
            let r = rooms.entry(room.clone()).or_insert_with(|| {
                Room { created_at: SystemTime::now(), members: HashMap::new() }
            });
            r.members.insert(
                device_id.clone(),
                Member {
                    name: name.clone(),
                    x25519_pub,
                    addr: peer,
                    joined_at: SystemTime::now(),
                    frames: AtomicU64::new(0),
                    bytes: AtomicU64::new(0),
                    tx: out_tx.clone(),
                },
            );
            let roster = roster_locked(&rooms, &room, &device_id);
            broadcast_locked(&rooms, &room);
            (created, roster)
        };
        tracing::info!(
            "room {room}: {} ({}) @ {peer} joined ({})",
            name,
            short_id(&device_id),
            if created { "new room" } else { "existing room" }
        );
        if write_frame(&mut wr, &RelayMsg::Ok { roster }).await.is_err() {
            self.leave(&room, &device_id, 0, 0);
            return Ok(());
        }
        // A write failure means the peer's socket is gone; without acting on
        // it the member entry (and its roster visibility) would linger until
        // the read side eventually errors — on a half-open TCP that can take
        // hours. Remove the member and tell the read loop to stop.
        let (writer_done_tx, mut writer_done_rx) = tokio::sync::oneshot::channel::<()>();
        let writer = tokio::spawn({
            let s = self.clone();
            let room = room.clone();
            let device_id = device_id.clone();
            async move {
                while let Some(bytes) = out_rx.recv().await {
                    if wr.write_all(&bytes).await.is_err() {
                        let (frames, bytes) = s.member_counters(&room, &device_id);
                        s.leave(&room, &device_id, frames, bytes);
                        let _ = writer_done_tx.send(());
                        break;
                    }
                }
            }
        });

        // route: socket in -> members; errors -> own socket via out_tx
        loop {
            let frame = tokio::select! {
                frame = read_frame(&mut rd) => match frame {
                    Ok(f) => f,
                    Err(_) => break,
                },
                _ = &mut writer_done_rx => break,
            };
            let decoded = match decode_frame(&frame) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let RelayMsg::Data { to, session, payload } = decoded else {
                continue;
            };
            let target = {
                let rooms = self.rooms.lock().unwrap();
                rooms
                    .iter()
                    .find_map(|(rn, r)| {
                        r.members.get(&to).map(|m| {
                            (m.tx.clone(), rn.clone(), m.name.clone())
                        })
                    })
            };
            match target {
                Some((t, target_room, target_name)) => {
                    let _ = t.send(encode(&RelayMsg::Data {
                        to: device_id.clone(),
                        session,
                        payload: payload.clone(),
                    }));
                    self.stats.frames_relayed.fetch_add(1, Ordering::Relaxed);
                    self.stats
                        .bytes_relayed
                        .fetch_add(payload.len() as u64, Ordering::Relaxed);
                    {
                        let rooms = self.rooms.lock().unwrap();
                        if let Some(r) = rooms.get(&target_room) {
                            if let Some(m) = r.members.get(&to) {
                                m.frames.fetch_add(1, Ordering::Relaxed);
                                m.bytes.fetch_add(payload.len() as u64, Ordering::Relaxed);
                            }
                        }
                    }
                    tracing::debug!(
                        "room {room}: relayed {} B -> {target_name} ({to})",
                        payload.len()
                    );
                }
                None => {
                    let _ = out_tx.send(encode(&RelayMsg::Err {
                        message: "peer not in room".into(),
                    }));
                }
            }
        }

        let (frames, bytes) = self.member_counters(&room, &device_id);
        self.leave(&room, &device_id, frames, bytes);
        writer.abort();
        Ok(())
    }

    /// A member's relay counters `(0, 0)` when it is not in the room.
    fn member_counters(&self, room: &str, device_id: &str) -> (u64, u64) {
        let rooms = self.rooms.lock().unwrap();
        rooms
            .get(room)
            .and_then(|r| r.members.get(device_id))
            .map(|m| (
                m.frames.load(Ordering::Relaxed),
                m.bytes.load(Ordering::Relaxed),
            ))
            .unwrap_or((0, 0))
    }

    /// Remove a member; delete the room when it becomes empty.
    fn leave(&self, room: &str, device_id: &str, frames: u64, bytes: u64) {
        {
            let mut rooms = self.rooms.lock().unwrap();
            let name = rooms
                .get(room)
                .and_then(|r| r.members.get(device_id))
                .map(|m| m.name.clone());
            if rooms.get_mut(room).and_then(|r| r.members.remove(device_id)).is_some() {
                if let Some(n) = name {
                    tracing::info!(
                        "room {room}: {n} ({}) left ({frames} frames, {bytes} B relayed)",
                        short_id(device_id)
                    );
                }
                maybe_drop_room(&mut rooms, room);
                broadcast_locked(&rooms, room);
            }
        }
    }

    // ---------- control connection ----------

    async fn handle_ctrl(&self, stream: TcpStream, peer: SocketAddr) -> std::io::Result<()> {
        let (rd, mut wr) = stream.into_split();
        let mut reader = BufReader::new(rd);
        tracing::info!("ctrl: session from {peer}");
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let cmd = line.trim();
                    if cmd.is_empty() {
                        continue;
                    }
                    tracing::info!("ctrl: {peer} > {cmd}");
                    let resp = self.ctrl_command(cmd);
                    if resp.is_empty() {
                        break;
                    }
                    wr.write_all(resp.as_bytes()).await?;
                }
                Err(e) => return Err(e),
            }
        }
        tracing::info!("ctrl: session from {peer} closed");
        Ok(())
    }
}

/// Delete a room when it is empty.
fn maybe_drop_room(rooms: &mut HashMap<String, Room>, room: &str) {
    if rooms.get(room).map(|r| r.members.is_empty()).unwrap_or(false) {
        tracing::info!("room {room} deleted (no members left)");
        rooms.remove(room);
    }
}

fn roster_locked(
    rooms: &HashMap<String, Room>,
    room: &str,
    except: &str,
) -> Vec<RosterPeer> {
    rooms
        .get(room)
        .map(|r| {
            r.members
                .iter()
                .filter(|(d, _)| d.as_str() != except)
                .map(|(d, m)| RosterPeer {
                    device_id: d.clone(),
                    name: m.name.clone(),
                    x25519_pub: m.x25519_pub,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Send the room roster to every member — each copy WITHOUT that member
/// itself. A self-entry would make the client offer the device as a sync
/// peer of itself (auto-sync targeting its own device id, a handshake that
/// fails on the frame that comes back to the same device).
fn broadcast_locked(rooms: &HashMap<String, Room>, room: &str) {
    let Some(r) = rooms.get(room) else {
        return;
    };
    for (d, m) in r.members.iter() {
        let roster = roster_locked(rooms, room, d);
        let frame = encode(&RelayMsg::Roster { roster });
        let _ = m.tx.send(frame);
    }
}

fn short_id(id: &str) -> String {
    if id.len() <= 12 {
        id.to_string()
    } else {
        format!("{}…{}", &id[..6], &id[id.len() - 4..])
    }
}

fn humanize(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else if s < 86400 {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    } else {
        format!("{}d {}h", s / 86400, (s % 86400) / 3600)
    }
}

fn decode_frame(frame: &[u8]) -> anyhow::Result<RelayMsg> {
    spidernotes_p2p::relay::decode(frame).map_err(|e| anyhow::anyhow!("relay frame: {e}"))
}
