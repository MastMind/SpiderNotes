//! End-to-end tests against the REAL server (crates/server): dynamic rooms,
//! global secret, room isolation, empty-room deletion, control socket.

use spidernotes_core::{DeviceIdentity, TrustStore, Vault};
use spidernotes_p2p::agent::{sync_initiator, sync_responder};
use spidernotes_p2p::server_client::{ServerClient, ServerEvent};
use spidernotes_server::Server;
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

#[tokio::test]
async fn real_server_relays_sync_between_two_clients() {
    let dir_a = std::env::temp_dir().join(format!("sn-srv-a-{}", uuid::Uuid::new_v4()));
    let dir_b = std::env::temp_dir().join(format!("sn-srv-b-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    let id_a = DeviceIdentity::generate("srv-a").unwrap();
    let id_b = DeviceIdentity::generate("srv-b").unwrap();
    let trust_a = TrustStore::memory();
    trust_a.add(&id_b.id, &id_b.name).unwrap();
    let trust_b = TrustStore::memory();
    trust_b.add(&id_a.id, &id_a.name).unwrap();

    let mut vault_a = Vault::open(&dir_a, id_a.clone()).unwrap();
    vault_a.write("real.md", "via the real server").unwrap();
    let mut vault_b = Vault::open(&dir_b, id_b.clone()).unwrap();

    let server = Server::new("s3cret".to_string());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        server.run(listener).await
    });

    // A joins first and CREATES the room
    let (client_a, roster_a) =
        ServerClient::connect(addr, "family", "s3cret", &id_a).await.unwrap();
    assert!(roster_a.is_empty(), "A joins first, room created empty");
    let handle_a = client_a.handle();
    let (ev_tx_a, mut ev_rx_a) = mpsc::unbounded_channel::<ServerEvent>();
    let a_task = tokio::spawn(async move {
        client_a.run(ev_tx_a).await
    });

    // B joins the same room and sees A
    let (client_b, roster_b) =
        ServerClient::connect(addr, "family", "s3cret", &id_b).await.unwrap();
    assert_eq!(roster_b.len(), 1, "B sees A");
    assert_eq!(roster_b[0].device_id, id_a.id);
    let (ev_tx_b, mut ev_rx_b) = mpsc::unbounded_channel::<ServerEvent>();
    let b_task = tokio::spawn(async move {
        client_b.run(ev_tx_b).await
    });

    // A learns of B through the roster broadcast
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

    // A opens a relay session to B and runs the initiator
    let stream = handle_a.open_session(&id_b.id).expect("open session");
    let sync_task = tokio::spawn({
        let id_a = id_a.clone();
        async move {
            let mut v = vault_a;
            sync_initiator(stream, &id_a, b_pub, &mut v).await
        }
    });

    // B surfaces the incoming session and runs the responder
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
    let on_b = vault_b.live_notes().into_iter().find(|n| n.path == "real.md");
    assert!(on_b.is_some(), "note arrived at B through the real server");

    a_task.abort();
    b_task.abort();
    server_task.abort();
    let _ = trust_a;
    let _ = std::fs::remove_dir_all(&dir_a);
    let _ = std::fs::remove_dir_all(&dir_b);
}

#[tokio::test]
async fn real_server_rejects_bad_secret() {
    let server = Server::new("right".to_string());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        server.run(listener).await
    });
    let id = DeviceIdentity::generate("x").unwrap();
    let res = ServerClient::connect(addr, "room", "WRONG", &id).await;
    assert!(res.is_err(), "wrong global secret must be rejected");
    server_task.abort();
}

#[tokio::test]
async fn rooms_are_dynamic_isolated_and_deleted_when_empty() {
    let server = Server::new("s".to_string());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let server_task = tokio::spawn({
        let s = server.clone();
        async move { s.run(listener).await }
    });

    // A creates room "one"
    let id_a = DeviceIdentity::generate("rm-a").unwrap();
    let (client_a, _) = ServerClient::connect(addr, "one", "s", &id_a).await.unwrap();
    let (ev_tx_a, _ev_rx_a) = mpsc::unbounded_channel::<ServerEvent>();
    let a_task = tokio::spawn(async move {
        client_a.run(ev_tx_a).await
    });
    assert_eq!(server.room_lines().len(), 1, "room created on first join");
    assert!(server
        .room_lines()
        .join("\n")
        .contains("one: 1 member(s)"));

    // B creates a different room "two": isolated roster
    let id_b = DeviceIdentity::generate("rm-b").unwrap();
    let (client_b, roster_b) = ServerClient::connect(addr, "two", "s", &id_b).await.unwrap();
    assert!(roster_b.is_empty(), "different room must not see A");
    let (ev_tx_b, _ev_rx_b) = mpsc::unbounded_channel::<ServerEvent>();
    let b_task = tokio::spawn(async move {
        client_b.run(ev_tx_b).await
    });
    assert_eq!(server.room_lines().len(), 2, "two independent rooms");

    // C joins room "two": sees B, not A
    let id_c = DeviceIdentity::generate("rm-c").unwrap();
    let (client_c, roster_c) = ServerClient::connect(addr, "two", "s", &id_c).await.unwrap();
    assert_eq!(roster_c.len(), 1);
    assert_eq!(roster_c[0].device_id, id_b.id);
    let (ev_tx_c, _ev_rx_c) = mpsc::unbounded_channel::<ServerEvent>();
    let c_task = tokio::spawn(async move {
        client_c.run(ev_tx_c).await
    });

    // A leaves -> room "one" is deleted automatically
    a_task.abort();
    // the server notices the drop asynchronously; give it a moment
    for _ in 0..50 {
        if server.room_lines().is_empty() || !server.room_lines().join("\n").contains("one:") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        !server.room_lines().join("\n").contains("one:"),
        "empty room must be deleted: {}",
        server.room_lines().join("\n")
    );

    b_task.abort();
    c_task.abort();
    server_task.abort();
}

/// Drive the control socket like a human would with `nc`: send one command,
/// read one response line (responses here are single-line).
 async fn ctrl_query(stream: &mut TcpStream, line: &str) -> String {
    stream
        .write_all(format!("{line}\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut l = String::new();
    reader.read_line(&mut l).await.unwrap();
    l
}

#[tokio::test]
async fn control_socket_reports_rooms_members_stats() {
    let server = Server::new("s".to_string());
    let data_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let data_addr: SocketAddr = data_listener.local_addr().unwrap();
    let ctrl_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ctrl_addr: SocketAddr = ctrl_listener.local_addr().unwrap();
    let server_task = tokio::spawn({
        let s = server.clone();
        async move { s.run(data_listener).await }
    });
    let ctrl_task = tokio::spawn({
        let s = server.clone();
        async move { s.run_ctrl(ctrl_listener).await }
    });

    // a member in room "office"
    let id = DeviceIdentity::generate("ctrl-member").unwrap();
    let (client, _) = ServerClient::connect(data_addr, "office", "s", &id).await.unwrap();
    let (ev_tx, _ev_rx) = mpsc::unbounded_channel::<ServerEvent>();
    let c_task = tokio::spawn(async move {
        client.run(ev_tx).await
    });
    // let the join land
    for _ in 0..50 {
        if !server.room_lines().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let mut stream = TcpStream::connect(ctrl_addr).await.unwrap();
    let rooms = ctrl_query(&mut stream, "rooms").await;
    assert!(rooms.contains("office: 1 member(s)"), "rooms: {rooms}");
    let members = ctrl_query(&mut stream, "members").await;
    assert!(members.contains("ctrl-member"), "members: {members}");
    assert!(members.contains("office:"), "members: {members}");
    let stats = ctrl_query(&mut stream, "stats").await;
    assert!(stats.contains("connections"), "stats: {stats}");
    assert!(stats.contains("auth failures"), "stats: {stats}");
    let unknown = ctrl_query(&mut stream, "bogus").await;
    assert!(unknown.contains("unknown command"), "bogus: {unknown}");
    ctrl_query(&mut stream, "quit").await;
    drop(stream);

    c_task.abort();
    server_task.abort();
    ctrl_task.abort();
}
