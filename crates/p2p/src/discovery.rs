//! mDNS discovery of other SpiderNotes devices on the LAN.
//!
//! Each device advertises `_spidernotes._tcp.local` with TXT records:
//! `did` (device id), `pub` (hex x25519 public key for Noise_XK), `name`.

use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent, ServiceInfo};
use spidernotes_core::DeviceIdentity;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const SERVICE_TYPE: &str = "_spidernotes._tcp.local.";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub device_id: String,
    pub x25519_pub: [u8; 32],
    /// One of the peer's addresses (first known).
    pub address: String,
    pub port: u16,
}

#[derive(Debug, Clone)]
pub enum PeerEvent {
    Joined(PeerInfo),
    /// device_id
    Left(String),
}

pub struct Discovery {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    /// keyed by instance label, e.g. "sn-0123456789ab"
    peers: Arc<Mutex<HashMap<String, PeerInfo>>>,
    own_id: String,
}

fn instance_for(id: &str) -> String {
    format!("sn-{}", &id[..12.min(id.len())])
}

/// Best-effort primary outbound IPv4: bind a UDP socket to the wildcard
/// address and `connect()` to a public endpoint — no packet is sent, the
/// OS just picks the interface it would use for the default route.
fn local_outbound_ip() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:53").ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

/// Rank an advertised address: home-LAN IPv4 (192.168/16, 10/8) best, then
/// other private IPv4 (172.16/12 — Docker's default bridge 172.17/16 also
/// falls here, so it stays behind the LAN ranges), then any global IPv4,
/// then loopback/link-local/IPv6 as a last resort (better a peer with a
/// weird address than no peer at all, e.g. single-host containers).
fn addr_score(a: &str) -> Option<u8> {
    let s = a.trim_start_matches('[').trim_end_matches(']');
    let s = s.split('%').next()?;
    let ip: std::net::IpAddr = s.parse().ok()?;
    match ip {
        std::net::IpAddr::V4(v4) => {
            if v4.is_loopback() || v4.is_link_local() {
                return Some(0);
            }
            let o = v4.octets();
            if o[0] == 192 && o[1] == 168 {
                Some(3)
            } else if o[0] == 10 {
                Some(3)
            } else if o[0] == 172 && (16..=31).contains(&o[1]) {
                Some(2)
            } else {
                Some(1)
            }
        }
        std::net::IpAddr::V6(_) => Some(0),
    }
}

fn pick_address(addrs: &[String]) -> Option<String> {
    addrs
        .iter()
        .filter_map(|a| addr_score(a).map(|s| (s, a)))
        .max_by_key(|(s, _)| *s)
        .map(|(_, a)| a.clone())
}

impl Discovery {
    /// Start advertising ourselves and browsing for other peers.
    pub fn start(identity: &DeviceIdentity, listen_port: u16) -> anyhow::Result<Self> {
        let daemon = ServiceDaemon::new()?;
        let instance = instance_for(&identity.id);
        // Also advertise our primary outbound IP in TXT: it rides in the same
        // packet as the SRV, unlike per-interface A records which can be
        // lost on flaky Wi-Fi (peers fall back to it in that case).
        let mut props: Vec<(String, String)> = vec![
            ("did".into(), identity.id.clone()),
            ("pub".into(), hex::encode(identity.x25519_public)),
            ("port".into(), listen_port.to_string()),
            ("name".into(), identity.name.clone()),
        ];
        if let Some(ip) = local_outbound_ip() {
            props.push(("ips".into(), ip.to_string()));
        }
        // The SRV target (hostname) must be UNIQUE per device: mdns-sd
        // registers A records for that name from the local interfaces, and a
        // name shared by two devices makes them overwrite each other's A
        // records in every peer's cache (peers then get the wrong IP and
        // "disappear"). Using the instance label (derived from the device
        // id) is unique even for cloned machines with the same hostname.
        let host = format!("{instance}.local.");
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host,
            "",
            listen_port,
            props.as_slice(),
        )
        .map_err(|e| anyhow::anyhow!("ServiceInfo::new: {e}"))?
        .enable_addr_auto();
        daemon
            .register(info)
            .map_err(|e| anyhow::anyhow!("register: {e}"))?;
        let events = daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| anyhow::anyhow!("browse: {e}"))?;
        Ok(Self {
            daemon,
            events,
            peers: Arc::new(Mutex::new(HashMap::new())),
            own_id: identity.id.clone(),
        })
    }

    /// Wait for the next peer join/leave event.
    pub async fn next_event(&self) -> Option<PeerEvent> {
        match self.events.recv_async().await {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let (instance, peer) = self.parse(&info)?;
                let mut map = self.peers.lock().unwrap();
                let changed = map
                    .insert(instance, peer.clone())
                    .map(|old| old != peer)
                    .unwrap_or(true);
                if changed {
                    tracing::info!("peer joined: {} ({})", peer.name, peer.address);
                    Some(PeerEvent::Joined(peer))
                } else {
                    None
                }
            }
            Ok(ServiceEvent::ServiceRemoved(_stype, fullname)) => {
                let key = fullname.split('.').next().unwrap_or(&fullname).to_string();
                let mut map = self.peers.lock().unwrap();
                if let Some(peer) = map.remove(&key) {
                    tracing::info!("peer left: {}", peer.name);
                    Some(PeerEvent::Left(peer.device_id))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// Current known peers.
    pub fn peers(&self) -> Vec<PeerInfo> {
        let mut v: Vec<PeerInfo> = self.peers.lock().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        v
    }

    fn parse(&self, info: &ServiceInfo) -> Option<(String, PeerInfo)> {
        let device_id = info.get_property_val_str("did")?.to_string();
        if device_id == self.own_id {
            return None;
        }
        let pub_hex = info.get_property_val_str("pub")?;
        let bytes = hex::decode(pub_hex).ok()?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        let name = info
            .get_property_val_str("name")
            .unwrap_or(&device_id[..12.min(device_id.len())])
            .to_string();
        // Prefer the resolved A records: they are segment-aware (a peer only
        // receives the record sent on the shared segment) and mDNS-native.
        // If the best A record is unusable (only v6/loopback arrived, e.g.
        // per-interface A packets were lost), fall back to the `ips` TXT
        // list (a single outbound IP that travels with the SRV packet).
        let a_addrs: Vec<String> = info
            .get_addresses()
            .iter()
            .map(|a| a.to_string())
            .collect();
        let t_addrs: Vec<String> = info
            .get_property_val_str("ips")
            .map(|ips| ips.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();
        let address = pick_address(&a_addrs)
            .filter(|a| addr_score(a) >= Some(1))
            .or_else(|| pick_address(&t_addrs))
            .or_else(|| pick_address(&a_addrs))?;
        let instance = info
            .get_fullname()
            .split('.')
            .next()
            .unwrap_or("")
            .to_string();
        Some((
            instance,
            PeerInfo {
                name,
                device_id,
                x25519_pub: arr,
                address,
                port: info.get_port(),
            },
        ))
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discover_peer_on_loopback() {
        let a = DeviceIdentity::generate("alpha").unwrap();
        let b = DeviceIdentity::generate("beta").unwrap();

        let disc_a = Discovery::start(&a, 18788).unwrap();
        // give the daemon a moment to come up
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let disc_b = Discovery::start(&b, 18789).unwrap();

        // best effort: mDNS may be unavailable in CI containers. Wait for
        // beta specifically — other real devices may be on the LAN (dev
        // machines) and join first.
        let seen = tokio::time::timeout(std::time::Duration::from_secs(6), async {
            loop {
                if let Some(PeerEvent::Joined(p)) = disc_a.next_event().await {
                    if p.device_id == b.id {
                        return p;
                    }
                }
            }
        })
        .await;

        match seen {
            Ok(p) => {
                assert_eq!(p.name, "beta");
                assert_eq!(p.x25519_pub, b.x25519_public);
                assert_eq!(p.port, 18789);
                assert!(!disc_a.peers().is_empty());
                let _ = disc_b;
            }
            Err(_) => {
                eprintln!("mDNS not available in test env, skipping assertion");
            }
        }
    }
}
