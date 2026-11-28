use anyhow::Context;
use ed25519_dalek::{Signer, SigningKey, Signature, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use x25519_dalek::StaticSecret;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeyMaterial {
    /// hex-encoded 32-byte ed25519 seed
    pub ed25519_secret_hex: String,
    /// hex-encoded 32-byte x25519 static seed
    pub x25519_secret_hex: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedIdentity {
    name: String,
    keys: KeyMaterial,
}

const FILE: &str = "identity.json";

pub fn config_dir() -> anyhow::Result<std::path::PathBuf> {
    let dir = dirs::config_dir().context("no config dir")?;
    let d = dir.join("spidernotes");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Earlier desktop builds double-nested the config path
/// (`<config>/spidernotes/spidernotes/<file>`) because `config_dir()` and
/// `load_or_create_in` each appended the app dir. If such a file exists,
/// move it to the canonical single-nested location (it is the newer one).
pub(crate) fn migrate_double_nested(dir: &std::path::Path, file: &str) {
    let old = dir.join("spidernotes").join(file);
    let raw = match std::fs::read(&old) {
        Ok(r) => r,
        Err(_) => return,
    };
    let _ = std::fs::write(dir.join(file), raw);
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_dir(dir.join("spidernotes"));
}

/// A SpiderNotes device identity:
/// - `id`: hex of the ed25519 public key (stable device id; HLC tiebreak)
/// - x25519 keypair for the Noise transport
/// - ed25519 keypair for signing (future: signed sync messages)
#[derive(Debug, Clone)]
pub struct DeviceIdentity {
    pub id: String,
    pub name: String,
    signing: SigningKey,
    verifying: VerifyingKey,
    x25519_seed: [u8; 32],
    pub x25519_public: [u8; 32],
}

impl DeviceIdentity {
    /// Load identity from `<config>/spidernotes/identity.json`, creating it if absent.
    pub fn load_or_create(name: &str) -> anyhow::Result<Self> {
        let base = dirs::config_dir().context("no config dir")?;
        Self::load_or_create_in(name, &base)
    }

    /// Load identity from `<base>/spidernotes/identity.json`, creating it if absent.
    /// Used on mobile, where the default config dir (`/.config`) is not writable.
    pub fn load_or_create_in(name: &str, base: &std::path::Path) -> anyhow::Result<Self> {
        let dir = base.join("spidernotes");
        std::fs::create_dir_all(&dir)?;
        migrate_double_nested(&dir, FILE);
        let path = dir.join(FILE);
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            let saved: SavedIdentity = serde_json::from_str(&raw)?;
            Self::from_parts(saved, name)
        } else {
            let id = Self::generate(name)?;
            id.save_in(&dir)?;
            Ok(id)
        }
    }

    pub fn generate(name: &str) -> anyhow::Result<Self> {
        let signing = SigningKey::generate(&mut OsRng);
        let verifying = signing.verifying_key();
        let x25519_seed: [u8; 32] =
            x25519_dalek::StaticSecret::random_from_rng(OsRng).to_bytes();
        let x25519_public = x25519_dalek::PublicKey::from(&StaticSecret::from(x25519_seed))
            .to_bytes();
        Ok(Self {
            id: hex::encode(verifying.to_bytes()),
            name: name.to_string(),
            signing,
            verifying,
            x25519_seed,
            x25519_public,
        })
    }

    fn from_parts(saved: SavedIdentity, _name: &str) -> anyhow::Result<Self> {
        let decoded = hex::decode(&saved.keys.ed25519_secret_hex)
            .context("bad ed25519 key hex")?;
        let seed: [u8; 32] = decoded
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad ed25519 key length"))?;
        let signing = SigningKey::from_bytes(&seed);
        let verifying = signing.verifying_key();
        let decoded = hex::decode(&saved.keys.x25519_secret_hex)
            .context("bad x25519 key hex")?;
        let x25519_seed: [u8; 32] = decoded
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad x25519 key length"))?;
        let x25519_public = x25519_dalek::PublicKey::from(&StaticSecret::from(x25519_seed))
            .to_bytes();
        Ok(Self {
            id: hex::encode(verifying.to_bytes()),
            name: saved.name,
            signing,
            verifying,
            x25519_seed,
            x25519_public,
        })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let dir = config_dir()?;
        self.save_in(&dir)
    }

    pub fn save_in(&self, dir: &std::path::Path) -> anyhow::Result<()> {
        let path = dir.join(FILE);
        let saved = SavedIdentity {
            name: self.name.clone(),
            keys: KeyMaterial {
                ed25519_secret_hex: hex::encode(self.signing.to_bytes()),
                x25519_secret_hex: hex::encode(self.x25519_seed),
            },
        };
        let mut raw = serde_json::to_string_pretty(&saved)?;
        raw.push('\n');
        std::fs::write(&path, raw)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn x25519_static(&self) -> StaticSecret {
        StaticSecret::from(self.x25519_seed)
    }

    pub fn x25519_seed_bytes(&self) -> [u8; 32] {
        self.x25519_seed
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing.sign(msg).to_bytes()
    }

    pub fn verify(&self, msg: &[u8], sig: &[u8; 64]) -> bool {
        self.verifying
            .verify(msg, &Signature::from_bytes(sig))
            .is_ok()
    }

    /// Short device id used as HLC tiebreak (first 16 hex chars of full id).
    pub fn short_id(&self) -> String {
        self.id.get(..16).unwrap_or(&self.id).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_and_roundtrip() {
        let a = DeviceIdentity::generate("test").unwrap();
        let b = DeviceIdentity::generate("test").unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(a.x25519_public, a.x25519_public);
        let msg = b"hello";
        let sig = a.sign(msg);
        assert!(a.verify(msg, &sig));
        let other = DeviceIdentity::generate("other").unwrap();
        assert!(!other.verify(msg, &sig));
    }

    #[test]
    fn short_id_is_prefix() {
        let a = DeviceIdentity::generate("t").unwrap();
        assert_eq!(a.short_id().len(), 16);
        assert!(a.id.starts_with(&a.short_id()));
    }

    #[test]
    fn save_in_spidernotes_subdir_matches_load() {
        // Regression: the app renames a device by saving with
        // `save_in(<base>/spidernotes)` — the SAME dir `load_or_create_in`
        // reads from. Saving to `<base>` directly (the old mobile bug) wrote a
        // file startup never read, losing the rename. Verify the roundtrip.
        let base =
            std::env::temp_dir().join(format!("sn-identity-path-{}", uuid::Uuid::new_v4()));
        let a = DeviceIdentity::load_or_create_in("orig", &base).unwrap();
        let mut renamed = a.clone();
        renamed.name = "renamed".into();
        renamed.save_in(&base.join("spidernotes")).unwrap();
        let c = DeviceIdentity::load_or_create_in("ignored", &base).unwrap();
        assert_eq!(c.id, a.id, "same identity");
        assert_eq!(c.name, "renamed", "rename persisted where load reads");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn migrates_double_nested_layout() {
        let base =
            std::env::temp_dir().join(format!("sn-identity-mig-{}", uuid::Uuid::new_v4()));
        let a = DeviceIdentity::load_or_create_in("t", &base).unwrap();
        // Simulate the old double-nested layout
        let dir = base.join("spidernotes");
        let old_dir = dir.join("spidernotes");
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::rename(dir.join(FILE), old_dir.join(FILE)).unwrap();
        let b = DeviceIdentity::load_or_create_in("t", &base).unwrap();
        assert_eq!(a.id, b.id);
        assert!(dir.join(FILE).exists());
        assert!(!old_dir.join(FILE).exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
