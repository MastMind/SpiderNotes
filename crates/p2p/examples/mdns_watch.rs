//! Watch all mDNS events for _spidernotes for N seconds (default 45).
//! Usage: cargo run -p spidernotes-p2p --example mdns_watch [seconds]

use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(45);
    let daemon = ServiceDaemon::new().expect("daemon");
    let rx = daemon
        .browse("_spidernotes._tcp.local.")
        .expect("browse");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut resolved = std::collections::HashSet::new();
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), rx.recv_async()).await {
            Ok(Ok(ev)) => match ev {
                ServiceEvent::ServiceFound(ty, inst) => {
                    println!("FOUND {ty} {inst}");
                }
                ServiceEvent::ServiceResolved(info) => {
                    let addrs: Vec<String> = info.get_addresses().iter().map(|a| a.to_string()).collect();
                    println!(
                        "RESOLVED {} host={} port={} addrs={:?} did={:?}",
                        info.get_fullname(),
                        info.get_hostname(),
                        info.get_port(),
                        addrs,
                        info.get_property_val_str("did").map(|s| &s[..12.min(s.len())]),
                    );
                    resolved.insert(info.get_fullname().to_string());
                }
                ServiceEvent::ServiceRemoved(ty, inst) => {
                    println!("REMOVED {ty} {inst}");
                    resolved.remove(&inst);
                }
                ServiceEvent::SearchStarted(t) => {
                    println!("SEARCH-STARTED {t}");
                }
                ServiceEvent::SearchStopped(t) => {
                    println!("SEARCH-STOPPED {t}");
                }
            },
            Ok(Err(e)) => {
                println!("RECV ERR {e}");
                break;
            }
            Err(_) => continue,
        }
    }
    println!("FINAL resolved set: {resolved:?}");
    let _ = daemon.shutdown();
}
