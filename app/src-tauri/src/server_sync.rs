//! Relay-server client manager: connects the app to the sync server when
//! enabled, keeps the roster fresh, runs incoming sync sessions, and feeds
//! the merged peer list (mDNS + relay) to the UI and the sync loops.

use crate::commands::{emit_peers, SYNC_PROTOCOL_TIMEOUT};
use crate::state::{AppState, ServerCfg};
use spidernotes_p2p::agent::{capped_responder, reject_busy};
use spidernotes_p2p::discovery::PeerInfo;
use spidernotes_p2p::relay::{RelayStream, RosterPeer};
use spidernotes_p2p::server_client::{ServerClient, ServerEvent};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;

fn set_status(st: &AppState, s: &str) {
    *st.server_status.lock().unwrap() = s.to_string();
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

fn apply_roster(app: &AppHandle, st: &AppState, roster: &[RosterPeer]) {
    *st.server_peers.lock().unwrap() = roster_to_peers(roster);
    emit_peers(app, st);
}

fn clear_server_peers(app: &AppHandle, st: &AppState) {
    if !st.server_peers.lock().unwrap().is_empty() {
        *st.server_peers.lock().unwrap() = Vec::new();
        emit_peers(app, st);
    }
}

/// Stop the current relay loop (if any) and start a fresh one for `cfg`
/// (or shut down when `cfg` is None).
pub fn restart_server_sync(app: AppHandle, st: &AppState) {
    if let Some(tx) = st.server_stop.lock().unwrap().take() {
        let _ = tx.send(true);
    }
    *st.server_handle.lock().unwrap() = None;
    clear_server_peers(&app, st);
    let Some(cfg) = st.server_cfg.lock().unwrap().clone() else {
        set_status(st, "server sync is off");
        return;
    };
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    *st.server_stop.lock().unwrap() = Some(stop_tx);
    set_status(st, &format!("connecting to {}:{} …", cfg.host, cfg.port));
    tauri::async_runtime::spawn(server_loop(app, st.clone(), cfg, stop_rx));
}

async fn server_loop(
    app: AppHandle,
    st: AppState,
    cfg: ServerCfg,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
) {
    let Ok(addr) = format!("{}:{}", cfg.host, cfg.port).parse::<SocketAddr>() else {
        set_status(&st, &format!("bad server address {}:{}", cfg.host, cfg.port));
        return;
    };
    let mut backoff = Duration::from_secs(2);
    // Dedupe state lives for the whole loop (across reconnects): only a
    // CHANGED trusted set triggers an auto-sync — every re-JOIN
    // re-broadcasts the full roster, and re-triggering on an unchanged
    // roster just creates collision opportunities.
    let mut last_trusted: BTreeSet<String> = BTreeSet::new();
    loop {
        if *stop_rx.borrow() {
            return;
        }
        set_status(&st, &format!("connecting to {}:{} …", cfg.host, cfg.port));
        let res = tokio::select! {
            _ = stop_rx.changed() => return,
            r = ServerClient::connect(addr, &cfg.room, &cfg.secret, &st.identity) => r,
        };
        match res {
            Ok((client, roster)) => {
                backoff = Duration::from_secs(2);
                let handle = client.handle();
                *st.server_handle.lock().unwrap() = Some(handle);
                apply_roster(&app, &st, &roster);
                set_status(
                    &st,
                    &format!("room '{}' — {} peer(s)", cfg.room, roster.len()),
                );
                let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<ServerEvent>();
                let run_task = tauri::async_runtime::spawn(async move {
                    client.run(ev_tx).await
                });
                loop {
                    tokio::select! {
                        _ = stop_rx.changed() => {
                            run_task.abort();
                            return;
                        }
                        ev = ev_rx.recv() => match ev {
                            Some(ServerEvent::RosterUpdated(r)) => {
                                apply_roster(&app, &st, &r);
                                set_status(
                                    &st,
                                    &format!("room '{}' — {} peer(s)", cfg.room, r.len()),
                                );
                                // a trusted room peer (re)appeared: sync now
                                // instead of waiting for the autosync tick
                                let trusted: Vec<PeerInfo> = roster_to_peers(&r)
                                    .into_iter()
                                    .filter(|p| st.trust.is_trusted(&p.device_id))
                                    .collect();
                                let ids: BTreeSet<String> =
                                    trusted.iter().map(|p| p.device_id.clone()).collect();
                                if ids != last_trusted {
                                    last_trusted = ids;
                                    if !trusted.is_empty() {
                                        crate::commands::spawn_auto_sync(
                                            app.clone(),
                                            st.clone(),
                                            trusted,
                                        );
                                    }
                                }
                            }
                            Some(ServerEvent::Incoming { devid, stream }) => {
                                spawn_server_responder(app.clone(), st.clone(), devid, stream);
                            }
                            None => break, // relay connection lost
                        },
                    }
                }
                let _ = run_task.await;
                *st.server_handle.lock().unwrap() = None;
                clear_server_peers(&app, &st);
                set_status(&st, "relay connection lost, reconnecting …");
            }
            Err(e) => {
                set_status(&st, &format!("relay: {e}"));
                tokio::select! {
                    _ = stop_rx.changed() => return,
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

/// Responder side of a relay-initiated sync. Same rules as the mDNS
/// responder: try_lock the gate (never await — the peer's initiator may be
/// blocked on us), reject untrusted devices, bounded protocol time.
fn spawn_server_responder(
    app: AppHandle,
    st: AppState,
    devid: String,
    stream: RelayStream,
) {
    tauri::async_runtime::spawn(async move {
        let Ok(_gate) = st.sync_gate.try_lock() else {
            tracing::info!("relay: local sync in progress, rejecting session from {devid}");
            // A relay session does not propagate a dropped stream as EOF, so
            // silently returning here would leave the peer's initiator
            // blocked until its timeout. Complete the handshake and answer
            // with a protocol error instead: the peer fails in milliseconds
            // with a clear message and retries on its next round.
            let _ = reject_busy(stream, &st.identity).await;
            return;
        };
        let Some(mut vault) = st.vault.lock().unwrap().clone() else {
            tracing::warn!("relay session from {devid}, but no vault is open");
            return;
        };
        if let Err(e) = vault.scan_and_repair() {
            tracing::warn!("pre-sync scan: {e}");
        }
        match capped_responder(
            stream,
            &st.identity,
            &mut vault,
            &st.trust,
            SYNC_PROTOCOL_TIMEOUT,
            st.download_media.load(Ordering::SeqCst),
        )
        .await
        {
            Ok(report) => {
                tracing::info!(
                    "relay sync with {}: sent {}, applied {}, skipped {}",
                    report.peer_name,
                    report.sent,
                    report.applied,
                    report.skipped
                );
                st.put_vault(vault);
                let _ = app.emit("notes-changed", ());
            }
            Err(e) => tracing::warn!("relay sync with {devid} failed: {e}"),
        }
    });
}
