use crate::conflict::{Conflict, ConflictStore};
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
    /// Versions a peer sent that could not be auto-merged (kept, never lost).
    conflicts: ConflictStore,
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
        let conflicts = ConflictStore::open(&root)
            .context("cannot load conflict store")?;
        let mut v = Self {
            root: root.clone(),
            index,
            device,
            conflicts,
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

        let (title, preview) = crate::note::title_preview(content);
        let summary = NoteSummary {
            uuid,
            path: rel.clone(),
            hlc,
            hash: body_hash(content),
            deleted: false,
            title,
            preview,
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
        // the folder is intentionally kept when it becomes empty (same as
        // right after creation); the user removes it explicitly if wanted
        let hlc = self.next_clock();
        let summary = NoteSummary {
            uuid: *uuid,
            path: s.path.clone(),
            hlc,
            hash: String::new(),
            deleted: true,
            title: String::new(),
            preview: String::new(),
        };
        self.index.notes.insert(*uuid, summary.clone());
        // a deleted note has no shared base and no pending conflicts
        self.remove_base(uuid);
        self.conflicts.clear_for_note(uuid);
        self.conflicts.save()?;
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
        // the old folder is kept when the move empties it
        let hlc = self.next_clock();
        // content is unchanged by a move: carry the row fields over
        let summary = NoteSummary {
            uuid: *uuid,
            path: new_rel.clone(),
            hlc,
            hash: s.hash,
            deleted: false,
            title: s.title,
            preview: s.preview,
        };
        self.index.notes.insert(*uuid, summary.clone());
        // the rename is the newest state on the destination path
        self.reconcile_path(&new_rel)?;
        self.save_index()?;
        Ok(summary)
    }

    /// Create a (possibly nested) folder inside the vault.
    /// Empty folders are local-only: folders have no index entries, so a peer
    /// learns about a folder when a note is moved into it (or created there).
    pub fn create_folder(&mut self, rel_dir: &str) -> Result<String> {
        let rel = normalize_dir_rel_path(rel_dir)?;
        let abs = self.root.join(&rel);
        std::fs::create_dir_all(&abs)
            .with_context(|| format!("mkdir {}", abs.display()))?;
        Ok(rel)
    }

    /// Rename/move a folder: the directory on disk plus every index entry
    /// (live notes AND tombstones) whose path lies under it. Each moved entry
    /// gets a fresh HLC so the move lands in sync deltas like any edit.
    /// Returns the number of index entries moved.
    pub fn rename_folder(&mut self, old_rel: &str, new_rel: &str) -> Result<usize> {
        let old = normalize_dir_rel_path(old_rel)?;
        let new = normalize_dir_rel_path(new_rel)?;
        if old == new {
            bail!("folder already has that name");
        }
        if new.starts_with(&format!("{old}/")) {
            bail!("cannot move a folder into its own subtree: {new}");
        }
        let old_abs = self.root.join(&old);
        let new_abs = self.root.join(&new);
        if !old_abs.is_dir() {
            bail!("folder not found: {old}");
        }
        if new_abs.exists() {
            bail!("folder already exists: {new}");
        }
        if let Some(parent) = new_abs.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
        }
        std::fs::rename(&old_abs, &new_abs)
            .with_context(|| format!("rename folder {old} -> {new}"))?;

        let old_prefix = format!("{old}/");
        let new_prefix = format!("{new}/");
        let moved: Vec<(Uuid, NoteSummary)> = self
            .index
            .notes
            .iter()
            .filter(|(_, s)| s.path.starts_with(&old_prefix))
            .map(|(u, s)| (*u, s.clone()))
            .collect();
        let count = moved.len();
        for (u, s) in moved {
            let new_path = format!("{}{}", new_prefix, &s.path[old_prefix.len()..]);
            let hlc = self.next_clock();
              self.index.notes.insert(
                u,
                NoteSummary {
                    uuid: u,
                    path: new_path,
                    hlc,
                    hash: s.hash,
                    deleted: s.deleted,
                    // a folder move does not touch the content
                    title: s.title,
                    preview: s.preview,
                },
            );
        }
        self.save_index()?;
        Ok(count)
    }

    /// Delete a folder: every note under it is deleted (file removed +
    /// tombstone, like a single-note delete, so the deletion propagates),
    /// then the directory tree is removed from disk. Returns the deleted
    /// summaries.
    pub fn delete_folder(&mut self, rel_dir: &str) -> Result<Vec<NoteSummary>> {
        let rel = normalize_dir_rel_path(rel_dir)?;
        let prefix = format!("{rel}/");
        let doomed: Vec<Uuid> = self
            .index
            .notes
            .iter()
            .filter(|(_, s)| !s.deleted && s.path.starts_with(&prefix))
            .map(|(u, _)| *u)
            .collect();
        let mut out = Vec::new();
        for u in doomed {
            let s = self.delete(&u)?;
            out.push(s);
        }
        let abs = self.root.join(&rel);
        if abs.exists() {
            std::fs::remove_dir_all(&abs)
                .with_context(|| format!("rmdir {}", abs.display()))?;
        }
        Ok(out)
    }

    /// All vault folders as relative slash-separated paths, sorted.
    pub fn list_folders(&self) -> Vec<String> {
        let mut out = Vec::new();
        walk_dirs(&self.root, &self.root, &mut out);
        out.sort();
        out
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
    ///
    /// When both sides hold the same live note and both diverged from the
    /// last version they shared (the base sidecar), the versions are
    /// combined with a three-way merge instead of a silent LWW drop:
    /// disjoint edits are auto-merged, overlapping edits become a CONFLICT —
    /// our version stays live and the peer's version is kept in the
    /// conflict store until the user resolves it (never a silent loss).
    /// `peer_name` (the sender's friendly name) is recorded with conflicts.
    /// Returns `Ok(true)` if the update — or its merged result — became the
    /// path winner.
     pub fn apply_update(&mut self, u: &NoteUpdate, peer_name: &str) -> Result<bool> {
        // title/preview are filled in below when the content arrives
        let mut incoming = NoteSummary {
            uuid: u.uuid,
            path: u.path.clone(),
            hlc: u.hlc.clone(),
            hash: u.hash.clone().unwrap_or_default(),
            deleted: u.deleted,
            title: String::new(),
            preview: String::new(),
        };
        let beats_uuid = crate::merge::remote_should_apply(self.index.notes.get(&u.uuid), &incoming);
        let beats_path = self
            .index
            .notes
            .iter()
            .filter(|(_, s)| s.path == u.path && s.uuid != u.uuid && !s.deleted)
            .all(|(_, s)| crate::merge::a_beats_b(&incoming, s));

        if !beats_uuid || !beats_path {
            // Loses LWW. The peer's version is stale, but it may still
            // contain edits we do not have: when both sides diverged from
            // the shared base, combine them (the merged version gets a fresh
            // HLC that beats both). A conflicting merge keeps ours live and
            // records the peer's version (threeway does that).
            let merged = match self.threeway(u, peer_name)? {
                MergeOutcome::Merged(m) => Some(m),
                _ => None,
            };
            if let Some(m) = merged {
                self.absorb_clock(&u.hlc);
                self.write_merged(u, &m)?;
                return Ok(true);
            }
            // Otherwise: if we did not know this uuid, remember it as a
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
                        title: String::new(),
                        preview: String::new(),
                    },
                );
                self.save_index()?;
            }
            return Ok(false);
        }

        self.absorb_clock(&u.hlc);
        // The note's PREVIOUS location (same uuid, different path) — a remote
        // move. The old file must go away here: if it stayed on disk with no
        // index entry pointing at its path, the next scan_and_repair would see
        // it as a "manually added file", read its frontmatter id (the same
        // uuid) and re-index it at the OLD path with a fresh (newest) HLC —
        // the note would fly back, overwriting the move, and flip on every
        // vault open.
        let prev = self.index.notes.get(&u.uuid).cloned();
        if u.deleted {
            let abs = self.abs_path(&u.path);
            if abs.exists() {
                let _ = std::fs::remove_file(&abs);
            }
            // emptied folders are kept (same as on the sender)
            self.index.notes.insert(u.uuid, incoming);
            // a deleted note has no shared base and no pending conflicts
            self.remove_base(&u.uuid);
            self.conflicts.clear_for_note(&u.uuid);
            self.conflicts.save()?;
        } else {
            let content = u
                .content
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("update for {} has no content", u.uuid))?;
            match self.threeway(u, peer_name)? {
                // We did not diverge (no base, or we still hold the shared
                // version): adopt the peer's version verbatim with its HLC.
                MergeOutcome::AsIs | MergeOutcome::None => {
                    let with_id = frontmatter::ensure_id(content, &u.uuid.to_string());
                    let abs = self.abs_path(&u.path);
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&abs, with_id.as_bytes())?;
                    incoming.hash = u.hash.clone().unwrap_or_else(|| body_hash(content));
                    (incoming.title, incoming.preview) = crate::note::title_preview(content);
                    self.index.notes.insert(u.uuid, incoming);
                    // both sides now share exactly this version
                    self.write_base(&u.uuid, content)?;
                }
                // Both sides diverged and the merge combined them: write the
                // merged content under a fresh HLC (the clock already absorbed
                // the peer's, so this beats both sides).
                MergeOutcome::Merged(m) => {
                    let hlc = self.next_clock();
                    let with_id = frontmatter::ensure_id(&m, &u.uuid.to_string());
                    let abs = self.abs_path(&u.path);
                    if let Some(parent) = abs.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&abs, with_id.as_bytes())?;
                    let (title, preview) = crate::note::title_preview(&m);
                    self.index.notes.insert(
                        u.uuid,
                        NoteSummary {
                            uuid: u.uuid,
                            path: u.path.clone(),
                            hlc,
                            hash: body_hash(&m),
                            deleted: false,
                            title,
                            preview,
                        },
                    );
                    // the peer holds its own (line-order-equivalent) merge
                    // result; it is the newest version both sides share
                    self.write_base(&u.uuid, &m)?;
                }
                // Overlapping edits: our version stays live, the peer's was
                // recorded in the conflict store (never silently dropped).
                // The update did NOT become the path winner.
                MergeOutcome::Conflict => {
                    self.save_index()?; // persist the absorbed clock
                    return Ok(false);
                }
            }
        }
        if let Some(p) = &prev {
            if p.path != u.path {
                let old_abs = self.abs_path(&p.path);
                if old_abs.exists() {
                    let _ = std::fs::remove_file(&old_abs);
                }
                // the folder the note moved out of is kept when empty
            }
        }
        self.reconcile_path(&u.path)?;
        self.save_index()?;
        Ok(true)
    }

    /// Three-way merge of our live version of note `u` with the incoming
    /// version, using the stored base (the last version both sides shared).
    fn threeway(&mut self, u: &NoteUpdate, peer_name: &str) -> Result<MergeOutcome> {
        let Some(p) = self.index.notes.get(&u.uuid).cloned() else {
            return Ok(MergeOutcome::None);
        };
        if p.deleted || p.path != u.path {
            return Ok(MergeOutcome::None);
        }
        let Some(theirs) = u.content.as_deref() else {
            return Ok(MergeOutcome::None);
        };
        let Some(base) = self.read_base(&u.uuid) else {
            // No common base recorded: plain LWW (the pre-merge behavior).
            return Ok(MergeOutcome::None);
        };
        let Some(ours) = std::fs::read_to_string(self.abs_path(&p.path)).ok() else {
            return Ok(MergeOutcome::None);
        };
        if ours == base {
            if theirs == ours {
                return Ok(MergeOutcome::None);
            }
            // We did not change since the shared version.
            return Ok(MergeOutcome::AsIs);
        }
        if theirs == base {
            // The peer sent back the shared version (stale resend).
            return Ok(MergeOutcome::None);
        }
        match crate::threeway::merge(&base, &ours, theirs) {
            crate::threeway::Merge::Ok(m) => {
                if m == ours {
                    Ok(MergeOutcome::None)
                } else {
                    Ok(MergeOutcome::Merged(m))
                }
            }
            crate::threeway::Merge::Conflict => {
                self.record_conflict(u, &p, theirs, peer_name);
                Ok(MergeOutcome::Conflict)
            }
        }
    }

    /// Write a clean merge result: fresh HLC (newer than both sides, the
    /// clock already absorbed the peer's) + base sidecar = the merge result.
    fn write_merged(&mut self, u: &NoteUpdate, merged: &str) -> Result<()> {
        let hlc = self.next_clock();
        let with_id = frontmatter::ensure_id(merged, &u.uuid.to_string());
        let abs = self.abs_path(&u.path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        std::fs::write(&abs, with_id.as_bytes())?;
        let (title, preview) = crate::note::title_preview(merged);
        self.index.notes.insert(
            u.uuid,
            NoteSummary {
                uuid: u.uuid,
                path: u.path.clone(),
                hlc,
                hash: body_hash(merged),
                deleted: false,
                title,
                preview,
            },
        );
        self.write_base(&u.uuid, merged)?;
        self.reconcile_path(&u.path)?;
        self.save_index()?;
        Ok(())
    }

    /// Keep the peer's version in the conflict store. Deduped by
    /// (note uuid, peer-version hash): a peer that re-sends the same version
    /// (its copy still wins LWW over ours) never stacks duplicates.
    fn record_conflict(
        &mut self,
        u: &NoteUpdate,
        p: &NoteSummary,
        their_content: &str,
        peer_name: &str,
    ) {
        let (title, preview) = crate::note::title_preview(their_content);
        let c = Conflict {
            id: Uuid::new_v4().to_string(),
            note_uuid: u.uuid,
            note_path: p.path.clone(),
            peer_name: peer_name.to_string(),
            peer_id: u.hlc.device.clone(),
            created_ms: crate::hlc::now_ms(),
            our_hash: p.hash.clone(),
            their_hash: body_hash(their_content),
            their_hlc: u.hlc.clone(),
            their_title: title,
            their_preview: preview,
        };
        if self.conflicts.add(c.clone()) {
            if let Err(e) = self.conflicts.write_sidecar(&c.id, their_content) {
                tracing::warn!("cannot store conflict version {id}: {e}", id = c.id);
            }
            if let Err(e) = self.conflicts.save() {
                tracing::warn!("cannot save conflict list: {e}");
            }
        }
    }

    /// The note's pending conflicts (for the UI).
    pub fn conflicts(&self) -> Vec<Conflict> {
        self.conflicts.list().to_vec()
    }

    /// The peer's version stored with a conflict.
    pub fn conflict_content(&self, id: &str) -> Result<String> {
        let c = self
            .conflicts
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("unknown conflict {id}"))?;
        self.conflicts.read_their(c)
    }

    /// Resolve a conflict by keeping OUR version: the note already holds it;
    /// drop the recorded peer version and suppress it — the peer still
    /// holds that (newer) version and will re-send it on every sync, so
    /// without suppression the same conflict would come back forever.
    pub fn resolve_conflict_keep_ours(&mut self, id: &str) -> Result<()> {
        let c = self
            .conflicts
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown conflict {id}"))?;
        self.conflicts.remove(id);
        self.conflicts.suppress(&c.note_uuid, &c.their_hash);
        self.conflicts.save()
    }

    /// Resolve a conflict by adopting the PEER's version: write it as a
    /// local edit (fresh HLC that beats the peer's), refresh the base, drop
    /// the conflict. If the note is still live it keeps its current path
    /// (it may have been renamed since the conflict); otherwise it is
    /// recreated where the conflict recorded it.
    pub fn resolve_conflict_keep_theirs(&mut self, id: &str) -> Result<()> {
        let c = self
            .conflicts
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown conflict {id}"))?;
        let content = self.conflicts.read_their(&c)?;
        let path = match self.index.notes.get(&c.note_uuid) {
            Some(s) if !s.deleted => s.path.clone(),
            _ => c.note_path.clone(),
        };
        self.absorb_clock(&c.their_hlc);
        let hlc = self.next_clock();
        let with_id = frontmatter::ensure_id(&content, &c.note_uuid.to_string());
        let abs = self.abs_path(&path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        std::fs::write(&abs, with_id.as_bytes())?;
        let (title, preview) = crate::note::title_preview(&content);
        self.index.notes.insert(
            c.note_uuid,
            NoteSummary {
                uuid: c.note_uuid,
                path: path.clone(),
                hlc,
                hash: body_hash(&content),
                deleted: false,
                title,
                preview,
            },
        );
        self.reconcile_path(&path)?;
        self.write_base(&c.note_uuid, &content)?;
        self.conflicts.remove(id);
        self.conflicts.save()?;
        self.save_index()?;
        Ok(())
    }

    /// The base sidecar: the last version of the note both sides shared —
    /// the reference point for three-way merges. `.spidernotes/base/<uuid>`.
    fn base_path(&self, uuid: &Uuid) -> PathBuf {
        self.root.join(Index::DIR).join("base").join(uuid.to_string())
    }

    fn read_base(&self, uuid: &Uuid) -> Option<String> {
        std::fs::read_to_string(self.base_path(uuid)).ok()
    }

    fn write_base(&self, uuid: &Uuid, content: &str) -> Result<()> {
        let p = self.base_path(uuid);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("mkdir {}", parent.display()))?;
        }
        std::fs::write(&p, content)?;
        Ok(())
    }

    /// Remember the last version both sides share (the merge base). Called
    /// when we send a version a peer will adopt: without it, the device that
    /// created a note would keep no base and a first divergent edit could be
    /// silently lost to LWW.
    pub fn set_base(&mut self, uuid: &Uuid, content: &str) -> Result<()> {
        self.write_base(uuid, content)
    }

    /// Notes where BOTH sides diverged from the last shared version (base):
    /// we hold a version different from the base, and the peer's summary
    /// differs from both our current hash and the base hash. These notes
    /// must be sent even when our version is OLDER than the peer's — so the
    /// peer can record the conflict (with the content) instead of silently
    /// letting its own version win.
    pub fn diverged_from_base(&self, peer: &[NoteSummary]) -> Vec<Uuid> {
        let mut out = Vec::new();
        for s in self.index.notes.values() {
            if s.deleted {
                continue;
            }
            let Some(base) = self.read_base(&s.uuid) else {
                continue;
            };
            let Ok(ours) = std::fs::read_to_string(self.abs_path(&s.path)) else {
                continue;
            };
            if ours == base {
                continue; // we still hold the shared version
            }
            let Some(p) = peer.iter().find(|p| p.uuid == s.uuid) else {
                continue; // peer has no record: it will adopt ours as-is
            };
            if p.hash == s.hash {
                continue; // identical content: nothing to combine
            }
            if p.hash == body_hash(&base) {
                continue; // peer still holds the shared version
            }
            out.push(s.uuid);
        }
        out
    }

    fn remove_base(&self, uuid: &Uuid) {
        let _ = std::fs::remove_file(self.base_path(uuid));
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
        let mut purged_conflicts = false;
        for (u, s) in &entries {
            if *u != *winner_uuid && !s.deleted {
                if let Some(l) = self.index.notes.get_mut(u) {
                    l.deleted = true;
                    l.hash = String::new();
                }
                // the losing split version has no shared base / conflicts left
                self.remove_base(u);
                if self.conflicts.clear_for_note(u) > 0 {
                    purged_conflicts = true;
                }
            }
        }
        if winner.deleted {
            let abs = self.abs_path(path);
            if abs.exists() {
                let _ = std::fs::remove_file(&abs);
            }
            self.remove_base(winner_uuid);
            if self.conflicts.clear_for_note(winner_uuid) > 0 {
                purged_conflicts = true;
            }
        }
        if purged_conflicts {
            self.conflicts.save()?;
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
                let (title, preview) = crate::note::title_preview(&content);
                let summary = NoteSummary {
                    uuid,
                    path: rel.clone(),
                    hlc,
                    hash: body_hash(&content),
                    deleted: false,
                    title,
                    preview,
                };
                self.index.notes.insert(uuid, summary);
                tracing::info!("indexed new note {}", rel);
                changed = true;
                continue;
            };

            if existing.deleted {
                // file reappeared -> un-tombstone
                let hlc = self.next_clock();
                let (title, preview) = crate::note::title_preview(&content);
                let summary = NoteSummary {
                    uuid: existing.uuid,
                    path: rel.clone(),
                    hlc,
                    hash: body_hash(&content),
                    deleted: false,
                    title,
                    preview,
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

/// What to do with an incoming version of a note we also hold.
enum MergeOutcome {
    /// Adopt the peer's version verbatim with its own HLC (we did not
    /// diverge from the shared base — or there is no base).
    AsIs,
    /// Write the merged content under a fresh HLC.
    Merged(String),
    /// Nothing to do: not the same live note, no base, or the versions
    /// are already identical.
    None,
    /// Overlapping edits: our version stays live, the peer's version was
    /// recorded in the conflict store.
    Conflict,
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

/// Normalize a relative folder path: strip leading/trailing `/`, forbid
/// `..`, `.`, empty components and the reserved index dir.
pub fn normalize_dir_rel_path(p: &str) -> Result<String> {
    let trimmed = p.trim().trim_matches('/');
    if trimmed.is_empty() {
        bail!("empty folder path");
    }
    for comp in trimmed.split('/') {
        if comp == ".." {
            bail!("path component '..' not allowed: {p}");
        }
        if comp == "." || comp.is_empty() {
            bail!("bad path component in folder: {p}");
        }
        if comp == Index::DIR {
            bail!("folder name {comp} is reserved");
        }
    }
    Ok(trimmed.to_string())
}

fn walk_dirs(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        if p.file_name().map(|n| n == Index::DIR).unwrap_or(false) {
            continue;
        }
        if let Ok(rel) = p.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
        walk_dirs(root, &p, out);
    }
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
        a.apply_update(&sb.to_update(Some("B version".into())), "b").unwrap();
        b.apply_update(&sa.to_update(Some("A version".into())), "a").unwrap();

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
            .apply_update(&sa.to_update(Some("A version late".into())), "a")
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
        a.apply_update(&sb.to_update(Some("B version".into())), "b").unwrap();
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
        assert!(v.apply_update(&u, "peer").unwrap());
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

    #[test]
    fn create_and_list_folders() {
        let (mut v, g) = mk_vault();
        v.create_folder("a/b").unwrap();
        v.create_folder("c").unwrap();
        assert_eq!(
            v.list_folders(),
            vec!["a".to_string(), "a/b".to_string(), "c".to_string()]
        );
        assert!(g.0.join("a/b").is_dir());
        // bad names are rejected
        assert!(v.create_folder("").is_err());
        assert!(v.create_folder("/").is_err());
        assert!(v.create_folder("a/../b").is_err());
        assert!(v.create_folder("a//b").is_err());
        assert!(v.create_folder(".spidernotes").is_err());
        assert!(v.create_folder("a/.spidernotes").is_err());
    }

    #[test]
    fn rename_folder_moves_notes_and_bumps_hlc() {
        let (mut v, g) = mk_vault();
        let s = v.write("work/a.md", "A").unwrap();
        let t = v.write("work/sub/b.md", "B").unwrap();
        let n = v.rename_folder("work", "office").unwrap();
        assert_eq!(n, 2);
        assert_eq!(v.get(&s.uuid).unwrap().path, "office/a.md");
        assert_eq!(v.get(&t.uuid).unwrap().path, "office/sub/b.md");
        assert!(v.get(&s.uuid).unwrap().hlc > s.hlc, "moved note must enter sync deltas");
        assert!(g.0.join("office/a.md").exists());
        assert!(!g.0.join("work").exists());
    }

    #[test]
    fn rename_folder_moves_tombstones_too() {
        let (mut v, g) = mk_vault();
        let s = v.write("old/a.md", "A").unwrap();
        v.write("old/b.md", "B").unwrap();
        v.delete(&s.uuid).unwrap();
        // the folder survives (b.md inside) and its tombstone moves with it
        v.rename_folder("old", "new").unwrap();
        let ts = v.get(&s.uuid).unwrap();
        assert!(ts.deleted);
        assert_eq!(ts.path, "new/a.md");
        assert!(g.0.join("new/b.md").exists());
        assert!(!g.0.join("old").exists());
    }

    #[test]
    fn delete_folder_removes_all_notes() {
        let (mut v, g) = mk_vault();
        let a = v.write("f/a.md", "A").unwrap();
        let b = v.write("f/nest/b.md", "B").unwrap();
        v.write("root.md", "R").unwrap();
        let deleted = v.delete_folder("f").unwrap();
        assert_eq!(deleted.len(), 2);
        assert!(v.get(&a.uuid).unwrap().deleted);
        assert!(v.get(&b.uuid).unwrap().deleted);
        assert!(!g.0.join("f").exists());
        assert!(g.0.join("root.md").exists());
    }

    #[test]
    fn moving_or_deleting_last_note_keeps_empty_dir() {
        let (mut v, g) = mk_vault();
        let s = v.write("dir/a.md", "A").unwrap();
        v.rename(&s.uuid, "top.md").unwrap();
        assert!(g.0.join("dir").exists(), "empty folder must stay after a move");
        let t = v.write("dir2/b.md", "B").unwrap();
        v.delete(&t.uuid).unwrap();
        assert!(g.0.join("dir2").exists(), "empty folder must stay after a delete");
    }

    #[test]
    fn remote_move_removes_old_file() {
        // A moves a note into a folder; B applies the update. The old file
        // must vanish on B and a rescan must NOT resurrect it at the old path
        // (frontmatter id would otherwise re-index it with a fresh HLC).
        let (mut a, _ga) = mk_vault();
        let s = a.write("x.md", "v1").unwrap();
        let (mut b, _gb) = mk_vault();
        b.apply_update(&s.to_update(Some("v1".into())), "a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let m = a.rename(&s.uuid, "dir/x.md").unwrap();
        b.apply_update(&m.to_update(Some("v1".into())), "a").unwrap();
        assert_eq!(b.get(&s.uuid).unwrap().path, "dir/x.md");
        let dir_b = b.root();
        assert!(!dir_b.join("x.md").exists(), "old file must be removed");
        assert!(dir_b.join("dir/x.md").exists());
        b.scan_and_repair().unwrap();
        assert_eq!(
            b.get(&s.uuid).unwrap().path,
            "dir/x.md",
            "rescan must not resurrect the old path"
        );
    }

    /// A "share" between two vaults: `a` sends the note at `path` to `b`
    /// (the way a sync would: a remembers the sent version as the merge
    /// base, b adopts it verbatim and stores it as its base too).
    fn share(a: &mut Vault, b: &mut Vault, path: &str) -> (Uuid, String) {
        let s = a
            .live_notes()
            .into_iter()
            .find(|n| n.path == path)
            .unwrap();
        let content = std::fs::read_to_string(a.root().join(path)).unwrap();
        a.set_base(&s.uuid, &content).unwrap();
        assert!(b.apply_update(&s.to_update(Some(content.clone())), "a").unwrap());
        (s.uuid, content)
    }

    #[test]
    fn disjoint_remote_edits_auto_merge() {
        let (mut a, ga) = mk_vault();
        let s = a.write("n.md", "l1\nl2\nl3\nl4").unwrap();
        let (mut b, gb) = mk_vault();
        share(&mut a, &mut b, "n.md");

        // both edit disjoint lines (B later -> B's version is newer)
        std::thread::sleep(std::time::Duration::from_millis(20));
        a.write("n.md", "L1\nl2\nl3\nl4").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let sb = b.write("n.md", "l1\nl2\nl3\nL4").unwrap();

        // A receives B's newer version: both diverged from the base ->
        // disjoint edits are combined, nothing is dropped
        let theirs = std::fs::read_to_string(gb.0.join("n.md")).unwrap();
        assert!(a
            .apply_update(&sb.to_update(Some(theirs.clone())), "b")
            .unwrap());
        let merged = std::fs::read_to_string(ga.0.join("n.md")).unwrap();
        assert!(merged.contains("L1") && merged.contains("L4"), "{merged}");
        assert!(a.conflicts().is_empty());

        // B receives A's merged version and converges to the same content
        let sa = a.get(&s.uuid).unwrap();
        assert!(b
            .apply_update(&sa.to_update(Some(merged.clone())), "a")
            .unwrap());
        let merged_b = std::fs::read_to_string(gb.0.join("n.md")).unwrap();
        assert!(
            merged_b.contains("L1") && merged_b.contains("L4"),
            "{merged_b}"
        );
        assert_eq!(s.uuid, b.get(&s.uuid).unwrap().uuid);
    }

    #[test]
    fn stale_remote_version_with_disjoint_edit_is_merged_not_dropped() {
        let (mut a, ga) = mk_vault();
        a.write("n.md", "l1\nl2\nl3").unwrap();
        let (mut b, gb) = mk_vault();
        share(&mut a, &mut b, "n.md");

        // B edits line 3; A edits line 1 LATER (A's version is newer)
        std::thread::sleep(std::time::Duration::from_millis(20));
        let sb = b.write("n.md", "l1\nl2\nL3b").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        a.write("n.md", "L1a\nl2\nl3").unwrap();

        // A receives B's OLDER version: it loses LWW, but its disjoint edit
        // must be merged in, not silently dropped
        let theirs = std::fs::read_to_string(gb.0.join("n.md")).unwrap();
        assert!(a.apply_update(&sb.to_update(Some(theirs)), "b").unwrap());
        let merged = std::fs::read_to_string(ga.0.join("n.md")).unwrap();
        assert!(merged.contains("L1a") && merged.contains("L3b"), "{merged}");
        assert!(a.conflicts().is_empty());
    }

    #[test]
    fn overlapping_edits_conflict_without_silent_loss() {
        let (mut a, ga) = mk_vault();
        let s = a.write("n.md", "a\nb\nc").unwrap();
        let (mut b, gb) = mk_vault();
        share(&mut a, &mut b, "n.md");

        // both rewrite the same line differently (B later -> B is newer)
        std::thread::sleep(std::time::Duration::from_millis(20));
        a.write("n.md", "a\nB1\nc").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let sb = b.write("n.md", "a\nB2\nc").unwrap();

        // A receives B's overlapping version: OURS stays live, THEIRS is kept
        let theirs = std::fs::read_to_string(gb.0.join("n.md")).unwrap();
        assert!(!a
            .apply_update(&sb.to_update(Some(theirs.clone())), "b")
            .unwrap());
        let ours = std::fs::read_to_string(ga.0.join("n.md")).unwrap();
        assert!(ours.contains("B1"), "local version must stay live");
        let cs = a.conflicts();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].note_uuid, s.uuid);
        assert_eq!(cs[0].peer_name, "b");
        assert!(a.conflict_content(&cs[0].id).unwrap().contains("B2"));

        // re-syncing the same version does not stack duplicates
        assert!(!a
            .apply_update(&sb.to_update(Some(theirs.clone())), "b")
            .unwrap());
        assert_eq!(a.conflicts().len(), 1);

        // "keep ours": the note is unchanged, and the rejected version does
        // not come back as a new conflict on the next sync
        a.resolve_conflict_keep_ours(&cs[0].id).unwrap();
        assert!(a.conflicts().is_empty());
        assert!(!a
            .apply_update(&sb.to_update(Some(theirs)), "b")
            .unwrap());
        assert!(
            a.conflicts().is_empty(),
            "a rejected version must not come back"
        );
        assert!(std::fs::read_to_string(ga.0.join("n.md"))
            .unwrap()
            .contains("B1"));
    }

    #[test]
    fn resolve_keep_theirs_adopts_peer_version() {
        let (mut a, ga) = mk_vault();
        let s = a.write("n.md", "a\nb\nc").unwrap();
        let (mut b, gb) = mk_vault();
        share(&mut a, &mut b, "n.md");

        std::thread::sleep(std::time::Duration::from_millis(20));
        a.write("n.md", "a\nB1\nc").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let sb = b.write("n.md", "a\nB2\nc").unwrap();
        let theirs = std::fs::read_to_string(gb.0.join("n.md")).unwrap();
        assert!(!a.apply_update(&sb.to_update(Some(theirs)), "b").unwrap());
        let cs = a.conflicts();
        assert_eq!(cs.len(), 1);

        // "keep theirs": the peer's version becomes the note, under a fresh
        // HLC that beats the peer's (so it propagates on the next sync)
        a.resolve_conflict_keep_theirs(&cs[0].id).unwrap();
        assert!(std::fs::read_to_string(ga.0.join("n.md"))
            .unwrap()
            .contains("B2"));
        assert!(a.conflicts().is_empty());
        assert!(a.get(&s.uuid).unwrap().hlc > sb.hlc);
    }

    #[test]
    fn identical_remote_edit_is_not_a_conflict() {
        let (mut a, ga) = mk_vault();
        let _s = a.write("n.md", "a\nb\nc").unwrap();
        let (mut b, _gb) = mk_vault();
        share(&mut a, &mut b, "n.md");

        // both sides make the EXACT same edit -> identical hunks, no conflict
        std::thread::sleep(std::time::Duration::from_millis(20));
        a.write("n.md", "a\nB\nc").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let sb = b.write("n.md", "a\nB\nc").unwrap();
        let theirs = std::fs::read_to_string(_gb.0.join("n.md")).unwrap();
        assert!(a.apply_update(&sb.to_update(Some(theirs)), "b").unwrap());
        assert!(a.conflicts().is_empty());
        assert!(std::fs::read_to_string(ga.0.join("n.md")).unwrap().contains("a\nB\nc"));
    }
}
