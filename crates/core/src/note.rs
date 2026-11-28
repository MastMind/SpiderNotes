use crate::hlc::Hlc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A note as it lives in the vault (full content loaded).
#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub uuid: Uuid,
    /// Path relative to the vault root, slash-separated.
    pub path: String,
    pub content: String,
    pub hlc: Hlc,
    pub deleted: bool,
}

/// One media file a note's body references: the vault-relative path where
/// the file lives (`.assets/...`, or the legacy `assets/...`) and the sha256
/// of its bytes. A peer compares (path, hash) to decide the bytes are
/// already there — no re-transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRef {
    pub path: String,
    pub hash: String,
}

/// A media file's bytes, attached to a [`NoteUpdate`]: the receiver stores
/// the file at `path` (guarded to the vault's asset dirs) so the note's
/// references resolve on this side too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetBlob {
    pub path: String,
    /// base64 of the file bytes.
    pub b64: String,
}

/// Lightweight summary used for index + sync deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSummary {
    pub uuid: Uuid,
    pub path: String,
    pub hlc: Hlc,
    /// sha256 hex of the note body (frontmatter excluded).
    pub hash: String,
    pub deleted: bool,
    /// Note title for list rows: the first `# heading`, else the first
    /// non-empty body line, else empty (the UI falls back to the file name).
    /// Computed when the note is written and carried in the index, so listing
    /// never re-reads the files. `default`: older index files / older peers
    /// simply have no title yet.
    #[serde(default)]
    pub title: String,
    /// First non-empty line after the title (whitespace collapsed, capped at
    /// 80 chars); empty when the note has no second line.
    #[serde(default)]
    pub preview: String,
    /// True when the body on disk is ciphertext (see `crate::crypto`): the
    /// title then comes from the `spidernotes-title` frontmatter line and
    /// the preview is always empty (there is no plaintext to preview).
    #[serde(default)]
    pub encrypted: bool,
    /// Media files the body references that exist in THIS vault (path +
    /// hash). Sync compares the lists: a missing/stale entry is what makes
    /// the note re-sent with its [`AssetBlob`]s attached. Empty for encrypted
    /// notes (the body is ciphertext — the references are unknowable) and for
    /// references whose file is missing locally. `default`: older index
    /// files / older peers simply have no list yet.
    #[serde(default)]
    pub assets: Vec<AssetRef>,
}

/// Maximum length of the list-row preview (before the ellipsis).
const PREVIEW_MAX: usize = 80;

/// Extract (title, preview) for a list row from the note content.
///
/// Title: the first ATX heading (`#`..`######` + space), else the first
/// non-empty line of the body (frontmatter excluded). Preview: the first
/// non-empty line after the title line, whitespace-collapsed and capped at
/// `PREVIEW_MAX` chars. Both empty when the note has no body text.
pub fn title_preview(content: &str) -> (String, String) {
    // Encrypted notes: the body is base64 ciphertext — the title is the
    // plaintext `spidernotes-title` metadata, and there is no preview.
    if crate::frontmatter::is_encrypted(content) {
        return (
            crate::crypto::extract_title(content).unwrap_or_default(),
            String::new(),
        );
    }
    let body: Vec<&str> = match crate::frontmatter::frontmatter_bounds(content) {
        Some((_, end)) => content.lines().skip(end).collect(),
        None => content.lines().collect(),
    };
    let mut title_at: Option<(usize, usize)> = None; // (line index, # count)
    for (i, line) in body.iter().enumerate() {
        let t = line.trim_start();
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) {
            let rest = &t[hashes..];
            if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                title_at = Some((i, hashes));
                break;
            }
        }
    }
    let (i, hashes) = match title_at {
        Some(p) => p,
        None => match body.iter().position(|l| !l.trim().is_empty()) {
            Some(i) => (i, 0),
            None => return (String::new(), String::new()),
        },
    };
    let line = body[i].trim_start();
    let title = line[hashes..].trim().trim_end_matches('#').trim().to_string();
    let preview = body
        .iter()
        .skip(i + 1)
        .find(|l| !l.trim().is_empty())
        .map(|l| collapse(l))
        .unwrap_or_default();
    (title, preview)
}

/// Rewrite the note's title line to `new_title` (frontmatter untouched):
/// an existing ATX heading keeps its level and gets the new text; a note
/// without a heading gets `# new_title` inserted as the first body line.
/// The result always yields `new_title` from `title_preview`.
pub fn rewrite_title(content: &str, new_title: &str) -> String {
    let (fm_lines, body): (Vec<&str>, Vec<&str>) =
        match crate::frontmatter::frontmatter_bounds(content) {
            Some((_start, end)) => (
                content.lines().take(end).collect(),
                content.lines().skip(end).collect(),
            ),
            None => (Vec::new(), content.lines().collect()),
        };
    let mut out: Vec<String> = fm_lines.iter().map(|l| l.to_string()).collect();
    let first_non_empty = body.iter().position(|l| !l.trim().is_empty());
    let mut body_out: Vec<String> = body.iter().map(|l| l.to_string()).collect();
    match first_non_empty {
        Some(i) => {
            let t = body[i].trim_start();
            let hashes: usize = t.chars().take_while(|c| *c == '#').count();
            let is_heading = (1..=6).contains(&hashes)
                && t[hashes..]
                    .chars()
                    .next()
                    .is_some_and(|c| c == ' ' || c == '\t');
            if is_heading {
                body_out[i] = format!("{} {new_title}", "#".repeat(hashes));
            } else {
                body_out.insert(i, format!("# {new_title}"));
            }
        }
        None => body_out.push(format!("# {new_title}")),
    }
    out.extend(body_out);
    out.join("\n") + if content.ends_with('\n') { "\n" } else { "" }
}

/// Collapse internal whitespace and cap at `PREVIEW_MAX` chars.
fn collapse(line: &str) -> String {
    let s: String = line
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut out: String = s.chars().take(PREVIEW_MAX).collect();
    if s.chars().count() > PREVIEW_MAX {
        out.push('…');
    }
    out
}

/// A sync update: a summary, optionally with full (or encrypted) content
/// and the media files the receiver is missing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteUpdate {
    pub uuid: Uuid,
    pub path: String,
    pub hlc: Hlc,
    pub hash: Option<String>,
    pub deleted: bool,
    /// Plaintext markdown body (P2P channel). `None` for deleted notes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Media files to store alongside the note (`None` = nothing to send).
    /// `default`: older peers do not understand the field and ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assets: Option<Vec<AssetBlob>>,
}

impl NoteSummary {
    pub fn to_update(&self, content: Option<String>) -> NoteUpdate {
        NoteUpdate {
            uuid: self.uuid,
            path: self.path.clone(),
            hlc: self.hlc.clone(),
            hash: Some(self.hash.clone()),
            deleted: self.deleted,
            content,
            assets: None,
        }
    }
}

/// sha256 hex of the given bytes.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Hash of the note body, i.e. content without the frontmatter block
/// (and without the blank separator line right after it).
pub fn body_hash(content: &str) -> String {
    let body = match crate::frontmatter::frontmatter_bounds(content) {
        Some((_, end)) => {
            let rest: Vec<&str> = content.lines().skip(end).collect();
            let mut start = 0;
            while start < rest.len() && rest[start].trim().is_empty() {
                start += 1;
            }
            rest[start..].join("\n")
        }
        None => content.to_string(),
    };
    sha256_hex(body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_hash_ignores_frontmatter() {
        let a = "# T\nhello";
        let b = crate::frontmatter::ensure_id(a, "x");
        assert_eq!(body_hash(a), body_hash(&b));
        assert_ne!(body_hash(a), body_hash("# T\nbye"));
    }

    #[test]
    fn update_roundtrip() {
        let hlc = Hlc::from_parts(1, 0, "d".into());
        let s = NoteSummary {
            uuid: Uuid::new_v4(),
            path: "a/b.md".into(),
            hlc,
            hash: "h".into(),
            deleted: false,
            title: "t".into(),
            preview: "p".into(),
            encrypted: false,
            assets: Vec::new(),
        };
        let u = s.to_update(Some("body".into()));
        let json = serde_json::to_string(&u).unwrap();
        let back: NoteUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content.as_deref(), Some("body"));
    }

    #[test]
    fn title_preview_from_heading() {
        let c = "---\nspidernotes-id: x\n---\n\n# Title here\n\nsome   body\ntail\n";
        assert_eq!(title_preview(c), ("Title here".into(), "some body".into()));
    }

    #[test]
    fn title_preview_without_heading() {
        let c = "first line\n\nsecond line\n";
        assert_eq!(title_preview(c), ("first line".into(), "second line".into()));
    }

    #[test]
    fn rewrite_title_replaces_heading_level_kept() {
        let c = "---\nspidernotes-id: x\n---\n\n## Old title\nbody\n";
        let out = rewrite_title(c, "New name");
        assert_eq!(out, "---\nspidernotes-id: x\n---\n\n## New name\nbody\n");
        assert_eq!(title_preview(&out).0, "New name");
    }

    #[test]
    fn rewrite_title_inserts_when_no_heading() {
        let c = "first line\nsecond line\n";
        let out = rewrite_title(c, "Added");
        assert_eq!(title_preview(&out).0, "Added");
        assert_eq!(out, "# Added\nfirst line\nsecond line\n");
    }

    #[test]
    fn rewrite_title_keeps_frontmatter_and_trailing_newline() {
        let c = "---\nspidernotes-id: x\ntags: [a]\n---\n\n# Old\n";
        let out = rewrite_title(c, "New");
        assert!(out.starts_with("---\nspidernotes-id: x\ntags: [a]\n---\n\n# New"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn title_preview_empty_and_single_line() {
        assert_eq!(title_preview(""), (String::new(), String::new()));
        assert_eq!(
            title_preview("   \n\n  "),
            (String::new(), String::new())
        );
        assert_eq!(title_preview("# only"), ("only".into(), String::new()));
    }

    #[test]
    fn title_preview_skips_frontmatter_and_caps_preview() {
        let c = "---\ntitle: fm\n---\ntext above\n# h\n";
        // the heading wins over the earlier plain line
        assert_eq!(title_preview(c), ("h".into(), String::new()));
        let long = format!("# t\n{}", "x".repeat(200));
        let (_, p) = title_preview(&long);
        assert_eq!(p.chars().count(), PREVIEW_MAX + 1);
        assert!(p.ends_with('…'));
    }
}
