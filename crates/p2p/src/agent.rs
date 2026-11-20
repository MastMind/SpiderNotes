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

pub async fn sync_initiator(
    stream: TcpStream,
    identity: &DeviceIdentity,
    remote_pub: [u8; 32],
    vault: &mut Vault,
) -> anyhow::Result<SyncReport> {
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
    let delta = my_delta(&my_summaries, &their_summaries);
    for s in &delta {
        let content = if s.deleted {
            None
        } else {
            Some(vault.read(&s.uuid)?.content)
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
                if vault.apply_update(&update)? {
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

pub async fn sync_responder(
    stream: TcpStream,
    identity: &DeviceIdentity,
    vault: &mut Vault,
    trust: &TrustStore,
) -> anyhow::Result<SyncReport> {
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

    // stream my delta
    let delta = my_delta(&my_summaries, &their_summaries);
    for s in &delta {
        let content = if s.deleted {
            None
        } else {
            Some(vault.read(&s.uuid)?.content)
        };
        ss.send_frame(&Msg::Note { update: s.to_update(content) }.encode())
            .await?;
    }
    ss.send_frame(&Msg::Done.encode()).await?;
    report.sent = delta.len();

    loop {
        match Msg::decode(&ss.recv_frame().await?)? {
            Msg::Note { update } => {
                if vault.apply_update(&update)? {
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
        let shared = vault_a.write("shared.md", "hello v1").unwrap();
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

        // Round 2: both edit shared.md; B edits later -> B wins LWW
        vault_a.write("shared.md", "A version").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let winner = vault_b.write("shared.md", "B version (winner)").unwrap();

        let (_ra2, _rb2, vault_b) = sync_once(&mut vault_a, &mut vault_b).await;
        for v in [&vault_a, &vault_b] {
            let s = v
                .live_notes()
                .into_iter()
                .find(|s| s.path == "shared.md")
                .unwrap();
            assert_eq!(s.uuid, winner.uuid, "LWW: B's later edit must win");
        }
        let content_b = std::fs::read_to_string(env_b.dir.join("shared.md")).unwrap();
        assert!(content_b.contains("B version (winner)"));
        let content_a = std::fs::read_to_string(env_a.dir.join("shared.md")).unwrap();
        assert!(content_a.contains("B version (winner)"));
    }

    #[tokio::test]
    async fn deletion_propagates() {
        let (_env_a, mut vault_a) = Env::new("del-a");
        let s = vault_a.write("doomed.md", "bye").unwrap();

        let (env_b, mut vault_b) = Env::new("del-b");
        // B already has the note (same uuid/hlc as A had)
        let _ = vault_b
            .apply_update(&s.to_update(Some("bye".into())));

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
