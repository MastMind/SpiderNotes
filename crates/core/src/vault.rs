use crate::frontmatter;
use crate::hlc::Hlc;
use crate::index::Index;
use crate::note::{body_hash, Note, NoteSummary};
use crate::{DeviceIdentity, NoteUpdate};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A SpiderNotes vault: a directory of plain Markdown files + `.spidernotes/` index.
#[derive(Clone)]
pub struct Vault {
    root: PathBuf,
    index: Index,
    device: DeviceIdentity,
}

impl Vault {
    /// Open (or initialize) a vault at `root`.
    /// Runs a filesystem scan to repair the index and assign ids to new files.
    pub fn open(root: impl Into<PathBuf>, device: DeviceIdentity) -> Result<Self> {
        let root = root.into();
        if !root.is_dir() {
            std::fs::create_dir_all(&root)
                .with_context(|| format!("cannot create vault dir {}", root.display()))?;
        }
        let mut index = Index::load(&root)?;
        if index.device.is_none() {
            index.device = Some(crate::index::DeviceMeta {
                id: device.short_id(),
                name: device.name.clone(),
            });
        }
        let mut v = Self {
            root: root.clone(),
            index,
            device,
        };
        v.scan_and_repair()
            .context("vault scan failed")?;
        Ok(v)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn device(&self) -> &DeviceIdentity {
        &self.device
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    /// All known notes (live + tombstones) with their current summaries.
    pub fn notes(&self) -> Vec<NoteSummary> {
        let mut v: Vec<NoteSummary> = self.index.notes.values().cloned().collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }

    pub fn live_notes(&self) -> Vec<NoteSummary> {
        self.notes().into_iter().filter(|n| !n.deleted).collect()
    }

    pub fn get(&self, uuid: &Uuid) -> Option<NoteSummary> {
        self.index.notes.get(uuid).cloned()
    }

    /// Read full note content for a uuid.
    pub fn read(&self, uuid: &Uuid) -> Result<Note> {
        let s = self
            .index
            .notes
            .get(uuid)
            .ok_or_else(|| anyhow::anyhow!("unknown note {uuid}"))?
            .clone();
        if s.deleted {
            bail!("note {uuid} is deleted");
        }
        let path = self.abs_path(&s.path);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        Ok(Note {
            uuid: *uuid,
            path: s.path,
            content,
            hlc: s.hlc,
            deleted: false,
        })
    }

    /// Create or overwrite a note at a relative path.
    /// If the path is known, keeps its uuid; otherwise assigns a fresh one.
    /// Returns the updated summary.
    pub fn write(&mut self, rel_path: &str, content: &str) -> Result<NoteSummary> {
        let rel = normalize_rel_path(rel_path)?;
        let hlc = self.next_clock();
        let uuid = self
            .find_by_path(&rel)
            .map(|s| s.uuid)
            .unwrap_or_else(Uuid::new_v4);

        let with_id = frontmatter::ensure_id(content, &uuid.to_string());
        let abs = self.abs_path(&rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        std::fs::write(&abs, with_id.as_bytes())
            .with_context(|| format!("write {}", abs.display()))?;

        let summary = NoteSummary {
            uuid,
            path: rel.clone(),
            hlc,
            hash: body_hash(content),
            deleted: false,
        };
        self.index.notes.insert(uuid, summary.clone());
        // a local write is the newest state on the path: any other live uuid
        // at this path (split from disconnected edits) loses now
        self.reconcile_path(&rel)?;
        self.save_index()?;
        Ok(summary)
    }

    /// Delete a note: removes the file and stores a tombstone.
    pub fn delete(&mut self, uuid: &Uuid) -> Result<NoteSummary> {
        let s = self
            .index
            .notes
            .get(uuid)
            .ok_or_else(|| anyhow::anyhow!("unknown note {uuid}"))?
            .clone();
        let abs = self.abs_path(&s.path);
        if abs.exists() {
            std::fs::remove_file(&abs).with_context(|| format!("rm {}", abs.display()))?;
        }
        let hlc = self.next_clock();
        let summary = NoteSummary {
            uuid: *uuid,
            path: s.path.clone(),
            hlc,
            hash: String::new(),
            deleted: true,
        };
        self.index.notes.insert(*uuid, summary.clone());
        // deleting a note kills the whole path, not just this uuid
        self.reconcile_path(&s.path)?;
        self.save_index()?;
        Ok(summary)
    }

    /// Move/rename a note (uuid preserved).
    pub fn rename(&mut self, uuid: &Uuid, new_rel_path: &str) -> Result<NoteSummary> {
        let s = self
            .index
            .notes
            .get(uuid)
            .ok_or_else(|| anyhow::anyhow!("unknown note {uuid}"))?
            .clone();
        let new_rel = normalize_rel_path(new_rel_path)?;
        let old_abs = self.abs_path(&s.path);
        let new_abs = self.abs_path(&new_rel);
        if let Some(parent) = new_abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&old_abs, &new_abs)
            .with_context(|| format!("rename {} -> {}", old_abs.display(), new_abs.display()))?;
        let hlc = self.next_clock();
        let summary = NoteSummary {
            uuid: *uuid,
            path: new_rel.clone(),
            hlc,
            hash: s.hash,
            deleted: false,
        };
        self.index.notes.insert(*uuid, summary.clone());
        // the rename is the newest state on the destination path
        self.reconcile_path(&new_rel)?;
        self.save_index()?;
        Ok(summary)
    }

    /// Search note bodies (case-insensitive substring). Returns matching summaries.
    pub fn search(&self, query: &str) -> Vec<NoteSummary> {
        let q = query.to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.live_notes()
            .into_iter()
            .filter(|s| {
                let abs = self.abs_path(&s.path);
                std::fs::read_to_string(&abs)
                    .map(|c| c.to_lowercase().contains(&q))
                    .unwrap_or(false)
            })
            .collect()
    }

  /// Apply a full note update (from a peer or the server) into the vault.
   /// LWW at two levels, both strict:
   /// - uuid level: the update must be newer than our record of the same uuid;
   /// - path level: the update must beat every other LIVE note at the same
   ///   path. The path level exists because two disconnected devices can
   ///   create the same path with different uuids ("split"); without it both
   ///   uuids would stay live forever and the file content would depend on
   ///   application order (permanent desync, visible as duplicates).
   /// Returns `Ok(true)` if the update became the path winner.
    pub fn apply_update(&mut self, u: &NoteUpdate) -> Result<bool> {
        let mut incoming = NoteSummary {
            uuid: u.uuid,
            path: u.path.clone(),
            hlc: u.hlc.clone(),
            hash: u.hash.clone().unwrap_or_default(),
            deleted: u.deleted,
        };
        let beats_uuid = crate::merge::remote_should_apply(self.index.notes.get(&u.uuid), &incoming);
        let beats_path = self
            .index
            .notes
            .iter()
            .filter(|(_, s)| s.path == u.path && s.uuid != u.uuid && !s.deleted)
            .all(|(_, s)| crate::merge::a_beats_b(&incoming, s));

        if !beats_uuid || !beats_path {
            // Loses: if we did not know this uuid, remember it as a
            // tombstone (at the received HLC, so it cannot outlive the
            // path winner) instead of re-encountering it on every sync.
            if self.index.notes.get(&u.uuid).is_none() {
                self.index.notes.insert(
                    u.uuid,
                    NoteSummary {
                        uuid: u.uuid,
                        path: u.path.clone(),
                        hlc: u.hlc.clone(),
                        hash: String::new(),
                        deleted: true,
                    },
                );
                self.save_index()?;
            }
            return Ok(false);
        }

        self.absorb_clock(&u.hlc);
        if u.deleted {
            let abs = self.abs_path(&u.path);
            if abs.exists() {
                let _ = std::fs::remove_file(&abs);
            }
            self.index.notes.insert(u.uuid, incoming);
        } else {
            let content = u
                .content
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("update for {} has no content", u.uuid))?;
            let with_id = frontmatter::ensure_id(content, &u.uuid.to_string());
            let abs = self.abs_path(&u.path);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&abs, with_id.as_bytes())?;
            incoming.hash = u.hash.clone().unwrap_or_else(|| body_hash(content));
            self.index.notes.insert(u.uuid, incoming);
        }
        self.reconcile_path(&u.path)?;
        self.save_index()?;
        Ok(true)
    }

    /// Enforce the path invariant: at most one LIVE note per path. The live
    /// note is the one with the highest HLC among all entries at the path;
    /// every other live entry is tombstoned (keeping its HLC, so the tombstone
    /// can never outvote the winner). If the winner is a tombstone, the file
    /// goes away.
    pub fn reconcile_path(&mut self, path: &str) -> Result<()> {
        let entries: Vec<(Uuid, NoteSummary)> = self
            .index
            .notes
            .iter()
            .filter(|(_, s)| s.path == path)
            .map(|(u, s)| (*u, s.clone()))
            .collect();
        if entries.len() <= 1 {
            return Ok(());
        }
        let Some((winner_uuid, winner)) = entries
            .iter()
            .max_by(|a, b| a.1.hlc.cmp(&b.1.hlc))
        else {
            return Ok(());
        };
        for (u, s) in &entries {
            if *u != *winner_uuid && !s.deleted {
                if let Some(l) = self.index.notes.get_mut(u) {
                    l.deleted = true;
                    l.hash = String::new();
                }
            }
        }
        if winner.deleted {
            let abs = self.abs_path(path);
            if abs.exists() {
                let _ = std::fs::remove_file(&abs);
            }
        }
        Ok(())
    }

    /// Scan the filesystem and reconcile the index:
    /// - new `.md` files get a uuid (written into frontmatter)
    /// - files that disappeared become tombstones
    /// - hashes are refreshed
    pub fn scan_and_repair(&mut self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        let mut changed = false;

        for entry in walk_md(&self.root)? {
            let rel = to_rel_slash(&entry, &self.root)?;
            seen.insert(rel.clone());
            let abs = self.root.join(&entry);
            let content = match std::fs::read_to_string(&abs) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("skip unreadable {}: {e}", abs.display());
                    continue;
                }
            };

        // the index owner of this path: prefer the live note with the
        // highest HLC, else any entry (a tombstone)
        let existing = self
            .index
            .notes
            .values()
            .filter(|s| s.path == rel)
            .max_by(|a, b| {
                (b.deleted as u8, b.hlc.clone()).cmp(&(a.deleted as u8, a.hlc.clone()))
            })
            .cloned();
            let Some(existing) = existing else {
                // a manually added file may already carry an id in its
                // frontmatter (copied from another device) — keep it
                let uuid = frontmatter::extract_id(&content)
                    .and_then(|id| id.parse::<Uuid>().ok())
                    .unwrap_or_else(Uuid::new_v4);
                let with_id = frontmatter::ensure_id(&content, &uuid.to_string());
                std::fs::write(&abs, with_id.as_bytes())?;
                let hlc = self.next_clock();
                let summary = NoteSummary {
                    uuid,
                    path: rel.clone(),
                    hlc,
                    hash: body_hash(&content),
                    deleted: false,
                };
                self.index.notes.insert(uuid, summary);
                tracing::info!("indexed new note {}", rel);
                changed = true;
                continue;
            };

            if existing.deleted {
                // file reappeared -> un-tombstone
                let hlc = self.next_clock();
                let summary = NoteSummary {
                    uuid: existing.uuid,
                    path: rel.clone(),
                    hlc,
                    hash: body_hash(&content),
                    deleted: false,
                };
                self.index.notes.insert(existing.uuid, summary);
                changed = true;
            }
        }

        // tombstone files that vanished
        let gone: Vec<(Uuid, String)> = self
            .index
            .notes
            .values()
            .filter(|s| !s.deleted && !seen.contains(&s.path))
            .map(|s| (s.uuid, s.path.clone()))
            .collect();
        for (u, path) in gone {
            let hlc = self.next_clock();
            if let Some(s) = self.index.notes.get_mut(&u) {
                s.deleted = true;
                s.hash = String::new();
                s.hlc = hlc;
                tracing::info!("tombstoned {} (file gone)", path);
                changed = true;
            }
        }

        if changed {
            self.save_index()?;
        }
        Ok(())
    }

    /// Advance and persist the device clock, returning the new value.
    fn next_clock(&mut self) -> Hlc {
        let dev_id = self
            .index
            .device
            .as_ref()
            .map(|d| d.id.clone())
            .unwrap_or_else(|| self.device.short_id());
        let mut hlc = self
            .index
            .device_clock
            .clone()
            .unwrap_or_else(|| Hlc::now(dev_id));
        hlc.tick();
        self.index.device_clock = Some(hlc.clone());
        hlc
    }

    /// Absorb a remote timestamp into the device clock (HLC receive).
    fn absorb_clock(&mut self, remote: &Hlc) {
        let dev_id = self
            .index
            .device
            .as_ref()
            .map(|d| d.id.clone())
            .unwrap_or_else(|| self.device.short_id());
        let mut hlc = self
            .index
            .device_clock
            .clone()
            .unwrap_or_else(|| Hlc::now(dev_id));
        hlc.receive(remote);
        self.index.device_clock = Some(hlc);
    }

    fn find_by_path(&self, rel: &str) -> Option<NoteSummary> {
        // the path owner is the live note with the highest HLC
        self.index
            .notes
            .values()
            .filter(|s| s.path == rel && !s.deleted)
            .max_by(|a, b| a.hlc.cmp(&b.hlc))
            .cloned()
    }

    fn abs_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn save_index(&mut self) -> Result<()> {
        self.index.save(&self.root)
    }
}

/// Normalize a relative path: strip leading `/`, use `/` separators, forbid `..`.
pub fn normalize_rel_path(p: &str) -> Result<String> {
    let trimmed = p.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        bail!("empty path");
    }
    for comp in trimmed.split('/') {
        if comp == ".." {
            bail!("path component '..' not allowed: {p}");
        }
    }
    if !trimmed.ends_with(".md") {
        bail!("only .md notes are supported: {p}");
    }
    Ok(trimmed.to_string())
}

fn walk_md(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        if p.file_name().map(|n| n == ".spidernotes").unwrap_or(false) {
            continue;
        }
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
    Ok(())
}

fn to_rel_slash(p: &Path, root: &Path) -> Result<String> {
    let rel = p.strip_prefix(root).context("strip prefix")?;
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "spidernotes-vault-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    struct Guard(PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn mk_vault() -> (Vault, Guard) {
        let dir = tmpdir();
        let dev = DeviceIdentity::generate("test-device").unwrap();
        let v = Vault::open(&dir, dev).unwrap();
        (v, Guard(dir))
    }

    #[test]
    fn split_path_converges_to_single_winner() {
        // Two disconnected devices create the same path with different uuids
        // (the "triangle" split). After exchanging updates, both must end up
        // with exactly ONE live note at the path: the later write.
        let (mut a, ga) = mk_vault();
        let sa = a.write("shared.md", "A version").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (mut b, gb) = mk_vault();
        let sb = b.write("shared.md", "B version").unwrap();
        assert_ne!(sa.uuid, sb.uuid);
        assert!(sb.hlc > sa.hlc);

        // exchange each other's notes
        a.apply_update(&sb.to_update(Some("B version".into()))).unwrap();
        b.apply_update(&sa.to_update(Some("A version".into()))).unwrap();

        for (v, dir) in [(&a, &ga.0), (&b, &gb.0)] {
            let live_at_path: Vec<NoteSummary> = v
                .live_notes()
                .into_iter()
                .filter(|n| n.path == "shared.md")
                .collect();
            assert_eq!(live_at_path.len(), 1, "one live note per path");
            assert_eq!(live_at_path[0].uuid, sb.uuid, "later write wins");
            // the loser uuid is a tombstone
            assert!(v.get(&sa.uuid).unwrap().deleted);
            // the file holds the winner's content
            let raw = std::fs::read_to_string(dir.join("shared.md")).unwrap();
            assert!(raw.contains("B version"));
        }

        // a late, even older version of the loser must not resurrect it
        assert!(!a
            .apply_update(&sa.to_update(Some("A version late".into())))
            .unwrap());
        assert!(a.get(&sa.uuid).unwrap().deleted);
        assert_eq!(
            a.live_notes().iter().filter(|n| n.path == "shared.md").count(),
            1
        );
    }

    #[test]
    fn delete_kills_whole_path() {
        // deleting either uuid of a split path kills the path
        let (mut a, ga) = mk_vault();
        let sa = a.write("shared.md", "A version").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let (mut b, gb) = mk_vault();
        let sb = b.write("shared.md", "B version").unwrap();
        a.apply_update(&sb.to_update(Some("B version".into()))).unwrap();
        // A has both uuids live->reconciled to sb; delete the LOSER uuid
        a.delete(&sa.uuid).unwrap();
        let raw = std::fs::read_to_string(ga.0.join("shared.md"));
        assert!(raw.is_err(), "file gone after deleting any uuid of the path");
        assert!(a.get(&sb.uuid).unwrap().deleted);

        let _ = b;
        let _ = gb;
    }

    #[test]
    fn write_read_roundtrip() {
        let (mut v, _) = mk_vault();
        let s = v.write("notes/hello.md", "# Hello\n\nworld").unwrap();
        let n = v.read(&s.uuid).unwrap();
        // raw file content (frontmatter included)
        assert!(n.content.contains("# Hello\n\nworld"));
        assert!(n.content.contains("spidernotes-id:"));
        assert_eq!(v.live_notes().len(), 1);
    }

    #[test]
    fn write_same_path_keeps_uuid() {
        let (mut v, _) = mk_vault();
        let a = v.write("x.md", "v1").unwrap();
        let b = v.write("x.md", "v2").unwrap();
        assert_eq!(a.uuid, b.uuid);
        assert_ne!(a.hlc, b.hlc);
    }

    #[test]
    fn delete_tombstones() {
        let (mut v, _) = mk_vault();
        let s = v.write("x.md", "v1").unwrap();
        v.delete(&s.uuid).unwrap();
        let notes = v.notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].deleted);
        assert!(!v.root().join("x.md").exists());
    }

    #[test]
    fn rename_keeps_uuid() {
        let (mut v, _) = mk_vault();
        let s = v.write("a.md", "v1").unwrap();
        let r = v.rename(&s.uuid, "sub/b.md").unwrap();
        assert_eq!(r.uuid, s.uuid);
        assert_eq!(r.path, "sub/b.md");
        assert!(v.root().join("sub/b.md").exists());
    }

    #[test]
    fn scan_indexes_foreign_files() {
        let dir = tmpdir();
        let dev = DeviceIdentity::generate("d").unwrap();
        std::fs::write(dir.join("foreign.md"), "# Foreign").unwrap();
        std::fs::create_dir_all(dir.join("deep")).unwrap();
        std::fs::write(dir.join("deep/nested.md"), "nested").unwrap();
        let mut v = Vault::open(&dir, dev).unwrap();
        assert_eq!(v.live_notes().len(), 2);
        // frontmatter id was added to the files
        let c = std::fs::read_to_string(dir.join("foreign.md")).unwrap();
        assert!(c.contains("spidernotes-id:"));
        // removing a file tombstones it
        std::fs::remove_file(dir.join("foreign.md")).unwrap();
        v.scan_and_repair().unwrap();
        let gone = v
            .notes()
            .into_iter()
            .find(|n| n.path == "foreign.md")
            .unwrap();
        assert!(gone.deleted);
    }

    #[test]
    fn apply_update_writes_file() {
        let (mut v, g) = mk_vault();
        let u = NoteUpdate {
            uuid: Uuid::new_v4(),
            path: "from-peer.md".into(),
            hlc: Hlc::from_parts(123, 0, "peer".into()),
            hash: Some("h".into()),
            deleted: false,
            content: Some("# From peer".into()),
        };
        assert!(v.apply_update(&u).unwrap());
        let content = std::fs::read_to_string(g.0.join("from-peer.md")).unwrap();
        assert!(content.contains("# From peer"));
    }

    #[test]
    fn search_finds_text() {
        let (mut v, _) = mk_vault();
        v.write("a.md", "apple pie").unwrap();
        v.write("b.md", "banana").unwrap();
        let r = v.search("apple");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].path, "a.md");
    }
}
