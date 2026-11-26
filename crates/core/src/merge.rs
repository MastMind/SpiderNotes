use crate::index::Index;
use crate::note::NoteSummary;

/// Pick the winner of an LWW conflict. Returns true if `a` is strictly newer
/// than `b` (i.e. `a` should win).
pub fn a_beats_b(a: &NoteSummary, b: &NoteSummary) -> bool {
    (a.hlc.ms, a.hlc.counter, a.hlc.device.as_str())
        > (b.hlc.ms, b.hlc.counter, b.hlc.device.as_str())
}

/// Decide, for a remote summary, whether it should replace the local one.
/// A tombstone beats a live note with the same timestamp only via normal LWW.
pub fn remote_should_apply(local: Option<&NoteSummary>, remote: &NoteSummary) -> bool {
    match local {
        None => true,
        Some(l) => a_beats_b(remote, l),
    }
}

/// Merge remote updates into the local index.
///
/// Returns the updates that were actually applied (winners), in arrival order —
/// the caller uses these to (1) write/delete files and (2) re-broadcast to
/// other peers.
pub fn merge_into_index(index: &mut Index, remote: Vec<NoteSummary>) -> Vec<NoteSummary> {
    let mut applied = Vec::new();
    for r in remote {
        if remote_should_apply(index.notes.get(&r.uuid), &r) {
            index.notes.insert(r.uuid, r.clone());
            applied.push(r);
        }
    }
    applied
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hlc::Hlc;
    use uuid::Uuid;

    fn sum(uuid: Uuid, ms: u64, device: &str, deleted: bool) -> NoteSummary {
        NoteSummary {
            uuid,
            path: format!("{uuid}.md"),
            hlc: Hlc::from_parts(ms, 0, device.into()),
            hash: "h".into(),
            deleted,
            title: String::new(),
            preview: String::new(),
        }
    }

    #[test]
    fn lww_ordering() {
        let a = sum(Uuid::new_v4(), 100, "a", false);
        let b = sum(a.uuid, 101, "b", false);
        assert!(a_beats_b(&b, &a));
        assert!(!a_beats_b(&a, &b));
        // same ms: counter, then device id
        let c = sum(a.uuid, 100, "z", false);
        let d = sum(a.uuid, 100, "a", false);
        assert!(a_beats_b(&c, &d));
    }

    #[test]
    fn merge_applies_only_newer() {
        let u = Uuid::new_v4();
        let mut idx = Index {
            version: 1,
            device: Some(crate::index::DeviceMeta {
                id: "me".into(),
                name: "n".into(),
            }),
            notes: Default::default(),
            device_clock: None,
        };
        idx.notes.insert(u, sum(u, 100, "me", false));

        // older remote -> ignored
        let older = vec![sum(u, 99, "peer", false)];
        assert!(merge_into_index(&mut idx, older).is_empty());
        assert_eq!(idx.notes[&u].hlc.device, "me");

        // newer remote -> applied
        let newer = vec![sum(u, 101, "peer", true)];
        let applied = merge_into_index(&mut idx, newer);
        assert_eq!(applied.len(), 1);
        assert!(idx.notes[&u].deleted);
    }
}
