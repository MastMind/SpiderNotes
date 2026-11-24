//! SpiderNotes relay server binary.
//!
//! Usage:
//!   spidernotes-server --secret "a-long-random-secret" \
//!       [--host 0.0.0.0] [--port 8788] [--ctrl 127.0.0.1:8789]
//!
//! One global secret guards the whole server; rooms are created by clients
//! on first join (same room name = same group) and deleted when empty.
//! The control socket answers `help` / `rooms` / `members [room]` /
//! `stats` / `quit` — e.g. `nc 127.0.0.1 8789`.
//!
//! Logging: INFO by default (join/leave/rooms/auth), per-frame details with
//! RUST_LOG=debug.

use std::net::{SocketAddr, ToSocketAddrs};

use spidernotes_server::Server;
use tokio::net::TcpListener;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut host = "0.0.0.0".to_string();
    let mut port: u16 = 8788;
    let mut secret: Option<String> = None;
    let mut ctrl: SocketAddr = "127.0.0.1:8789".parse()?;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = args.get(i).cloned().unwrap_or_else(|| die("--host requires a value"));
            }
            "--port" => {
                i += 1;
                port = args.get(i).and_then(|s| s.parse().ok()).unwrap_or_else(|| {
                    die("--port requires a number")
                });
            }
            "--secret" => {
                i += 1;
                let s = args.get(i).cloned().unwrap_or_else(|| die("--secret requires a value"));
                if s.trim().is_empty() {
                    die("--secret must not be empty");
                }
                secret = Some(s);
            }
            "--ctrl" => {
                i += 1;
                let s = args.get(i).cloned().unwrap_or_else(|| die("--ctrl requires a value"));
                ctrl = s
                    .to_socket_addrs()
                    .ok()
                    .and_then(|mut it| it.next())
                    .unwrap_or_else(|| die("--ctrl must be HOST:PORT, e.g. 127.0.0.1:8789"));
            }
            other => {
                eprintln!("unknown argument {other}");
                usage();
                std::process::exit(2);
            }
        }
        i += 1;
    }
    let secret = secret.unwrap_or_else(|| {
        eprintln!("--secret is required (one shared secret for the whole server)");
        usage();
        std::process::exit(2);
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let listener = TcpListener::bind((host.as_str(), port)).await?;
        let ctrl_listener = TcpListener::bind(ctrl).await?;
        let server = Server::new(secret);
        println!(
            "SpiderNotes relay server listening on {} (control: {ctrl})",
            listener.local_addr()?
        );
        tracing::info!(
            "relay server started on {}, control socket on {ctrl}",
            listener.local_addr()?
        );
        tokio::select! {
            r = server.clone().run(listener) => r,
            r = server.run_ctrl(ctrl_listener) => r,
        }
    })
}

fn usage() {
    eprintln!(
        "usage: spidernotes-server --secret SECRET [--host H] [--port P] [--ctrl HOST:PORT]"
    );
    eprintln!("rooms are created by clients (same --room value = same group); empty rooms are deleted automatically");
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    usage();
    std::process::exit(2);
}
