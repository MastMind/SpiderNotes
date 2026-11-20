//! Local mDNS self-test: two Discovery instances must see each other.
//! Run: cargo run -p spidernotes-p2p --example mdns_selftest

use spidernotes_core::DeviceIdentity;
use spidernotes_p2p::discovery::Discovery;
use std::time::Duration;

fn print_ifaces() {
    let out = std::process::Command::new("ip")
        .args(["-4", "-o", "addr", "show"])
        .output();
    if let Ok(o) = out {
        println!("{}", String::from_utf8_lossy(&o.stdout).trim());
    }
}

#[tokio::main]
async fn main() {
    print_ifaces();
    let a = DeviceIdentity::generate("alpha").unwrap();
    let b = DeviceIdentity::generate("beta").unwrap();
    println!("alpha: {} (port 18788)", &a.id[..16]);
    println!("beta:  {} (port 18789)", &b.id[..16]);

    let da = Discovery::start(&a, 18788).expect("start A");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let db = Discovery::start(&b, 18789).expect("start B");
    println!("both started, waiting up to 20s for mutual discovery...");

    // pump both until each sees the other (by name), or the deadline hits
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(150), da.next_event()).await;
        let _ = tokio::time::timeout(Duration::from_millis(150), db.next_event()).await;
        let a_saw_b = da.peers().iter().any(|p| p.name == "beta");
        let b_saw_a = db.peers().iter().any(|p| p.name == "alpha");
        if a_saw_b && b_saw_a {
            break;
        }
    }

    let peers_a = da.peers();
    let peers_b = db.peers();
    let ok_a = peers_a.iter().any(|p| p.name == "beta");
    let ok_b = peers_b.iter().any(|p| p.name == "alpha");
    println!("A saw B: {ok_a}");
    println!("B saw A: {ok_b}");
    println!("A peers: {peers_a:?}");
    println!("B peers: {peers_b:?}");
    if ok_a && ok_b {
        println!("SELFTEST PASS");
    } else {
        println!("SELFTEST FAIL");
        std::process::exit(1);
    }
}
