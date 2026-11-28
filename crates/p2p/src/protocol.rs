//! Wire protocol messages + delta computation.
//!
//! All messages are JSON, sent as frames over the Noise-encrypted stream.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use spidernotes_core::{Hlc, NoteSummary, NoteUpdate, Uuid};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", content = "c")]
pub enum Msg {
    Hello {
        device: String,
        name: String,
        hlc: Hlc,
        /// Capability: this peer understands `NoteUpdate.assets` (media
        /// files travel attached to the note). `default`: an older peer
        /// simply has no field, so a new peer never sends blobs to it.
        #[serde(default)]
        assets: bool,
    },
    HelloAck {
        device: String,
        name: String,
        hlc: Hlc,
        /// Same capability, advertised back to the initiator.
        #[serde(default)]
        assets: bool,
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

/// Notes I have that the peer should pull: unknown to the peer, newer than
/// the peer's copy, or in `forced` — notes where BOTH sides diverged from
/// the last shared version. The forced ones may be OLDER than the peer's
/// copy: they must still be sent, so the peer can record the conflict (with
/// the version's content) instead of silently letting its own version win.
pub fn my_delta(
    mine: &[NoteSummary],
    theirs: &[NoteSummary],
    forced: &[Uuid],
    asset_delta: bool,
) -> Vec<NoteSummary> {
    mine.iter()
        .filter(|m| {
            match theirs.iter().find(|t| t.uuid == m.uuid) {
                None => true,
                Some(t) => {
                    m.hlc > t.hlc
                        || forced.iter().any(|f| *f == m.uuid)
                        || (asset_delta && !m.deleted && m.assets != t.assets)
                }
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
            title: String::new(),
            preview: String::new(),
            encrypted: false,
            assets: Vec::new(),
        }
    }

    #[test]
    fn msg_roundtrip() {
        let m = Msg::Hello {
            device: "d".into(),
            name: "n".into(),
            hlc: Hlc::from_parts(1, 0, "d".into()),
            assets: true,
        };
        let back = Msg::decode(&m.encode()).unwrap();
        assert!(matches!(back, Msg::Hello { .. }));
        // an older peer's Hello has no `assets` field at all: it decodes as false
        let old = r#"{"t":"Hello","c":{"device":"d","name":"n","hlc":{"ms":1,"counter":0,"device":"d"}}}"#;
        let back: Msg = serde_json::from_str(old).unwrap();
        match back {
            Msg::Hello { assets, .. } => assert!(!assets),
            _ => panic!("expected Hello"),
        }
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
                title: String::new(),
                preview: String::new(),
                encrypted: false,
                assets: Vec::new(),
            },
            NoteSummary {
                uuid: u2,
                path: "b.md".into(),
                hlc: Hlc::from_parts(50, 0, "me".into()),
                hash: "h".into(),
                deleted: false,
                title: String::new(),
                preview: String::new(),
                encrypted: false,
                assets: Vec::new(),
            },
        ];
        let theirs = vec![NoteSummary {
            uuid: u1,
            path: "a.md".into(),
            hlc: Hlc::from_parts(99, 0, "peer".into()),
            hash: "h".into(),
            deleted: false,
            title: String::new(),
            preview: String::new(),
            encrypted: false,
            assets: Vec::new(),
        }];
        let d = my_delta(&mine, &theirs, &[], false);
        let uuids: Vec<Uuid> = d.iter().map(|s| s.uuid).collect();
        assert_eq!(uuids, vec![u1, u2]);
    }

    #[test]
    fn delta_empty_when_in_sync() {
        let s1 = sum(100, "peer", false);
        let mine = vec![s1.clone()];
        let theirs = vec![s1];
        assert!(my_delta(&mine, &theirs, &[], false).is_empty());
    }

    #[test]
    fn delta_includes_asset_difference_only() {
        // same HLC on both sides, but the peer lacks a media file the note
        // references (it downloaded the note with media disabled): the note
        // must be re-sent (with its AssetBlobs attached by the caller).
        let u = Uuid::new_v4();
        let mine = vec![NoteSummary {
            uuid: u,
            path: "a.md".into(),
            hlc: Hlc::from_parts(50, 0, "me".into()),
            hash: "h".into(),
            deleted: false,
            title: String::new(),
            preview: String::new(),
            encrypted: false,
            assets: vec![spidernotes_core::note::AssetRef {
                path: ".assets/a.png".into(),
                hash: "abc".into(),
            }],
        }];
        let theirs = vec![NoteSummary {
            uuid: u,
            path: "a.md".into(),
            hlc: Hlc::from_parts(50, 0, "me".into()),
            hash: "h".into(),
            deleted: false,
            title: String::new(),
            preview: String::new(),
            encrypted: false,
            assets: Vec::new(),
        }];
        // without the asset-delta flag (peer cannot receive media) no re-send
        assert!(my_delta(&mine, &theirs, &[], false).is_empty());
        let d = my_delta(&mine, &theirs, &[], true);
        assert_eq!(d.iter().map(|s| s.uuid).collect::<Vec<_>>(), vec![u]);
    }

    #[test]
    fn delta_forces_diverged_older_note() {
        // the peer's copy is NEWER, but both sides diverged from the shared
        // base: the older note must still be sent (conflict detection)
        let u = Uuid::new_v4();
        let mine = vec![NoteSummary {
            uuid: u,
            path: "a.md".into(),
            hlc: Hlc::from_parts(50, 0, "me".into()),
            hash: "mine".into(),
            deleted: false,
            title: String::new(),
            preview: String::new(),
            encrypted: false,
            assets: Vec::new(),
        }];
        let theirs = vec![NoteSummary {
            uuid: u,
            path: "a.md".into(),
            hlc: Hlc::from_parts(60, 0, "peer".into()),
            hash: "theirs".into(),
            deleted: false,
            title: String::new(),
            preview: String::new(),
            encrypted: false,
            assets: Vec::new(),
        }];
        assert!(my_delta(&mine, &theirs, &[], false).is_empty());
        let d = my_delta(&mine, &theirs, &[u], false);
        assert_eq!(d.iter().map(|s| s.uuid).collect::<Vec<_>>(), vec![u]);
    }
}
