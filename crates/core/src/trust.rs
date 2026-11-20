//! Device trust allowlist: which other SpiderNotes devices may exchange notes
//! with this one. Persisted next to the identity in the config dir.
//!
//! Sync (both directions) is only allowed between mutually trusted devices:
//! the initiator checks the peer's id before connecting, and the responder
//! rejects any `Hello` from an id that is not in the allowlist.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::identity;

const FILE: &str = "trusted.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedDevice {
    pub device_id: String,
    pub name: String,
    pub added_at_ms: u64,
}

#[derive(Debug, Default)]
struct Inner {
    path: Option<PathBuf>,
    devices: BTreeMap<String, TrustedDevice>,
}

/// A cheaply-clonable, thread-safe allowlist of trusted device ids.
#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    inner: Arc<Mutex<Inner>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl TrustStore {
    /// Load the allowlist from `<config>/spidernotes/trusted.json`, creating
    /// an empty file if absent.
    pub fn load_or_create() -> anyhow::Result<Self> {
        let base = dirs::config_dir().context("no config dir")?;
        Self::load_or_create_in(&base)
    }

    /// Load the allowlist from `<base>/spidernotes/trusted.json`, creating an
    /// empty file if absent. Used on mobile, where the default config dir
    /// (`/.config`) is not writable.
    pub fn load_or_create_in(base: &std::path::Path) -> anyhow::Result<Self> {
        let dir = base.join("spidernotes");
        std::fs::create_dir_all(&dir)?;
        identity::migrate_double_nested(&dir, FILE);
        let path = dir.join(FILE);
        let devices = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let v: Vec<TrustedDevice> =
                serde_json::from_str(&raw).unwrap_or_default();
            v.into_iter()
                .map(|d| (d.device_id.clone(), d))
                .collect()
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                path: Some(path),
                devices,
            })),
        })
    }

    /// An in-memory store that does not persist (used by tests).
    pub fn memory() -> Self {
        Self::default()
    }

    pub fn is_trusted(&self, device_id: &str) -> bool {
        self.inner.lock().unwrap().devices.contains_key(device_id)
    }

    pub fn add(&self, device_id: &str, name: &str) -> anyhow::Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.devices.insert(
            device_id.to_string(),
            TrustedDevice {
                device_id: device_id.to_string(),
                name: name.to_string(),
                added_at_ms: now_ms(),
            },
        );
        Self::save_locked(&g)
    }

    pub fn remove(&self, device_id: &str) -> anyhow::Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.devices.remove(device_id);
        Self::save_locked(&g)
    }

    pub fn list(&self) -> Vec<TrustedDevice> {
        self.inner.lock().unwrap().devices.values().cloned().collect()
    }

    fn save_locked(g: &Inner) -> anyhow::Result<()> {
        let Some(path) = &g.path else {
            return Ok(());
        };
        let v: Vec<TrustedDevice> = g.devices.values().cloned().collect();
        let mut raw = serde_json::to_string_pretty(&v)?;
        raw.push('\n');
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
    fn add_remove_list() {
        let t = TrustStore::memory();
        assert!(!t.is_trusted("abc"));
        t.add("abc", "device-a").unwrap();
        assert!(t.is_trusted("abc"));
        assert_eq!(t.list().len(), 1);
        assert_eq!(t.list()[0].name, "device-a");
        t.remove("abc").unwrap();
        assert!(!t.is_trusted("abc"));
        assert!(t.list().is_empty());
    }

    #[test]
    fn migrates_double_nested_layout() {
        let base =
            std::env::temp_dir().join(format!("sn-trust-mig-{}", uuid::Uuid::new_v4()));
        let t = TrustStore::load_or_create_in(&base).unwrap();
        t.add("dev-a", "a").unwrap();
        // Simulate the old double-nested layout
        let dir = base.join("spidernotes");
        let old_dir = dir.join("spidernotes");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::rename(dir.join(FILE), old_dir.join(FILE)).unwrap();
        let t2 = TrustStore::load_or_create_in(&base).unwrap();
        assert!(t2.is_trusted("dev-a"));
        assert!(dir.join(FILE).exists());
        assert!(!old_dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn persists_to_config_dir() {
        // Uses the real config dir; unique id to avoid cross-test interference.
        let id = format!("test-{}", uuid::Uuid::new_v4());
        let t = TrustStore::load_or_create().unwrap();
        t.add(&id, "persist-test").unwrap();
        let t2 = TrustStore::load_or_create().unwrap();
        assert!(t2.is_trusted(&id));
        t2.remove(&id).unwrap();
        let t3 = TrustStore::load_or_create().unwrap();
        assert!(!t3.is_trusted(&id));
    }
}
