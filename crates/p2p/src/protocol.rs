//! Wire protocol messages + delta computation.
//!
//! All messages are JSON, sent as frames over the Noise-encrypted stream.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use spidernotes_core::{Hlc, NoteSummary, NoteUpdate};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
pub enum Msg {
    Hello {
        device: String,
        name: String,
        hlc: Hlc,
    },
    HelloAck {
        device: String,
        name: String,
        hlc: Hlc,
    },
    Summaries {
        summaries: Vec<NoteSummary>,
    },
    Note {
        update: NoteUpdate,
    },
    Done,
    Error {
        message: String,
    },
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialize Msg")
    }

    pub fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        serde_json::from_slice(bytes).context("deserialize Msg")
    }
}

/// Notes I have that the peer should pull: unknown to peer, or newer than peer's copy.
pub fn my_delta(mine: &[NoteSummary], theirs: &[NoteSummary]) -> Vec<NoteSummary> {
    mine.iter()
        .filter(|m| {
            match theirs.iter().find(|t| t.uuid == m.uuid) {
                None => true,
                Some(t) => m.hlc > t.hlc,
            }
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use spidernotes_core::Hlc;
    use uuid::Uuid;

    fn sum(ms: u64, dev: &str, deleted: bool) -> NoteSummary {
        NoteSummary {
            uuid: Uuid::new_v4(),
            path: "x.md".into(),
            hlc: Hlc::from_parts(ms, 0, dev.into()),
            hash: "h".into(),
            deleted,
        }
    }

    #[test]
    fn msg_roundtrip() {
        let m = Msg::Hello {
            device: "d".into(),
            name: "n".into(),
            hlc: Hlc::from_parts(1, 0, "d".into()),
        };
        let back = Msg::decode(&m.encode()).unwrap();
        assert!(matches!(back, Msg::Hello { .. }));
    }

    #[test]
    fn delta_includes_newer_and_unknown() {
        let u1 = Uuid::new_v4();
        let u2 = Uuid::new_v4();
        let mine = vec![
            NoteSummary {
                uuid: u1,
                path: "a.md".into(),
                hlc: Hlc::from_parts(100, 0, "me".into()),
                hash: "h".into(),
                deleted: false,
            },
            NoteSummary {
                uuid: u2,
                path: "b.md".into(),
                hlc: Hlc::from_parts(50, 0, "me".into()),
                hash: "h".into(),
                deleted: false,
            },
        ];
        let theirs = vec![NoteSummary {
            uuid: u1,
            path: "a.md".into(),
            hlc: Hlc::from_parts(99, 0, "peer".into()),
            hash: "h".into(),
            deleted: false,
        }];
        let d = my_delta(&mine, &theirs);
        let uuids: Vec<Uuid> = d.iter().map(|s| s.uuid).collect();
        assert_eq!(uuids, vec![u1, u2]);
    }

    #[test]
    fn delta_empty_when_in_sync() {
        let s1 = sum(100, "peer", false);
        let mine = vec![s1.clone()];
        let theirs = vec![s1];
        assert!(my_delta(&mine, &theirs).is_empty());
    }
}
