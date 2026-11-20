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
        };
        let u = s.to_update(Some("body".into()));
        let json = serde_json::to_string(&u).unwrap();
        let back: NoteUpdate = serde_json::from_str(&json).unwrap();
        assert_eq!(back.content.as_deref(), Some("body"));
    }
}
