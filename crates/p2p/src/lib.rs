//! SpiderNotes P2P: mDNS discovery + Noise_XK/TCP transport + sync protocol
//! + relay-server client (for syncing when mDNS cannot discover peers).

pub mod agent;
pub mod discovery;
pub mod protocol;
pub mod relay;
pub mod server_client;
pub mod transport;

pub use agent::{spawn_responder, sync_initiator, sync_responder, SyncReport};
pub use discovery::{Discovery, PeerEvent, PeerInfo, SERVICE_TYPE};
pub use protocol::{my_delta, Msg};
pub use relay::{RelayShared, RelayStream, RosterPeer};
pub use server_client::{ServerClient, ServerEvent, ServerHandle};
pub use transport::{SecureStream, SyncListener};
