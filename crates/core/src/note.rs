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

/// A sync update: a summary, optionally with full (or encrypted) content.
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
