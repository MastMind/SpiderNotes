//! Sync conflicts: peer versions that could not be auto-merged with the
//! local version of the same note.
//!
//! A conflict is never a silent loss: the local version stays live in the
//! note file, and the peer's version is kept here (metadata in
//! `conflicts.json`, content in a per-conflict sidecar) until the user
//! resolves it ("keep this version" / "keep the other one").

use crate::hlc::Hlc;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A pending sync conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// Unique id (a v4 uuid); also the sidecar file name.
    pub id: String,
    /// The note this conflict belongs to.
    pub note_uuid: Uuid,
    /// The note's path when the conflict was recorded (display only — the
    /// note may be renamed or deleted before the conflict is resolved).
    pub note_path: String,
    /// Friendly name of the device that sent the conflicting version.
    pub peer_name: String,
    /// The (short) device id of the conflicting version (from its HLC).
    pub peer_id: String,
    /// When the conflict was recorded (unix ms).
    pub created_ms: u64,
    /// Hash of our live version at the moment of the conflict.
    pub our_hash: String,
    /// Hash of the peer's version. Together with `note_uuid` this is the
    /// dedup key: re-syncing the same version never stacks duplicates.
    pub their_hash: String,
    /// The peer version's HLC (re-absorbed when resolving "keep theirs").
    pub their_hlc: Hlc,
    /// Title/preview of the peer version, for the list row.
    #[serde(default)]
    pub their_title: String,
    #[serde(default)]
    pub their_preview: String,
}

/// On-disk shape of `conflicts.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    conflicts: Vec<Conflict>,
    /// (note uuid, peer-version hash) pairs the user already rejected with
    /// "keep ours". The peer keeps holding that newer version, so it arrives
    /// again on every sync — without the suppression list the same conflict
    /// would just be re-created forever.
    #[serde(default)]
    suppressed: Vec<Suppressed>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Suppressed {
    note_uuid: Uuid,
    their_hash: String,
}

#[derive(Debug, Clone, Default)]
pub struct ConflictStore {
    /// Vault root; the files live under `.spidernotes/`.
    root: PathBuf,
    items: Vec<Conflict>,
    suppressed: Vec<Suppressed>,
}

impl ConflictStore {
    /// Load the store from `<vault>/.spidernotes/conflicts.json`
    /// (missing or broken file = no conflicts).
    pub fn open(vault_root: &Path) -> anyhow::Result<Self> {
        let p = vault_root.join(crate::index::Index::DIR).join("conflicts.json");
        let f: File = std::fs::read_to_string(&p)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Ok(Self {
            root: vault_root.to_path_buf(),
            items: f.conflicts,
            suppressed: f.suppressed,
        })
    }

    /// A store that never persists (tests).
    pub fn memory() -> Self {
        Self::default()
    }

    pub fn list(&self) -> &[Conflict] {
        &self.items
    }

    pub fn get(&self, id: &str) -> Option<&Conflict> {
        self.items.iter().find(|c| c.id == id)
    }

    /// Add a conflict. Returns `false` (and adds nothing) when a conflict
    /// with the same note + peer version already exists, or when the user
    /// already rejected that version ("keep ours") — a peer re-sending the
    /// same version must not create duplicates or resurrect rejected ones.
    pub fn add(&mut self, c: Conflict) -> bool {
        let seen = |x: &Uuid, h: &str| x == &c.note_uuid && h == &c.their_hash;
        let dup = self.items.iter().any(|x| seen(&x.note_uuid, &x.their_hash))
            || self.suppressed.iter().any(|x| seen(&x.note_uuid, &x.their_hash));
        if dup {
            return false;
        }
        self.items.push(c);
        true
    }

    /// Remember that the user rejected this peer version ("keep ours"), so
    /// re-syncs of the same version do not recreate the conflict.
    pub fn suppress(&mut self, note_uuid: &Uuid, their_hash: &str) {
        if !self
            .suppressed
            .iter()
            .any(|x| &x.note_uuid == note_uuid && x.their_hash == their_hash)
        {
            self
                .suppressed
                .push(Suppressed { note_uuid: *note_uuid, their_hash: their_hash.into() });
        }
    }

    /// Remove a conflict by id (also deletes its content sidecar).
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|c| c.id != id);
        if self.items.len() == before {
            return false;
        }
        let _ = std::fs::remove_file(self.sidecar_path(id));
        true
    }

    /// Drop every conflict belonging to a note (the note was deleted).
    /// Returns the number removed.
    pub fn clear_for_note(&mut self, note_uuid: &Uuid) -> usize {
        let before = self.items.len();
        self.items
            .iter()
            .filter(|c| c.note_uuid == *note_uuid)
            .for_each(|c| {
                let _ = std::fs::remove_file(self.sidecar_path(&c.id));
            });
        self.items.retain(|c| c.note_uuid != *note_uuid);
        self.suppressed.retain(|x| x.note_uuid != *note_uuid);
        before - self.items.len()
    }

    /// Persist the list (atomic-ish, 0600 like the trust list).
    pub fn save(&self) -> anyhow::Result<()> {
        let p = self
            .root
            .join(crate::index::Index::DIR)
            .join("conflicts.json");
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("mkdir {}", dir.display()))?;
        }
        let f = File {
            conflicts: self.items.clone(),
            suppressed: self.suppressed.clone(),
        };
        let mut raw = serde_json::to_string_pretty(&f)?;
        raw.push('\n');
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, &p)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// The peer's version stored with the conflict.
    pub fn read_their(&self, c: &Conflict) -> anyhow::Result<String> {
        std::fs::read_to_string(self.sidecar_path(&c.id))
            .with_context(|| format!("read conflict sidecar {}", c.id))
    }

    /// Store the peer's version (0600: it is the user's note content).
    pub fn write_sidecar(&self, id: &str, content: &str) -> anyhow::Result<()> {
        let p = self.sidecar_path(id);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("mkdir {}", dir.display()))?;
        }
        std::fs::write(&p, content)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    fn sidecar_path(&self, id: &str) -> PathBuf {
        self.root
            .join(crate::index::Index::DIR)
            .join("conflicts")
            .join(format!("{id}.md"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct T(std::path::PathBuf);
    impl T {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!(
                "spidernotes-conf-{}-{}",
                std::process::id(),
                Uuid::new_v4()
            ));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for T {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mk(id: &str, uuid: Uuid, their_hash: &str) -> Conflict {
        Conflict {
            id: id.into(),
            note_uuid: uuid,
            note_path: "n.md".into(),
            peer_name: "peer".into(),
            peer_id: "peer".into(),
            created_ms: 1,
            our_hash: "o".into(),
            their_hash: their_hash.into(),
            their_hlc: Hlc::from_parts(1, 0, "peer".into()),
            their_title: String::new(),
            their_preview: String::new(),
        }
    }

    #[test]
    fn add_dedup_and_remove() {
        let t = T::new();
        let mut s = ConflictStore::open(&t.0).unwrap();
        let u = Uuid::new_v4();
        assert!(s.add(mk("a", u, "h1")));
        // same note + same peer version: duplicate
        assert!(!s.add(mk("b", u, "h1")));
        // same note, different version: new conflict
        assert!(s.add(mk("c", u, "h2")));
        // other note, same version: new conflict
        assert!(s.add(mk("d", Uuid::new_v4(), "h1")));
        assert_eq!(s.list().len(), 3);
        assert!(s.remove("a"));
        assert!(!s.remove("a"));
        assert_eq!(s.list().len(), 2);
    }

    #[test]
    fn suppressed_version_does_not_come_back() {
        let t = T::new();
        let mut s = ConflictStore::open(&t.0).unwrap();
        let u = Uuid::new_v4();
        assert!(s.add(mk("a", u, "h1")));
        // "keep ours": the conflict goes away, the version is suppressed
        s.remove("a");
        s.suppress(&u, "h1");
        s.save().unwrap();
        // re-sync of the same version: no new conflict
        let mut s2 = ConflictStore::open(&t.0).unwrap();
        assert!(!s2.add(mk("b", u, "h1")));
        assert!(s2.list().is_empty());
        // a DIFFERENT version of the same note is still a conflict
        assert!(s2.add(mk("c", u, "h2")));
    }

    #[test]
    fn persists_and_roundtrips() {
        let t = T::new();
        let mut s = ConflictStore::open(&t.0).unwrap();
        let u = Uuid::new_v4();
        assert!(s.add(mk("a", u, "h1")));
        s.write_sidecar("a", "their content").unwrap();
        s.save().unwrap();

        let mut s2 = ConflictStore::open(&t.0).unwrap();
        assert_eq!(s2.list().len(), 1);
        let c = s2.list()[0].clone();
        assert_eq!(c.id, "a");
        assert_eq!(s2.read_their(&c).unwrap(), "their content");

        // clearing for the note drops the record and the sidecar
        assert_eq!(s2.clear_for_note(&u), 1);
    }
}
