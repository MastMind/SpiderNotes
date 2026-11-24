//! Client <-> relay-server protocol and the virtual byte stream that makes
//! the Noise_XK sync protocol run over the relay.
//!
//! The server is a dumb router: after an authenticated join (room + secret,
//! proved with an HMAC challenge so the secret itself is never sent), the
//! client receives a roster of room members (device id, name, x25519 public
//! key — all public information) and can exchange opaque DATA frames with
//! any member. DATA payloads are end-to-end encrypted sync frames, the
//! server never sees plaintext.
//!
//! Framing: `[u32 LE length][payload]` (same shape as the sync transport).

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::task::{Context, Poll};

use bytes::{Buf, BytesMut};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

type HmacSha256 = Hmac<Sha256>;

pub const MAX_FRAME: u32 = 64 * 1024 * 1024;

// message tags
const TAG_JOIN: u8 = 0x01;
const TAG_CHALLENGE: u8 = 0x02;
const TAG_PROOF: u8 = 0x03;
const TAG_OK: u8 = 0x04;
const TAG_ROSTER: u8 = 0x05;
const TAG_DATA: u8 = 0x06;
const TAG_ERR: u8 = 0x07;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterPeer {
    pub device_id: String,
    pub name: String,
    pub x25519_pub: [u8; 32],
}

#[derive(Debug)]
pub enum RelayMsg {
    Join {
        room: String,
        device_id: String,
        name: String,
        x25519_pub: [u8; 32],
        nonce: [u8; 32],
    },
    Challenge {
        nonce: [u8; 32],
    },
    Proof {
        mac: [u8; 32],
    },
    Ok {
        roster: Vec<RosterPeer>,
    },
    Roster {
        roster: Vec<RosterPeer>,
    },
    Data {
        to: String,
        session: u32,
        payload: Vec<u8>,
    },
    Err {
        message: String,
    },
}

// ---------- encoding ----------

fn put_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn put_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    put_u16(buf, b.len() as u16);
    buf.extend_from_slice(b);
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, i: 0 }
    }
    fn u8(&mut self) -> io::Result<u8> {
        let v = self.b.get(self.i).ok_or(io::ErrorKind::UnexpectedEof)?;
        self.i += 1;
        Ok(*v)
    }
    fn u16(&mut self) -> io::Result<u16> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn bytes(&mut self, n: usize) -> io::Result<&'a [u8]> {
        let e = self.i + n;
        let s = self.b.get(self.i..e).ok_or(io::ErrorKind::UnexpectedEof)?;
        self.i = e;
        Ok(s)
    }
    fn str(&mut self) -> io::Result<String> {
        let n = self.u16()? as usize;
        let s = std::str::from_utf8(self.bytes(n)?).map_err(|_| io::ErrorKind::InvalidData)?;
        Ok(s.to_string())
    }
    fn fixed32(&mut self) -> io::Result<[u8; 32]> {
        let b = self.bytes(32)?;
        Ok(b.try_into().unwrap())
    }
}

fn encode_roster(buf: &mut Vec<u8>, roster: &[RosterPeer]) {
    put_u16(buf, roster.len() as u16);
    for p in roster {
        put_bytes(buf, p.device_id.as_bytes());
        put_bytes(buf, p.name.as_bytes());
        buf.extend_from_slice(&p.x25519_pub);
    }
}

fn decode_roster(cur: &mut Cursor) -> io::Result<Vec<RosterPeer>> {
    let n = cur.u16()? as usize;
    let mut out = Vec::with_capacity(n.min(1024));
    for _ in 0..n {
        let device_id = cur.str()?;
        let name = cur.str()?;
        let x25519_pub = cur.fixed32()?;
        out.push(RosterPeer { device_id, name, x25519_pub });
    }
    Ok(out)
}

pub fn encode(msg: &RelayMsg) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::new();
    match msg {
        RelayMsg::Join { room, device_id, name, x25519_pub, nonce } => {
            body.push(TAG_JOIN);
            put_bytes(&mut body, room.as_bytes());
            put_bytes(&mut body, device_id.as_bytes());
            put_bytes(&mut body, name.as_bytes());
            body.extend_from_slice(x25519_pub);
            body.extend_from_slice(nonce);
        }
        RelayMsg::Challenge { nonce } => {
            body.push(TAG_CHALLENGE);
            body.extend_from_slice(nonce);
        }
        RelayMsg::Proof { mac } => {
            body.push(TAG_PROOF);
            body.extend_from_slice(mac);
        }
        RelayMsg::Ok { roster } => {
            body.push(TAG_OK);
            encode_roster(&mut body, roster);
        }
        RelayMsg::Roster { roster } => {
            body.push(TAG_ROSTER);
            encode_roster(&mut body, roster);
        }
        RelayMsg::Data { to, session, payload } => {
            body.push(TAG_DATA);
            put_bytes(&mut body, to.as_bytes());
            body.extend_from_slice(&session.to_le_bytes());
            body.extend_from_slice(payload);
        }
        RelayMsg::Err { message } => {
            body.push(TAG_ERR);
            put_bytes(&mut body, message.as_bytes());
        }
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn decode(frame: &[u8]) -> io::Result<RelayMsg> {
    let mut cur = Cursor::new(frame);
    let tag = cur.u8()?;
    match tag {
        TAG_JOIN => {
            let room = cur.str()?;
            let device_id = cur.str()?;
            let name = cur.str()?;
            let x25519_pub = cur.fixed32()?;
            let nonce = cur.fixed32()?;
            Ok(RelayMsg::Join { room, device_id, name, x25519_pub, nonce })
        }
        TAG_CHALLENGE => Ok(RelayMsg::Challenge { nonce: cur.fixed32()? }),
        TAG_PROOF => Ok(RelayMsg::Proof { mac: cur.fixed32()? }),
        TAG_OK => Ok(RelayMsg::Ok { roster: decode_roster(&mut cur)? }),
        TAG_ROSTER => Ok(RelayMsg::Roster { roster: decode_roster(&mut cur)? }),
        TAG_DATA => {
            let to = cur.str()?;
            let session = cur.u32()?;
            let payload = cur.bytes(cur.b.len() - cur.i)?.to_vec();
            Ok(RelayMsg::Data { to, session, payload })
        }
        TAG_ERR => Ok(RelayMsg::Err { message: cur.str()? }),
        t => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown relay tag {t}"),
        )),
    }
}

/// Read one length-prefixed frame.
pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(
    r: &mut R,
) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} out of bounds"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

pub async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    msg: &RelayMsg,
) -> io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let bytes = encode(msg);
    w.write_all(&bytes).await
}

/// HMAC-SHA256(secret, room || nonce_client || nonce_server) — the proof a
/// client sends to show it knows the room secret without ever sending it.
pub fn room_proof(secret: &str, room: &str, nonce_client: &[u8; 32], nonce_server: &[u8; 32]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(room.as_bytes());
    mac.update(nonce_client);
    mac.update(nonce_server);
    mac.finalize().into_bytes().into()
}

fn const_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verify a room proof against a candidate secret.
pub fn verify_room_proof(secret: &str, room: &str, nonce_client: &[u8; 32], nonce_server: &[u8; 32], proof: &[u8; 32]) -> bool {
    const_eq(&room_proof(secret, room, nonce_client, nonce_server), proof)
}

pub fn random_nonce() -> [u8; 32] {
    let mut n = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut n);
    n
}

// ---------- virtual stream over the relay ----------

/// Outcome of routing a DATA payload to a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteResult {
    /// Delivered to a live session.
    Delivered,
    /// No such session: this is a NEW incoming session (first frame).
    NoSession,
    /// Session existed but its stream is gone; the entry was removed.
    Dead,
}

/// What a session writes to the server connection.
pub enum RelayOut {
    Data {
        to: String,
        session: u32,
        payload: Vec<u8>,
    },
    /// A session stream was dropped: forget it.
    Close {
        to: String,
        session: u32,
    },
}

/// How long a closed session id is remembered so that in-flight frames of
/// the dead session are dropped instead of starting a phantom session.
const DEAD_SESSION_TTL: Duration = Duration::from_secs(120);
const DEAD_SESSION_MAX: usize = 10_000;

/// Shared state between the relay read loop and everything else.
pub struct RelayShared {
    /// Write side of the server connection (owned by the read loop).
    pub conn_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<RelayOut>>>,
    /// (peer devid, session) -> feed for that session's incoming bytes.
    sessions: std::sync::Mutex<HashMap<(String, u32), mpsc::UnboundedSender<Vec<u8>>>>,
    /// Recently closed session ids. A peer's in-flight frames (e.g. the
    /// `Hello` sent right before a busy-reject) arrive AFTER we closed the
    /// session; without this set the read loop would mistake them for a NEW
    /// session and start a phantom responder on garbage bytes.
    dead: std::sync::Mutex<HashMap<(String, u32), Instant>>,
    pub connected: AtomicBool,
}

impl RelayShared {
    pub fn new() -> Self {
        Self {
            conn_tx: std::sync::Mutex::new(None),
            sessions: std::sync::Mutex::new(HashMap::new()),
            dead: std::sync::Mutex::new(HashMap::new()),
            connected: AtomicBool::new(false),
        }
    }

    /// True when `key` is a recently closed session: its late frames must
    /// be dropped, not treated as a new incoming session.
    pub fn is_dead(&self, key: &(String, u32)) -> bool {
        let mut dead = self.dead.lock().unwrap();
        match dead.get(key) {
            Some(t) if t.elapsed() <= DEAD_SESSION_TTL => true,
            Some(_) => {
                dead.remove(key);
                false
            }
            None => false,
        }
    }

    /// Register a session, returning its (feed, stream) halves: feed
    /// incoming bytes into `feed_tx`, build the peer's `RelayStream` on
    /// `stream_rx`.
    pub fn insert_session(
        &self,
        key: (String, u32),
    ) -> (
        mpsc::UnboundedSender<Vec<u8>>,
        mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        let (tx, rx) = mpsc::unbounded_channel();
        self.sessions.lock().unwrap().insert(key, tx.clone());
        (tx, rx)
    }

    pub fn has_session(&self, key: &(String, u32)) -> bool {
        self.sessions.lock().unwrap().contains_key(key)
    }

    pub fn take_session(&self, key: &(String, u32)) -> Option<mpsc::UnboundedSender<Vec<u8>>> {
        let removed = self.sessions.lock().unwrap().remove(key);
        if removed.is_some() {
            self.mark_dead(key);
        }
        removed
    }

    fn mark_dead(&self, key: &(String, u32)) {
        let mut dead = self.dead.lock().unwrap();
        // opportunistic prune: expired entries, then the oldest overflow
        if dead.len() >= DEAD_SESSION_MAX {
            dead.retain(|_, t| t.elapsed() <= DEAD_SESSION_TTL);
            if dead.len() >= DEAD_SESSION_MAX {
                let excess = dead.len() - DEAD_SESSION_MAX + 1;
                let mut entries: Vec<((String, u32), Instant)> =
                    dead.iter().map(|(k, t)| (k.clone(), *t)).collect();
                entries.sort_by_key(|(_, t)| *t); // oldest first
                for (key, _) in entries.into_iter().take(excess) {
                    dead.remove(&key);
                }
            }
        }
        dead.insert(key.clone(), Instant::now());
    }

    pub fn route(&self, key: (String, u32), payload: Vec<u8>) -> RouteResult {
        let gone = {
            let mut m = self.sessions.lock().unwrap();
            match m.get(&key) {
                Some(tx) => match tx.send(payload) {
                    Ok(()) => return RouteResult::Delivered,
                    Err(_) => {
                        // the stream holding the receiver is gone
                        m.remove(&key);
                        true
                    }
                },
                None => return RouteResult::NoSession,
            }
        };
        if gone {
            self.mark_dead(&key);
        }
        RouteResult::Dead
    }

    /// Drop all sessions (connection lost).
    pub fn clear_sessions(&self) {
        self.sessions.lock().unwrap().clear();
    }
}

/// A byte stream backed by the relay: writes become DATA frames for one
/// (peer, session), reads come from the matching DATA frames the read loop
/// routes here. Lets the stock Noise_XK sync protocol run over the server.
pub struct RelayStream {
    out: mpsc::UnboundedSender<RelayOut>,
    to: String,
    session: u32,
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    buf: BytesMut,
    done: bool,
}

impl RelayStream {
    pub fn new(
        out: mpsc::UnboundedSender<RelayOut>,
        to: String,
        session: u32,
        rx: mpsc::UnboundedReceiver<Vec<u8>>,
    ) -> Self {
        Self { out, to, session, rx, buf: BytesMut::new(), done: false }
    }

    pub fn session_id(&self) -> u32 {
        self.session
    }

    fn push(&mut self, chunk: Vec<u8>) {
        self.buf.extend_from_slice(&chunk);
    }
}

impl Drop for RelayStream {
    fn drop(&mut self) {
        let _ = self.out.send(RelayOut::Close {
            to: self.to.clone(),
            session: self.session,
        });
    }
}

impl AsyncRead for RelayStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.done && self.buf.is_empty() {
            return Poll::Ready(Ok(())); // EOF
        }
        if self.buf.is_empty() {
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(chunk)) => self.push(chunk),
                Poll::Ready(None) => {
                    self.done = true;
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
        let n = std::cmp::min(buf.remaining(), self.buf.len());
        buf.put_slice(&self.buf[..n]);
        self.buf.advance(n);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for RelayStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let chunk = buf.to_vec();
        let to = self.to.clone();
        let session = self.session;
        // unbounded channel: send only fails once the connection is gone
        match self.out.send(RelayOut::Data { to, session, payload: chunk }) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(_) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "relay connection closed",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn msg_roundtrip() {
        let roster = vec![
            RosterPeer {
                device_id: "abc".into(),
                name: "phone".into(),
                x25519_pub: [7u8; 32],
            },
            RosterPeer {
                device_id: "def".into(),
                name: "laptop".into(),
                x25519_pub: [9u8; 32],
            },
        ];
        for msg in [
            RelayMsg::Join {
                room: "r".into(),
                device_id: "d".into(),
                name: "n".into(),
                x25519_pub: [1; 32],
                nonce: [2; 32],
            },
            RelayMsg::Challenge { nonce: [3; 32] },
            RelayMsg::Proof { mac: [4; 32] },
            RelayMsg::Ok { roster: roster.clone() },
            RelayMsg::Roster { roster: roster.clone() },
            RelayMsg::Data {
                to: "d".into(),
                session: 42,
                payload: vec![5, 6, 7],
            },
            RelayMsg::Err { message: "nope".into() },
        ] {
            // encode is deterministic: re-encoding the decoded message must
            // give back the original bytes
            let wire = encode(&msg);
            let back = decode(&wire[4..]).unwrap();
            assert_eq!(wire, encode(&back), "roundtrip mismatch");
        }
    }

    #[test]
    fn room_proof_verifies() {
        let nc = [1u8; 32];
        let ns = [2u8; 32];
        let proof = room_proof("secret", "room", &nc, &ns);
        assert!(verify_room_proof("secret", "room", &nc, &ns, &proof));
        assert!(!verify_room_proof("wrong", "room", &nc, &ns, &proof));
        assert!(!verify_room_proof("secret", "other", &nc, &ns, &proof));
    }

    #[tokio::test]
    async fn relay_stream_read_across_chunks() {
        // the "server" side feeds chunks into the session feed; the stream
        // must reassemble them into a contiguous read
        let (out_tx, _out_rx) = mpsc::unbounded_channel::<RelayOut>();
        let (feed_tx, feed_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let stream = RelayStream {
            out: out_tx,
            to: "peer".into(),
            session: 7,
            rx: feed_rx,
            buf: BytesMut::new(),
            done: false,
        };
        let mut stream = stream;
        for chunk in [b"hello over ".to_vec(), b"relay".to_vec()] {
            feed_tx.send(chunk).unwrap();
        }
        drop(feed_tx); // EOF after the chunks
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello over relay");
    }

    #[tokio::test]
    async fn relay_stream_write_emits_data() {
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<RelayOut>();
        let stream = RelayStream {
            out: out_tx,
            to: "peer".into(),
            session: 9,
            rx: mpsc::unbounded_channel::<Vec<u8>>().1,
            buf: BytesMut::new(),
            done: false,
        };
        let mut stream = stream;
        stream.write_all(b"abc").await.unwrap();
        let msg = out_rx.recv().await.unwrap();
        match msg {
            RelayOut::Data { to, session, payload } => {
                assert_eq!(to, "peer");
                assert_eq!(session, 9);
                assert_eq!(payload, b"abc");
            }
            _ => panic!("expected Data"),
        }
        drop(stream);
        match out_rx.recv().await.unwrap() {
            RelayOut::Close { to, session } => {
                assert_eq!(to, "peer");
                assert_eq!(session, 9);
            }
            _ => panic!("expected Close on drop"),
        }
    }
}
