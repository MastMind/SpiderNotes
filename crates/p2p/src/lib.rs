//! SpiderNotes P2P: mDNS discovery + Noise_XK/TCP transport + sync protocol.

pub mod agent;
pub mod discovery;
pub mod protocol;
pub mod transport;

pub use agent::{spawn_responder, sync_initiator, sync_responder, SyncReport};
pub use discovery::{Discovery, PeerEvent, PeerInfo, SERVICE_TYPE};
pub use protocol::{my_delta, Msg};
pub use transport::{SecureStream, SyncListener};
