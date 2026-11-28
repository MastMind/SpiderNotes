//! Local display-name overrides for peers: a device id -> name map the user
//! edits in the Network panel. It never travels to other devices (the peer
//! keeps advertising its own identity name); it only changes what THIS
//! device shows for that peer — in the peer list, note rows, anywhere
//! `merged_peers()` is the source. Persisted next to the trust list.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;

use crate::identity;

const FILE: &str = "peer_names.json";

/// A cheaply-clonable, thread-safe map of device id -> local display name.
#[derive(Debug, Clone, Default)]
pub struct PeerNames {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    path: Option<PathBuf>,
    names: BTreeMap<String, String>,
}

impl PeerNames {
    /// Load the map from `<config>/spidernotes/peer_names.json` (absent file
    /// = empty map).
    pub fn load_or_create() -> anyhow::Result<Self> {
        let base = dirs::config_dir().context("no config dir")?;
        Self::load_or_create_in(&base)
    }

    /// Load from `<base>/spidernotes/peer_names.json` (mobile: the app data
    /// dir, where the default config dir is not writable).
    pub fn load_or_create_in(base: &std::path::Path) -> anyhow::Result<Self> {
        let dir = base.join("spidernotes");
        std::fs::create_dir_all(&dir)?;
        identity::migrate_double_nested(&dir, FILE);
        let path = dir.join(FILE);
        let names = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default()
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                path: Some(path),
                names,
            })),
        })
    }

    /// An in-memory map that does not persist (used by tests).
    pub fn memory() -> Self {
        Self::default()
    }

    /// The local override for a peer, if any.
    pub fn name(&self, device_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .names
            .get(device_id)
            .cloned()
    }

    /// Set (or replace) the local display name of a peer.
    pub fn set(&self, device_id: &str, name: &str) -> anyhow::Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.names.insert(device_id.to_string(), name.to_string());
        Self::save_locked(&g)
    }

    /// Drop the override (the peer shows its advertised name again).
    pub fn remove(&self, device_id: &str) -> anyhow::Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.names.remove(device_id);
        Self::save_locked(&g)
    }

    fn save_locked(g: &Inner) -> anyhow::Result<()> {
        let Some(path) = &g.path else {
            return Ok(());
        };
        let raw = serde_json::to_string_pretty(&g.names)?;
        std::fs::write(path, raw)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_remove() {
        let p = PeerNames::memory();
        assert_eq!(p.name("abc"), None);
        p.set("abc", "Kitchen").unwrap();
        assert_eq!(p.name("abc").as_deref(), Some("Kitchen"));
        p.remove("abc").unwrap();
        assert_eq!(p.name("abc"), None);
    }

    #[test]
    fn persists_across_instances() {
        let base =
            std::env::temp_dir().join(format!("sn-peer-names-{}", uuid::Uuid::new_v4()));
        let p = PeerNames::load_or_create_in(&base).unwrap();
        p.set("dev-x", "Office").unwrap();
        let p2 = PeerNames::load_or_create_in(&base).unwrap();
        assert_eq!(p2.name("dev-x").as_deref(), Some("Office"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
