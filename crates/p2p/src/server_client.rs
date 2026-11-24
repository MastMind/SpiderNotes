//! Client side of the relay-server protocol: authenticated join (room +
//! secret, proved via HMAC challenge), roster tracking, and session
//! management that lets the stock Noise_XK sync protocol run over the relay.

use crate::relay::{
    random_nonce, read_frame, room_proof, write_frame, RelayMsg, RelayOut, RelayShared,
    RelayStream, RosterPeer,
};
use spidernotes_core::DeviceIdentity;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// Events the read loop reports to the application.
pub enum ServerEvent {
    /// Full room roster (replaces the previous one).
    RosterUpdated(Vec<RosterPeer>),
    /// A room peer opened a sync session towards us.
    Incoming { devid: String, stream: RelayStream },
}

/// Cheap handle the app keeps: lets sync commands open relay sessions
/// towards room peers while the connection is alive.
#[derive(Clone)]
pub struct ServerHandle {
    pub shared: Arc<RelayShared>,
}

impl ServerHandle {
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::SeqCst)
    }

    /// Open an initiator-side session towards `to`. `None` when the relay
    /// connection is not up.
    pub fn open_session(&self, to: &str) -> Option<RelayStream> {
        if !self.connected() {
            return None;
        }
        let out = self.shared.conn_tx.lock().unwrap().clone()?;
        let session = rand::random::<u32>() | 1;
        let key = (to.to_string(), session);
        let (_feed, stream_rx) = self.shared.insert_session(key);
        Some(RelayStream::new(out, to.to_string(), session, stream_rx))
    }
}

/// A connected, authenticated relay client. `run` consumes it and drives the
/// read/write loop until the connection drops.
pub struct ServerClient {
    pub shared: Arc<RelayShared>,
    roster: Vec<RosterPeer>,
    /// Never advertise ourselves to the app: a self-entry in the peer list
    /// makes the auto-sync target our OWN device id (a "sync" that sends
    /// msg1 into a session we opened towards ourselves and fails on the
    /// frame that comes back to us). Servers may include the member in its
    /// own roster broadcasts, so this filter is the client-side guarantee.
    own_id: String,
    rd: OwnedReadHalf,
    wr: OwnedWriteHalf,
}

impl ServerClient {
    /// TCP-connect, JOIN + HMAC challenge/proof, return with the initial roster.
    pub async fn connect(
        addr: SocketAddr,
        room: &str,
        secret: &str,
        identity: &DeviceIdentity,
    ) -> anyhow::Result<(Self, Vec<RosterPeer>)> {
        let stream = TcpStream::connect(addr).await?;
        let _ = stream.set_nodelay(true);
        let (mut rd, mut wr) = stream.into_split();

        let nonce_c = random_nonce();
        write_frame(
            &mut wr,
            &RelayMsg::Join {
                room: room.to_string(),
                device_id: identity.id.clone(),
                name: identity.name.clone(),
                x25519_pub: identity.x25519_public,
                nonce: nonce_c,
            },
        )
        .await?;

        let frame = read_frame(&mut rd).await?;
        let RelayMsg::Challenge { nonce: nonce_s } = relay_decode(&frame)? else {
            anyhow::bail!("relay: expected challenge");
        };

        let mac = room_proof(secret, room, &nonce_c, &nonce_s);
        write_frame(&mut wr, &RelayMsg::Proof { mac }).await?;

        let frame = read_frame(&mut rd).await?;
        let mut roster = match relay_decode(&frame)? {
            RelayMsg::Ok { roster } => roster,
            RelayMsg::Err { message } => anyhow::bail!("relay rejected: {message}"),
            other => anyhow::bail!("relay: unexpected {other:?}"),
        };
        let own_id = identity.id.clone();
        filter_self(&mut roster, &own_id);

        let shared = Arc::new(RelayShared::new());
        Ok((
            Self { shared, roster: roster.clone(), rd, wr, own_id },
            roster,
        ))
    }

    pub fn handle(&self) -> ServerHandle {
        ServerHandle { shared: self.shared.clone() }
    }

    pub fn roster(&self) -> &[RosterPeer] {
        &self.roster
    }

    /// Drive the connection until it drops. Returns `Ok(())` on any clean end.
    pub async fn run(
        self,
        ev_tx: mpsc::UnboundedSender<ServerEvent>,
    ) -> anyhow::Result<()> {
        let Self { shared, roster: _, rd, wr, own_id } = self;
        let mut rd = rd;
        let mut wr = wr;

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RelayOut>();
        *shared.conn_tx.lock().unwrap() = Some(out_tx.clone());
        shared.connected.store(true, Ordering::SeqCst);

        let result: anyhow::Result<()> = 'outer: loop {
            tokio::select! {
                frame = read_frame(&mut rd) => {
                    let frame = frame?;
                    match relay_decode(&frame)? {
                        RelayMsg::Data { to: from, session, payload } => {
                            let key = (from.clone(), session);
                            if shared.has_session(&key) {
                                let _ = shared.route(key, payload); // Delivered or Dead
                            } else if shared.is_dead(&key) {
                                // In-flight frame of a session we already
                                // closed (e.g. the initiator's `Hello` that
                                // landed after our busy-reject). Must be
                                // dropped: treating it as a new session
                                // starts a phantom responder on garbage.
                                tracing::debug!(
                                    "relay: dropping late frame for closed session {session} from {from}"
                                );
                            } else {
                                // a peer is starting a sync towards us
                                let (feed_tx, stream_rx) =
                                    shared.insert_session((from.clone(), session));
                                let _ = feed_tx.send(payload);
                                let out = shared
                                    .conn_tx
                                    .lock()
                                    .unwrap()
                                    .clone()
                                    .ok_or_else(|| anyhow::anyhow!("relay: no connection"))?;
                                let stream = RelayStream::new(out, from.clone(), session, stream_rx);
                                if ev_tx
                                    .send(ServerEvent::Incoming { devid: from, stream })
                                    .is_err()
                                {
                                    break 'outer Ok(());
                                }
                            }
                        }
                        RelayMsg::Roster { mut roster } => {
                            filter_self(&mut roster, &own_id);
                            if ev_tx
                                .send(ServerEvent::RosterUpdated(roster))
                                .is_err()
                            {
                                break 'outer Ok(());
                            }
                        }
                        RelayMsg::Err { message } => {
                            // A transient routing error (e.g. "peer not in
                            // room" for a peer whose connection the server
                            // has just noticed is dead). The offending
                            // session simply times out on its own — tearing
                            // down the WHOLE relay connection here would
                            // cause a re-JOIN storm and fresh roster
                            // broadcasts (more sync-trigger collisions).
                            tracing::warn!("relay server: {message}");
                        }
                        other => anyhow::bail!("relay: unexpected {other:?}"),
                    }
                }
                out = out_rx.recv() => {
                    match out {
                        Some(RelayOut::Data { to, session, payload }) => {
                            write_frame(&mut wr, &RelayMsg::Data { to, session, payload }).await?;
                        }
                        Some(RelayOut::Close { to, session }) => {
                            shared.take_session(&(to, session));
                        }
                        None => break 'outer Ok(()),
                    }
                }
            }
        };

        shared.connected.store(false, Ordering::SeqCst);
        shared.clear_sessions();
        *shared.conn_tx.lock().unwrap() = None;
        result
    }
}

fn relay_decode(frame: &[u8]) -> anyhow::Result<RelayMsg> {
    crate::relay::decode(frame).map_err(|e| anyhow::anyhow!("relay frame: {e}"))
}

/// Drop the local device from a roster, in place.
fn filter_self(roster: &mut Vec<RosterPeer>, own_id: &str) {
    roster.retain(|p| p.device_id != own_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{sync_initiator, sync_responder};
    use crate::protocol::Msg;
    use crate::relay::{encode, verify_room_proof, RelayMsg as M};
    use crate::transport::SecureStream;
    use spidernotes_core::{TrustStore, Vault};
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use uuid::Uuid;

    struct Member {
        name: String,
        x25519_pub: [u8; 32],
        room: String,
        tx: mpsc::UnboundedSender<Vec<u8>>,
    }

    /// Minimal in-process relay with the same protocol/semantics as
    /// crates/server (one global secret, dynamic rooms), small enough to
    /// live in a test.
    #[derive(Clone)]
    struct MiniRelay {
        secret: Arc<String>,
        members: Arc<Mutex<HashMap<String, Member>>>,
    }

    impl MiniRelay {
        fn new(secret: &str) -> Self {
            Self {
                secret: Arc::new(secret.to_string()),
                members: Arc::new(Mutex::new(HashMap::new())),
            }
        }

        fn roster(&self, room: &str, except: &str) -> Vec<RosterPeer> {
            self.members
                .lock()
                .unwrap()
                .iter()
                .filter(|(d, m)| m.room == room && d.as_str() != except)
                .map(|(d, m)| RosterPeer {
                    device_id: d.clone(),
                    name: m.name.clone(),
                    x25519_pub: m.x25519_pub,
                })
                .collect()
        }

        fn broadcast_roster(&self, room: &str) {
            let frame = encode(&M::Roster { roster: self.roster(room, "") });
            let members = self.members.lock().unwrap();
            for m in members.values().filter(|m| m.room == room) {
                let _ = m.tx.send(frame.clone());
            }
        }

        async fn run_one(&self, stream: TcpStream) {
            let (rd, wr) = stream.into_split();
            let mut rd = rd;
            let mut wr = wr;

            // JOIN
            let frame = match read_frame(&mut rd).await {
                Ok(f) => f,
                Err(_) => return,
            };
            let decoded = match relay_decode(&frame) {
                Ok(m) => m,
                Err(_) => return,
            };
            let M::Join { room, device_id, name, x25519_pub, nonce: nc } = decoded else {
                return;
            };
            // ONE global secret for the whole server
            let ns = random_nonce();
            if write_frame(&mut wr, &M::Challenge { nonce: ns }).await.is_err() {
                return;
            }
            let frame = match read_frame(&mut rd).await {
                Ok(f) => f,
                Err(_) => return,
            };
            let decoded = match relay_decode(&frame) {
                Ok(m) => m,
                Err(_) => return,
            };
            let M::Proof { mac } = decoded else {
                return;
            };
            if !verify_room_proof(&self.secret, &room, &nc, &ns, &mac) {
                let _ = write_frame(&mut wr, &M::Err { message: "bad secret".into() }).await;
                return;
            }

            // register (dynamic room) + welcome. Member.tx feeds THIS
            // member's socket write side, so roster broadcasts and routed
            // frames land here.
            let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
            self.members.lock().unwrap().insert(
                device_id.clone(),
                Member { name, x25519_pub, room: room.clone(), tx: out_tx.clone() },
            );
            if write_frame(&mut wr, &M::Ok { roster: self.roster(&room, &device_id) }).await.is_err() {
                self.members.lock().unwrap().remove(&device_id);
                return;
            }
            self.broadcast_roster(&room);

            // route: socket in -> members of the TARGET's room
            loop {
                tokio::select! {
                    frame = read_frame(&mut rd) => {
                        let frame = match frame { Ok(f) => f, Err(_) => break };
                        let decoded = match relay_decode(&frame) {
                            Ok(m) => m,
                            Err(_) => break,
                        };
                        let M::Data { to, session, payload } = decoded else {
                            break;
                        };
                        let target = self
                            .members
                            .lock()
                            .unwrap()
                            .get(&to)
                            .map(|m| m.tx.clone());
                        match target {
                            Some(t) => {
                                let _ = t.send(encode(&M::Data {
                                    to: device_id.clone(),
                                    session,
                                    payload,
                                }));
                            }
                            None => {
                                let _ = out_tx.send(encode(&M::Err {
                                    message: "peer not in room".into(),
                                }));
                            }
                        }
                    }
                    out = out_rx.recv() => match out {
                        Some(bytes) => {
                            if wr.write_all(&bytes).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    },
                }
            }
            self.members.lock().unwrap().remove(&device_id);
            self.broadcast_roster(&room);
        }
    }

    async fn spawn_relay(secret: &str) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let relay = MiniRelay::new(secret);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let relay = relay.clone();
                tokio::spawn(async move {
                    relay.run_one(stream).await;
                });
            }
        });
        (addr, task)
    }

    #[tokio::test]
    async fn e2e_sync_over_relay() {
        let dir_a = std::env::temp_dir().join(format!("sn-relay-a-{}", Uuid::new_v4()));
        let dir_b = std::env::temp_dir().join(format!("sn-relay-b-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let id_a = DeviceIdentity::generate("relay-a").unwrap();
        let id_b = DeviceIdentity::generate("relay-b").unwrap();
        let trust_a = TrustStore::memory();
        trust_a.add(&id_b.id, &id_b.name).unwrap();
        let trust_b = TrustStore::memory();
        trust_b.add(&id_a.id, &id_a.name).unwrap();
        let vault_a = Vault::open(&dir_a, id_a.clone()).unwrap();
        let mut vault_a = vault_a;
        vault_a.write("hello.md", "synced over relay").unwrap();
        let vault_b = Vault::open(&dir_b, id_b.clone()).unwrap();
        let mut vault_b = vault_b;

        let (addr, relay_task) = spawn_relay("topsecret").await;

        // A joins first (this CREATES room "room1")
        let (client_a, roster_a) =
            ServerClient::connect(addr, "room1", "topsecret", &id_a).await.unwrap();
        assert!(roster_a.is_empty(), "A joins first, room empty");
        let handle_a = client_a.handle();
        let (ev_tx_a, mut ev_rx_a) = mpsc::unbounded_channel::<ServerEvent>();
        let a_task = tokio::spawn(async move {
            client_a.run(ev_tx_a).await
        });

        // B joins and immediately sees A
        let (client_b, roster_b) =
            ServerClient::connect(addr, "room1", "topsecret", &id_b).await.unwrap();
        assert_eq!(roster_b.len(), 1, "B sees A in the roster");
        assert_eq!(roster_b[0].device_id, id_a.id);
        let (ev_tx_b, mut ev_rx_b) = mpsc::unbounded_channel::<ServerEvent>();
        let b_task = tokio::spawn(async move {
            client_b.run(ev_tx_b).await
        });

        // A learns about B through the roster broadcast
        let b_pub = loop {
            match ev_rx_a.try_recv() {
                Ok(ServerEvent::RosterUpdated(r)) => {
                    if let Some(p) = r.iter().find(|p| p.device_id == id_b.id) {
                        break p.x25519_pub;
                    }
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        };

        // A opens a relay session to B and runs the sync initiator
        let stream = handle_a.open_session(&id_b.id).expect("open session");
        let sync_task = tokio::spawn({
            let id_a = id_a.clone();
            async move {
                let mut v = vault_a;
                sync_initiator(stream, &id_a, b_pub, &mut v).await
            }
        });

        // B's read loop surfaces the incoming session; run the responder
        let incoming = loop {
            match ev_rx_b.try_recv() {
                Ok(ServerEvent::Incoming { devid, stream }) => {
                    assert_eq!(devid, id_a.id);
                    break stream;
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        };
        let responder = tokio::spawn({
            let id_b = id_b.clone();
            async move {
                let mut v = vault_b;
                let r = sync_responder(incoming, &id_b, &mut v, &trust_b).await;
                (r, v)
            }
        });

        let (res_a, res_b) = tokio::join!(sync_task, responder);
        let report_a = res_a.unwrap().unwrap();
        let (report_b, vault_b) = res_b.unwrap();
        let report_b = report_b.unwrap();
        assert!(report_a.sent >= 1, "A sent its note");
        assert!(report_b.applied >= 1, "B applied A's note");
        let on_b = vault_b
            .live_notes()
            .into_iter()
            .find(|n| n.path == "hello.md");
        assert!(on_b.is_some(), "note arrived at B via the relay");

        a_task.abort();
        b_task.abort();
        relay_task.abort();
        let _ = trust_a;
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    /// A server that includes a member in its OWN roster broadcast (the old
    /// builds did — `broadcast_locked` had no per-member exclusion) must not
    /// surface that member to itself: a self-entry in the peer list makes
    /// the auto-sync target its own device id, a "sync" whose msg1 comes
    /// back to the same device and fails on its own frame.
    #[tokio::test]
    async fn roster_never_contains_self() {
        let (addr, relay_task) = spawn_relay("topsecret").await;
        let id_a = DeviceIdentity::generate("self-a").unwrap();
        let id_b = DeviceIdentity::generate("self-b").unwrap();

        // MiniRelay's Ok already excludes the joiner, but its broadcast
        // (fired by B's join) contains EVERY member — self included.
        let (client_a, roster_a) =
            ServerClient::connect(addr, "room1", "topsecret", &id_a).await.unwrap();
        assert!(roster_a.is_empty(), "A joins first, room empty");
        let (ev_tx_a, mut ev_rx_a) = mpsc::unbounded_channel::<ServerEvent>();
        let a_task = tokio::spawn(async move {
            client_a.run(ev_tx_a).await
        });

        let (client_b, roster_b) =
            ServerClient::connect(addr, "room1", "topsecret", &id_b).await.unwrap();
        assert_eq!(roster_b.len(), 1, "B sees A in the initial roster");
        assert_ne!(
            roster_b[0].device_id,
            id_b.id,
            "B's initial roster must not contain B"
        );
        let (ev_tx_b, mut ev_rx_b) = mpsc::unbounded_channel::<ServerEvent>();
        let b_task = tokio::spawn(async move {
            client_b.run(ev_tx_b).await
        });

        // A's roster UPDATE (the broadcast fired by B's join) must contain
        // B and must NOT contain A, even though the raw broadcast lists A.
        let updated_a = loop {
            match ev_rx_a.try_recv() {
                Ok(ServerEvent::RosterUpdated(r)) if r.iter().any(|p| p.device_id == id_b.id) => {
                    break r
                }
                Ok(_) => {}
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        };
        assert!(
            !updated_a.iter().any(|p| p.device_id == id_a.id),
            "A sees itself in the roster: {updated_a:?}"
        );

        // Same for B's update.
        let updated_b = loop {
            match ev_rx_b.try_recv() {
                Ok(ServerEvent::RosterUpdated(r)) if r.iter().any(|p| p.device_id == id_a.id) => {
                    break r
                }
                Ok(_) => {}
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        };
        assert!(
            !updated_b.iter().any(|p| p.device_id == id_b.id),
            "B sees itself in the roster: {updated_b:?}"
        );

        a_task.abort();
        b_task.abort();
        relay_task.abort();
    }

    #[tokio::test]
    async fn busy_responder_fails_initiator_fast() {
        // When both devices sync at the same moment, each rejects the other's
        // session (the local gate is held by its own initiator). A dropped
        // relay session is NOT visible to the peer as EOF, so the app's
        // responder completes the handshake and answers with Msg::Error.
        // The initiator must fail fast with the reason — not hang.
        let dir_a = std::env::temp_dir().join(format!("sn-relay-busy-a-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir_a).unwrap();
        let id_a = DeviceIdentity::generate("busy-a").unwrap();
        let id_b = DeviceIdentity::generate("busy-b").unwrap();
        let vault_a = Vault::open(&dir_a, id_a.clone()).unwrap();
        let mut vault_a = vault_a;

        let (addr, relay_task) = spawn_relay("topsecret").await;

        let (client_a, _) = ServerClient::connect(addr, "room1", "topsecret", &id_a).await.unwrap();
        let handle_a = client_a.handle();
        let (ev_tx_a, mut ev_rx_a) = mpsc::unbounded_channel::<ServerEvent>();
        let a_task = tokio::spawn(async move {
            client_a.run(ev_tx_a).await
        });

        let (client_b, _) = ServerClient::connect(addr, "room1", "topsecret", &id_b).await.unwrap();
        let (ev_tx_b, mut ev_rx_b) = mpsc::unbounded_channel::<ServerEvent>();
        let b_task = tokio::spawn(async move {
            client_b.run(ev_tx_b).await
        });

        // A learns B's public key from the roster broadcast
        let b_pub = loop {
            match ev_rx_a.try_recv() {
                Ok(ServerEvent::RosterUpdated(r)) => {
                    if let Some(p) = r.iter().find(|p| p.device_id == id_b.id) {
                        break p.x25519_pub;
                    }
                }
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        };

        let stream = handle_a.open_session(&id_b.id).expect("open session");
        let sync_task = tokio::spawn({
            let id_a = id_a.clone();
            async move { sync_initiator(stream, &id_a, b_pub, &mut vault_a).await }
        });

        // B's read loop surfaces the incoming session; play the busy
        // responder: handshake, then Msg::Error instead of HelloAck.
        let incoming = loop {
            match ev_rx_b.try_recv() {
                Ok(ServerEvent::Incoming { stream, .. }) => break stream,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        };
        let busy = tokio::spawn({
            let id_b = id_b.clone();
            async move {
                let Ok(mut ss) = SecureStream::accept(incoming, &id_b).await else {
                    return;
                };
                let _ = ss
                    .send_frame(
                        &Msg::Error {
                            message: "device busy — retry".to_string(),
                        }
                        .encode(),
                    )
                    .await;
            }
        });

        let started = std::time::Instant::now();
        let res = tokio::time::timeout(std::time::Duration::from_secs(10), sync_task)
            .await
            .expect("initiator must not hang on a busy responder")
            .unwrap();
        let elapsed = started.elapsed();
        let msg = res.expect_err("busy responder must fail the initiator").to_string();
        assert!(msg.contains("busy"), "error must carry the reason, got: {msg}");
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "failed in {elapsed:?}"
        );

        busy.abort();
        a_task.abort();
        b_task.abort();
        relay_task.abort();
        let _ = std::fs::remove_dir_all(&dir_a);
    }

    #[tokio::test]
    async fn bad_secret_rejected() {
        let (addr, relay_task) = spawn_relay("right").await;
        let id = DeviceIdentity::generate("x").unwrap();
        let res = ServerClient::connect(addr, "room1", "WRONG", &id).await;
        assert!(res.is_err(), "wrong secret must be rejected");
        relay_task.abort();
    }

    #[tokio::test]
    async fn different_rooms_cannot_see_each_other() {
        let (addr, relay_task) = spawn_relay("right").await;
        let id_a = DeviceIdentity::generate("room-a").unwrap();
        let id_b = DeviceIdentity::generate("room-b").unwrap();

        // A creates room "one"
        let (client_a, roster_a) =
            ServerClient::connect(addr, "one", "right", &id_a).await.unwrap();
        assert!(roster_a.is_empty());
        let (ev_tx_a, _ev_rx_a) = mpsc::unbounded_channel::<ServerEvent>();
        let a_task = tokio::spawn(async move {
            client_a.run(ev_tx_a).await
        });

        // B creates a DIFFERENT room "two" — must not see A
        let (client_b, roster_b) =
            ServerClient::connect(addr, "two", "right", &id_b).await.unwrap();
        assert!(roster_b.is_empty(), "different rooms must not share rosters");
        let (ev_tx_b, _ev_rx_b) = mpsc::unbounded_channel::<ServerEvent>();
        let b_task = tokio::spawn(async move {
            client_b.run(ev_tx_b).await
        });

        // give any (wrong) cross-room broadcast a chance to arrive
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        a_task.abort();
        b_task.abort();
        relay_task.abort();
    }
}
