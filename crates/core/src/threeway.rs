//! Line-based three-way merge (diff3), git-style.
//!
//! Given a `base` (the last content both sides had in common), `ours` and
//! `theirs`, changes that touch disjoint regions are combined automatically;
//! overlapping changes are reported as a conflict (the caller keeps both
//! versions — never silently drops one).

/// The result of a three-way merge: either combined content or a conflict.
#[derive(Debug, PartialEq)]
pub enum Merge {
    /// Clean merge. `content` is the combined text (byte-identical to one of
    /// the inputs when only one side changed).
    Ok(String),
    /// Overlapping changes — the caller must keep both versions.
    Conflict,
}

/// Guard: the LCS DP is O(n*m) cells. Above this product we cannot afford
/// it (huge notes), so we report a conflict instead — safe, never lossy.
const MAX_CELLS: usize = 4_000_000;

/// Three-way merge of `ours` and `theirs` against the common `base`.
pub fn merge(base: &str, ours: &str, theirs: &str) -> Merge {
    // Fast paths: one side (or both) unchanged.
    if ours == base {
        return Merge::Ok(theirs.to_string());
    }
    if theirs == base {
        return Merge::Ok(ours.to_string());
    }
    let b = split(base);
    let o = split(ours);
    let t = split(theirs);
    let cells = b.len() as u128 * o.len() as u128 + b.len() as u128 * t.len() as u128;
    if cells > MAX_CELLS as u128 {
        return Merge::Conflict;
    }
    let ho = diff(&b, &o);
    let ht = diff(&b, &t);
    if hunks_overlap(&ho, &ht) {
        return Merge::Conflict;
    }
    // Apply both hunk sets to the base. Identical hunks (both sides made
    // the very same change) sort adjacent and collapse via dedup; for the
    // rest, ours sorts first on equal positions (deterministic order for
    // same-position insertions).
    let mut combined: Vec<&Hunk> = ho.iter().chain(ht.iter()).collect();
    // identical hunks share the (del_from, del_to) key, so they sort
    // adjacent and collapse via dedup
    combined.sort_by_key(|h| (h.del_from, h.del_to));
    combined.dedup();
    let mut merged: Vec<String> = Vec::with_capacity(b.len() + combined.len());
    let mut bi = 0usize; // position in base
    for h in &combined {
        if h.del_from < bi {
            continue; // already covered by a previous hunk
        }
        merged.extend_from_slice(&b[bi..h.del_from]);
        merged.extend_from_slice(&h.insert);
        bi = h.del_to;
    }
    merged.extend_from_slice(&b[bi..]);
    Merge::Ok(merged.join("\n"))
}

fn split(s: &str) -> Vec<String> {
    s.split('\n').map(str::to_string).collect()
}

/// A change hunk in base coordinates: delete `del_from..del_to` and insert
/// `insert` in its place.
#[derive(PartialEq)]
struct Hunk {
    del_from: usize,
    del_to: usize,
    insert: Vec<String>,
}

/// Diff `a -> b` as a list of base-side hunks (LCS-based).
fn diff(a: &[String], b: &[String]) -> Vec<Hunk> {
    let n = a.len();
    let m = b.len();
    // dp[i][j] = LCS length of a[i..] and b[j..]
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut i = 0;
    let mut j = 0;
    let mut h: Option<(usize, usize, Vec<String>)> = None; // (a_from, a_to, insert)
    let flush = |hunks: &mut Vec<Hunk>, h: &mut Option<(usize, usize, Vec<String>)>| {
        if let Some((f, t, ins)) = h.take() {
            hunks.push(Hunk { del_from: f, del_to: t, insert: ins });
        }
    };
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            flush(&mut hunks, &mut h);
            i += 1;
            j += 1;
        } else if j < m && (i == n || dp[i][j + 1] >= dp[i + 1][j]) {
            // insert b[j] (a pure insertion has del_from == del_to == i)
            match &mut h {
                Some((_, t, ins)) if *t == i => ins.push(b[j].clone()),
                _ => h = Some((i, i, vec![b[j].clone()])),
            }
            j += 1;
        } else {
            // delete a[i]
            match &mut h {
                Some((f, t, ins)) if *t == i => *t = i + 1,
                _ => {
                    if h.is_some() {
                        flush(&mut hunks, &mut h);
                    }
                    h = Some((i, i + 1, vec![]));
                }
            }
            i += 1;
        }
    }
    flush(&mut hunks, &mut h);
    hunks
}

/// Do any hunk pairs touch overlapping base regions? Pure insertions at the
/// same position (empty ranges) do NOT conflict — both are kept.
fn hunks_overlap(a: &[Hunk], b: &[Hunk]) -> bool {
    for x in a {
        for y in b {
            // identical hunks (same edit on both sides) are not a conflict
            if x.del_from == y.del_from && x.del_to == y.del_to && x.insert == y.insert {
                continue;
            }
            let x_empty = x.del_from == x.del_to;
            let y_empty = y.del_from == y.del_to;
            if x_empty && y_empty {
                continue;
            }
            if x_empty {
                // x inserts inside y's deleted span
                if x.del_from > y.del_from && x.del_from <= y.del_to {
                    return true;
                }
            } else if y_empty {
                if y.del_from > x.del_from && y.del_from <= x.del_to {
                    return true;
                }
            } else if x.del_from < y.del_to && y.del_from < x.del_to {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_side_wins_verbatim() {
        assert_eq!(
            merge("a\nb\nc", "a\nb\nc", "a\nB\nc"),
            Merge::Ok("a\nB\nc".into())
        );
        assert_eq!(
            merge("a\nb\nc", "a\nB\nc", "a\nb\nc"),
            Merge::Ok("a\nB\nc".into())
        );
    }

    #[test]
    fn disjoint_changes_combine() {
        let base = "l1\nl2\nl3\nl4\nl5";
        let ours = "L1\nl2\nl3\nl4\nl5";
        let theirs = "l1\nl2\nl3\nl4\nL5";
        match merge(base, ours, theirs) {
            Merge::Ok(m) => assert_eq!(m, "L1\nl2\nl3\nl4\nL5"),
            Merge::Conflict => panic!("expected clean merge"),
        }
    }

    #[test]
    fn same_edit_on_both_sides_is_not_a_conflict() {
        let base = "a\nb\nc";
        let both = "a\nB\nc";
        assert_eq!(merge(base, both, both), Merge::Ok("a\nB\nc".into()));
    }

    #[test]
    fn same_line_edited_differently_conflicts() {
        let base = "a\nb\nc";
        assert_eq!(merge(base, "a\nB\nc", "a\nX\nc"), Merge::Conflict);
    }

    #[test]
    fn delete_vs_edit_of_same_line_conflicts() {
        let base = "a\nb\nc\nd";
        assert_eq!(merge(base, "a\nc\nd", "a\nB\nc\nd"), Merge::Conflict);
    }

    #[test]
    fn insertions_at_same_position_both_kept_ours_first() {
        let base = "a\nc";
        let ours = "a\no\nc";
        let theirs = "a\nt\nc";
        match merge(base, ours, theirs) {
            Merge::Ok(m) => assert_eq!(m, "a\no\nt\nc"),
            Merge::Conflict => panic!("expected clean merge"),
        }
    }

    #[test]
    fn append_at_different_ends_combines() {
        let base = "a\nb";
        let ours = "a\nb\nours";
        let theirs = "a\nb\nt theirs";
        match merge(base, ours, theirs) {
            Merge::Ok(m) => assert_eq!(m, "a\nb\nours\nt theirs"),
            Merge::Conflict => panic!("expected clean merge"),
        }
    }

    #[test]
    fn empty_base_both_created_conflicts() {
        assert_eq!(merge("", "x", "y"), Merge::Conflict);
        assert_eq!(merge("", "x", "x"), Merge::Ok("x".into()));
    }
}
