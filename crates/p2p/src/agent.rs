//! Sync session driver: initiator (connects to peer) and responder (accepts).
//!
//! Flow over one Noise-encrypted connection:
//!   I -> Hello, R -> HelloAck
//!   I -> Summaries(mine), R -> Summaries(theirs)
//!   I -> Note* + Done (its delta)
//!   R -> Note* + Done (its delta)
//! Both sides apply received notes with LWW and are converged afterwards.

use crate::protocol::{my_delta, Msg};
use crate::transport::SecureStream;
use spidernotes_core::{DeviceIdentity, TrustStore, Vault};
use std::marker::Unpin;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SyncReport {
    pub peer_name: String,
    pub sent: usize,
    pub applied: usize,
    pub skipped: usize,
}

fn hello_hlc(vault: &Vault) -> spidernotes_core::Hlc {
    vault
        .index()
        .device_clock
        .clone()
        .unwrap_or_else(|| spidernotes_core::Hlc::now(vault.device().short_id()))
}

/// LWW is decided by HLC timestamps, whose ms part is the local wall clock.
/// If the peer's clock is far off, "newer" edits may lose to older ones —
/// surface it instead of failing silently.
fn warn_clock_skew(peer: &str, remote: &spidernotes_core::Hlc) {
    let now = spidernotes_core::hlc::now_ms();
    let drift = now.abs_diff(remote.ms);
    if drift > 60_000 {
        tracing::warn!(
            "clock skew with {peer}: ~{} min — conflicting edits may resolve against you",
            drift / 60_000
        );
    }
}

pub async fn sync_initiator<S>(
    stream: S,
    identity: &DeviceIdentity,
    remote_pub: [u8; 32],
    vault: &mut Vault,
) -> anyhow::Result<SyncReport>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ss = SecureStream::initiator(stream, identity, remote_pub).await?;
    let mut report = SyncReport::default();

    ss.send_frame(
        &Msg::Hello {
            device: identity.id.clone(),
            name: identity.name.clone(),
            hlc: hello_hlc(vault),
        }
        .encode(),
    )
    .await?;
    match Msg::decode(&ss.recv_frame().await?)? {
        Msg::HelloAck { name, hlc, .. } => {
            warn_clock_skew(&name, &hlc);
            report.peer_name = name;
        }
        other => anyhow::bail!("expected HelloAck, got {other:?}"),
    }

    let my_summaries = vault.notes();
    ss.send_frame(
        &Msg::Summaries {
            summaries: my_summaries.clone(),
        }
        .encode(),
    )
    .await?;
    let their_summaries = match Msg::decode(&ss.recv_frame().await?)? {
        Msg::Summaries { summaries } => summaries,
        other => anyhow::bail!("expected Summaries, got {other:?}"),
    };

    // stream my delta
    // Both sides diverged from the shared base -> send even the OLDER copy,
    // so the peer records the conflict (with content) instead of silently
    // letting its own version win.
    let diverged = vault.diverged_from_base(&their_summaries);
    let delta = my_delta(&my_summaries, &their_summaries, &diverged);
    for s in &delta {
        let content = if s.deleted {
            None
        } else {
            let c = vault.read(&s.uuid)?.content;
            // Remember the sent version as the merge base only when the peer
            // will actually adopt it. A forced send (both sides diverged)
            // does not move the base: the peer keeps its own version, and
            // the old base stays the correct common ancestor. Without the
            // sender-side base, the device that created a note would keep no
            // base and a first divergent edit could be silently lost to LWW.
            if !diverged.iter().any(|f| *f == s.uuid) {
                vault.set_base(&s.uuid, &c)?;
            }
            Some(c)
        };
        ss.send_frame(&Msg::Note { update: s.to_update(content) }.encode())
            .await?;
    }
    ss.send_frame(&Msg::Done.encode()).await?;
    report.sent = delta.len();

    // receive peer delta
    loop {
        match Msg::decode(&ss.recv_frame().await?)? {
            Msg::Note { update } => {
                if vault.apply_update(&update, &report.peer_name)? {
                    report.applied += 1;
                } else {
                    report.skipped += 1;
                }
            }
            Msg::Done => break,
            Msg::Error { message } => anyhow::bail!("peer error: {message}"),
            other => anyhow::bail!("unexpected {other:?}"),
        }
    }
    Ok(report)
}

pub async fn sync_responder<S>(
    stream: S,
    identity: &DeviceIdentity,
    vault: &mut Vault,
    trust: &TrustStore,
) -> anyhow::Result<SyncReport>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut ss = SecureStream::accept(stream, identity).await?;
    let mut report = SyncReport::default();

    match Msg::decode(&ss.recv_frame().await?)? {
        Msg::Hello { device, name, hlc } => {
            warn_clock_skew(&name, &hlc);
            if !trust.is_trusted(&device) {
                let msg = format!("device {device} is not trusted; approve it in settings first");
                let _ = ss.send_frame(&Msg::Error { message: msg }.encode()).await;
                anyhow::bail!("rejected untrusted device {device}");
            }
            report.peer_name = name;
        }
        other => anyhow::bail!("expected Hello, got {other:?}"),
    }
    ss.send_frame(
        &Msg::HelloAck {
            device: identity.id.clone(),
            name: identity.name.clone(),
            hlc: hello_hlc(vault),
        }
        .encode(),
    )
    .await?;

    let their_summaries = match Msg::decode(&ss.recv_frame().await?)? {
        Msg::Summaries { summaries } => summaries,
        other => anyhow::bail!("expected Summaries, got {other:?}"),
    };
    let my_summaries = vault.notes();
    ss.send_frame(
        &Msg::Summaries {
            summaries: my_summaries.clone(),
        }
        .encode(),
    )
    .await?;

    // stream my delta (same rules as in sync_initiator: LWW winners plus
    // diverged versions, so conflicts are recorded on both sides)
    let diverged = vault.diverged_from_base(&their_summaries);
    let delta = my_delta(&my_summaries, &their_summaries, &diverged);
    for s in &delta {
        let content = if s.deleted {
            None
        } else {
            let c = vault.read(&s.uuid)?.content;
            if !diverged.iter().any(|f| *f == s.uuid) {
                vault.set_base(&s.uuid, &c)?;
            }
            Some(c)
        };
        ss.send_frame(&Msg::Note { update: s.to_update(content) }.encode())
            .await?;
    }
    ss.send_frame(&Msg::Done.encode()).await?;
    report.sent = delta.len();

    loop {
        match Msg::decode(&ss.recv_frame().await?)? {
            Msg::Note { update } => {
                if vault.apply_update(&update, &report.peer_name)? {
                    report.applied += 1;
                } else {
                    report.skipped += 1;
                }
            }
            Msg::Done => break,
            Msg::Error { message } => anyhow::bail!("peer error: {message}"),
            other => anyhow::bail!("unexpected {other:?}"),
        }
    }
    Ok(report)
}

/// Initiator-side sync with a hard cap and a human-readable error. Without
/// the cap a peer that accepts but never answers (its responder was rejected
/// because it is busy, and a relay session does not surface that as EOF)
/// would block the caller — and, with the sync gate held, every save behind
/// it — forever.
///
/// A "busy" error (see [`is_busy_error`]) means the peer is mid-sync with
/// someone else; the peer answered fast and on purpose, so a short retry is
/// safe and usually succeeds.
pub async fn capped_initiator<S>(
    stream: S,
    identity: &DeviceIdentity,
    remote_pub: [u8; 32],
    vault: &mut Vault,
    timeout: Duration,
) -> Result<SyncReport, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match tokio::time::timeout(timeout, sync_initiator(stream, identity, remote_pub, vault)).await
    {
        Ok(r) => r.map_err(|e| format!("sync failed: {e}")),
        Err(_) => Err(format!("sync timed out after {timeout:?}")),
    }
}

/// Responder-side sync with a hard cap and a human-readable error (same
/// reasoning as [`capped_initiator`]).
pub async fn capped_responder<S>(
    stream: S,
    identity: &DeviceIdentity,
    vault: &mut Vault,
    trust: &TrustStore,
    timeout: Duration,
) -> Result<SyncReport, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match tokio::time::timeout(timeout, sync_responder(stream, identity, vault, trust)).await {
        Ok(r) => r.map_err(|e| format!("sync failed: {e}")),
        Err(_) => Err(format!("sync timed out after {timeout:?}")),
    }
}

/// Busy rejection for an incoming session. A dropped relay session is NOT
/// visible to the peer as EOF, so silently returning would leave the peer's
/// initiator blocked until its timeout. Complete the Noise handshake and
/// answer with a protocol error instead: the peer fails in milliseconds with
/// a "busy" error and can retry on its next round.
pub async fn reject_busy<S>(stream: S, identity: &DeviceIdentity) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Ok(mut ss) = SecureStream::accept(stream, identity).await else {
        return Ok(()); // the peer already went away; nothing to tell it
    };
    ss.send_frame(
        &Msg::Error {
            message: "device busy — sync again in a moment".to_string(),
        }
        .encode(),
    )
    .await
    .map_err(|e| format!("busy reject failed: {e}"))
}

/// True when an error string carries a peer's "busy" rejection
/// (safe to retry after a short stagger).
pub fn is_busy_error(e: &str) -> bool {
    e.contains("busy")
}

/// Spawn the responder side for a given connection (used by tests and the app).
pub fn spawn_responder(
    stream: TcpStream,
    identity: DeviceIdentity,
    mut vault: Vault,
    trust: TrustStore,
) -> tokio::task::JoinHandle<anyhow::Result<SyncReport>> {
    tokio::spawn(async move { sync_responder(stream, &identity, &mut vault, &trust).await })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::SyncListener;
    use spidernotes_core::{DeviceIdentity, Vault};
    use std::net::SocketAddr;
    use uuid::Uuid;

    struct Env {
        dir: std::path::PathBuf,
    }
    impl Env {
        fn new(name: &str) -> (Self, Vault) {
            let dir = std::env::temp_dir().join(format!(
                "spidernotes-p2p-{}-{}",
                name,
                Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let dev = DeviceIdentity::generate(name).unwrap();
            let vault = Vault::open(&dir, dev).unwrap();
            (Self { dir }, vault)
        }
    }
    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

   async fn sync_once(
    a: &mut Vault,
    b: &mut Vault,
) -> (SyncReport, SyncReport, Vault) {
        let listener = SyncListener::bind_ephemeral().await.unwrap();
        let port = listener.port();
        let identity_a = a.device().clone();
        let identity_b = b.device().clone();
        let pub_b = identity_b.x25519_public;
        let b_clone = b.clone();
        // B is the responder: it must trust A for the sync to proceed.
        let trust_b = TrustStore::memory();
        trust_b.add(&identity_a.id, &identity_a.name).unwrap();

        let server_task = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            let mut b = b_clone;
            let report = sync_responder(stream, &identity_b, &mut b, &trust_b).await;
            (report, b)
        });

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let report_a = sync_initiator(
        tokio::net::TcpStream::connect(addr).await.unwrap(),
        &identity_a,
        pub_b,
        a,
    )
    .await
    .unwrap();
    let (r, b) = server_task.await.unwrap();
    let report_b = r.unwrap();
    (report_a, report_b, b)
}

    #[tokio::test]
    async fn two_vaults_converge() {
        // Round 1: A has shared.md + only-a.md; B has only-b.md
        let (env_a, mut vault_a) = Env::new("a");
        let shared = vault_a
            .write("shared.md", "base one\nbase two\nbase three")
            .unwrap();
        vault_a.write("only-a.md", "A only").unwrap();
        let (env_b, mut vault_b) = Env::new("b");
        vault_b.write("only-b.md", "B only").unwrap();

        let (_ra, _rb, mut vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        // all three notes on both sides, shared.md has A's uuid
        assert_eq!(vault_a.live_notes().len(), 3);
        assert_eq!(vault_b.live_notes().len(), 3);
        let shared_on_b = vault_b
            .live_notes()
            .into_iter()
            .find(|s| s.path == "shared.md")
            .unwrap();
        assert_eq!(shared_on_b.uuid, shared.uuid);

        // Round 2: both edit DISJOINT lines of shared.md -> three-way merge
        // combines them on both sides (no LWW loss, no conflict). One
        // exchange is not enough to converge: the merge result must travel
        // back, so a second exchange follows.
        vault_a.write("shared.md", "base one\nA two\nbase three").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        vault_b.write("shared.md", "base one\nbase two\nB three").unwrap();

        let (_ra2, _rb2, mut vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        for v in [&vault_a, &vault_b] {
            assert!(v.conflicts().is_empty(), "disjoint edits must not conflict");
        }
        let (_ra3, _rb3, _vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        let content_b = std::fs::read_to_string(env_b.dir.join("shared.md")).unwrap();
        assert!(
            content_b.contains("A two") && content_b.contains("B three"),
            "both edits must survive the merge: {content_b}"
        );
        let content_a = std::fs::read_to_string(env_a.dir.join("shared.md")).unwrap();
        assert!(
            content_a.contains("A two") && content_a.contains("B three"),
            "both edits must survive the merge: {content_a}"
        );
    }

    #[tokio::test]
    async fn overlapping_edits_conflict_without_silent_loss() {
        // Both sides rewrite the same (single-line) note: a genuine
        // overlap. Neither version may be dropped — each side keeps its own
        // live and records the peer's version as a conflict.
        let (env_a, mut vault_a) = Env::new("ov-a");
        let shared = vault_a.write("shared.md", "old line").unwrap();
        let (env_b, mut vault_b) = Env::new("ov-b");

        let (_ra, _rb, mut vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        assert_eq!(
            vault_b.get(&shared.uuid).map(|s| s.path.clone()),
            Some("shared.md".to_string())
        );

        vault_a.write("shared.md", "A's rewrite").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        vault_b.write("shared.md", "B's rewrite").unwrap();

        let (_ra2, _rb2, vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        // each side keeps its own version live ...
        let content_a = std::fs::read_to_string(env_a.dir.join("shared.md")).unwrap();
        assert!(content_a.contains("A's rewrite"));
        let content_b = std::fs::read_to_string(env_b.dir.join("shared.md")).unwrap();
        assert!(content_b.contains("B's rewrite"));
        // ... and records the peer's version (named after the peer)
        for (v, peer, other) in [
            (&vault_a, "ov-b", "B's rewrite"),
            (&vault_b, "ov-a", "A's rewrite"),
        ] {
            let cs = v.conflicts();
            assert_eq!(cs.len(), 1, "one pending conflict per side");
            assert_eq!(cs[0].peer_name, peer);
            assert!(
                v.conflict_content(&cs[0].id).unwrap().contains(other),
                "peer's version must be recoverable"
            );
        }
        let _ = env_b;
    }

    #[tokio::test]
    async fn deletion_propagates() {
        let (_env_a, mut vault_a) = Env::new("del-a");
        let s = vault_a.write("doomed.md", "bye").unwrap();

        let (env_b, mut vault_b) = Env::new("del-b");
        // B already has the note (same uuid/hlc as A had)
        let _ = vault_b
            .apply_update(&s.to_update(Some("bye".into())), "a");

        vault_a.delete(&s.uuid).unwrap();

        let listener = SyncListener::bind_ephemeral().await.unwrap();
        let port = listener.port();
        let identity_a = vault_a.device().clone();
        let identity_b = vault_b.device().clone();
        let pub_b = identity_b.x25519_public;
        let trust_b = TrustStore::memory();
        trust_b.add(&identity_a.id, &identity_a.name).unwrap();

        let server_task = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            let report = sync_responder(stream, &identity_b, &mut vault_b, &trust_b).await;
            (report, vault_b)
        });
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let _ = sync_initiator(
            tokio::net::TcpStream::connect(addr).await.unwrap(),
            &identity_a,
            pub_b,
            &mut vault_a,
        )
        .await
        .unwrap();
        let (r, vault_b) = server_task.await.unwrap();
        r.unwrap();

        let on_b = vault_b.get(&s.uuid).unwrap();
        assert!(on_b.deleted);
        assert!(!env_b.dir.join("doomed.md").exists());
    }

    #[tokio::test]
    async fn move_note_propagates_and_old_file_vanishes() {
        // A moves a note into a folder. B must end up with the note at the
        // NEW path, the old file removed, and — critically — a rescan (vault
        // re-open) must not resurrect the old path from the leftover file.
        let (env_a, mut vault_a) = Env::new("mv-a");
        let s = vault_a.write("x.md", "v1").unwrap();
        let (env_b, mut vault_b) = Env::new("mv-b");
        let (_ra, _rb, mut vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        assert_eq!(vault_b.get(&s.uuid).unwrap().path, "x.md");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        vault_a.rename(&s.uuid, "work/x.md").unwrap();

        let (_ra2, _rb2, mut vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        assert_eq!(vault_b.get(&s.uuid).unwrap().path, "work/x.md");
        assert!(!env_b.dir.join("x.md").exists(), "old file must vanish on receiver");
        assert!(env_b.dir.join("work/x.md").exists());

        vault_b.scan_and_repair().unwrap();
        assert_eq!(
            vault_b.get(&s.uuid).unwrap().path,
            "work/x.md",
            "rescan must not resurrect the old path"
        );
        let _ = env_a;
    }

    #[tokio::test]
    async fn folder_delete_propagates() {
        let (env_a, mut vault_a) = Env::new("fd-a");
        let s = vault_a.write("f/a.md", "A").unwrap();
        let (env_b, mut vault_b) = Env::new("fd-b");
        let (_ra, _rb, mut vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        assert_eq!(vault_b.get(&s.uuid).unwrap().path, "f/a.md");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let deleted = vault_a.delete_folder("f").unwrap();
        assert_eq!(deleted.len(), 1);

        let (_ra2, _rb2, mut vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        assert!(vault_b.get(&s.uuid).unwrap().deleted);
        assert!(!env_b.dir.join("f/a.md").exists());
        // the emptied dir is kept on the receiver (empty folders are
        // local state and are not pruned automatically)
        assert!(env_b.dir.join("f").exists(), "emptied dir must stay");
        let _ = env_a;
    }

    #[tokio::test]
    async fn untrusted_peer_rejected() {
        let (_env_a, mut vault_a) = Env::new("untr-a");
        vault_a.write("secret.md", "top secret").unwrap();
        let (env_b, mut vault_b) = Env::new("untr-b");
        vault_b.write("b-only.md", "B").unwrap();
        let before = vault_b.live_notes().len();

        let listener = SyncListener::bind_ephemeral().await.unwrap();
        let port = listener.port();
        let identity_a = vault_a.device().clone();
        let identity_b = vault_b.device().clone();
        let pub_b = identity_b.x25519_public;
        // B does NOT trust A.
        let trust_b = TrustStore::memory();

        let server_task = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            let report = sync_responder(stream, &identity_b, &mut vault_b, &trust_b).await;
            (report, vault_b)
        });
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let res_a = sync_initiator(
            tokio::net::TcpStream::connect(addr).await.unwrap(),
            &identity_a,
            pub_b,
            &mut vault_a,
        )
        .await;
        let (res_b, vault_b) = server_task.await.unwrap();

        // Both sides must fail.
        assert!(res_a.is_err(), "initiator should be rejected");
        assert!(res_b.is_err(), "responder should reject untrusted peer");
        // B's vault must be unchanged: no note from A leaked in.
        assert_eq!(vault_b.live_notes().len(), before);
        assert!(vault_b.live_notes().iter().all(|n| n.path != "secret.md"));
        let _ = env_b;
    }

    #[tokio::test]
    async fn initiator_fails_fast_when_peer_rejects() {
        // Regression for the cross-device gate deadlock: the responder now
        // rejects a connection (closes the socket) when a local sync holds
        // the gate. The initiator — which holds the local gate for the whole
        // exchange — must observe that as a prompt, clean error, never hang.
        let (env_a, mut vault_a) = Env::new("reject-a");
        let identity_a = vault_a.device().clone();

        let listener = SyncListener::bind_ephemeral().await.unwrap();
        let port = listener.port();

        // The "peer" accepts and immediately drops the connection (rejection).
        let server_task = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            drop(stream);
        });

        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            sync_initiator(
                tokio::net::TcpStream::connect(addr).await.unwrap(),
                &identity_a,
                [0u8; 32], // key value irrelevant: the point is the EOF
                &mut vault_a,
            ),
        )
        .await;
        server_task.await.unwrap();

        assert!(
            matches!(res, Ok(Err(_))),
            "initiator must get a prompt error when the peer closes; got: {res:?}"
        );
        let _ = env_a;
    }
}
