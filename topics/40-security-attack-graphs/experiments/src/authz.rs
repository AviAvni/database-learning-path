//! Authorization as graph reachability: Zanzibar's Check, and the price
//! of the index that makes it fast.
//!
//! Zanzibar (ATC'19 §3.2.3) states Check as a recursive definition over
//! the relation-tuple graph:
//!
//! ```text
//!   CHECK(U, object#relation) =
//!       ∃ tuple ⟨object#relation@U⟩
//!     ∨ ∃ tuple ⟨object#relation@U'⟩ where U' = ⟨object'#relation'⟩
//!                                     s.t. CHECK(U, U')
//! ```
//!
//! That is pointer chasing, and the paper is blunt about where it hurts:
//! "can be expensive when indirect ACLs or groups are deep or wide"
//! (§3.2.3). The answer is Leopard (§3.2.4), a denormalized index of two
//! flattened sets stored as ordered integer lists:
//!
//! * `GROUP2GROUP(s) → {e}` — `e` is a group that is directly *or
//!   indirectly* a sub-group of ancestor group `s`.
//! * `MEMBER2GROUP(u) → {e}` — `e` is a group `u` is a *direct* member of.
//!
//! and then membership is a set intersection:
//!
//! ```text
//!   U ∈ G  ⟺  MEMBER2GROUP(U) ∩ GROUP2GROUP(G) ≠ ∅
//! ```
//!
//! "evaluating the intersection between two sets, A and B, requires only
//! O(min(|A|,|B|)) skip-list seeks" — the galloping intersect from topic
//! 23, doing authorization. In production this index answers 1.56M QPS
//! at a 150 µs median (§4.4).
//!
//! The trade is the one from topic 1's RUM conjecture, wearing a badge:
//! the flattened closure is bigger than the tuples it denormalizes, and
//! it has to be maintained. Zanzibar pays it offline (periodic snapshots
//! + an incremental layer fed by Watch, ~500 index updates/sec median).

use rand::Rng;
use rand_chacha::ChaCha8Rng;
#[allow(unused_imports)]
use std::collections::HashSet;

/// The relation-tuple store, restricted to the group-membership subset
/// that Leopard indexes. Ids are dense so sets can be `Vec<u32>`.
pub struct RelStore {
    /// `group:g#member@user:u` — direct user members of each group.
    pub direct_users: Vec<Vec<u32>>,
    /// `group:g#member@group:c#member` — members of `c` are members of
    /// `g`. Stored parent → children.
    pub children: Vec<Vec<u32>>,
    pub n_users: usize,
}

impl RelStore {
    pub fn n_groups(&self) -> usize {
        self.children.len()
    }
    pub fn tuple_count(&self) -> usize {
        self.direct_users.iter().map(|v| v.len()).sum::<usize>()
            + self.children.iter().map(|v| v.len()).sum::<usize>()
    }
}

/// A nesting shape that reproduces the paper's pain point: one deep
/// chain of groups (`depth` levels, the top-level group is 0), and at
/// every level `width` decoy sub-groups that carry ordinary members.
///
/// The interesting user is a direct member of the *deepest* group only,
/// so a positive Check has to walk the whole chain, and the decoys are
/// what a naive traversal wastes its time on.
pub fn nested_groups(
    rng: &mut ChaCha8Rng,
    depth: usize,
    width: usize,
    members_per_group: usize,
    n_users: usize,
) -> (RelStore, u32) {
    let n_groups = depth * (1 + width);
    let mut direct_users = vec![Vec::new(); n_groups];
    let mut children = vec![Vec::new(); n_groups];

    // Chain: group i is the parent of group i+1.
    for i in 0..depth - 1 {
        children[i].push((i + 1) as u32);
    }
    // Decoys hang off each chain level.
    let mut next = depth;
    for i in 0..depth {
        for _ in 0..width {
            children[i].push(next as u32);
            for _ in 0..members_per_group {
                // Never the needle: it must be findable only by walking
                // the chain to the bottom.
                direct_users[next].push(rng.gen_range(0..n_users as u32 - 1));
            }
            next += 1;
        }
    }
    // The needle: a member of the deepest chain group and nothing else.
    let deep_user = (n_users - 1) as u32;
    direct_users[depth - 1].push(deep_user);

    for v in direct_users.iter_mut() {
        v.sort_unstable();
        v.dedup();
    }
    (
        RelStore {
            direct_users,
            children,
            n_users,
        },
        deep_user,
    )
}

/// What a Check cost: how many relation tuples it had to read. This is
/// the number Zanzibar's caching, pooling and Leopard index all exist to
/// hold down — the paper reports only 20M read RPCs/sec reaching Spanner
/// behind >10M client QPS (§4.4).
#[derive(Default, Clone, Copy, Debug)]
pub struct CheckCost {
    pub tuple_reads: usize,
    pub groups_visited: usize,
}

/// Zanzibar Check by pointer chasing (§3.2.3), with the cycle protection
/// a real relation graph needs — group nesting cycles are a
/// misconfiguration, not an impossibility, and the evaluator must
/// terminate on them.
///
/// `memo` toggles the "cache intermediate check results" behaviour of
/// §3.2.5. On a DAG where the same sub-group is reachable through many
/// parents, it is the difference between paths and nodes.
pub fn check_pointer(store: &RelStore, user: u32, group: u32, memo: bool) -> (bool, CheckCost) {
    let _ = (store, user, group, memo);
    todo!(
        "expand the userset: start at `group`, and at each group read its \n\
         direct users (count one tuple read; `direct_users` is sorted, so \n\
         binary_search) then push its children (count one more). Return \n\
         true on the first hit. With `memo`, skip groups already expanded; \n\
         without it, still bound the walk so a nesting cycle terminates."
    )
}

/// Leopard's two flattened sets. Ordered integer lists, exactly as the
/// paper describes them.
pub struct LeopardIndex {
    /// `GROUP2GROUP(g)` — every group that is a sub-group of `g`,
    /// transitively, including `g` itself so the intersection test needs
    /// no special case for direct membership.
    pub group2group: Vec<Vec<u32>>,
    /// `MEMBER2GROUP(u)` — the groups `u` is a *direct* member of.
    pub member2group: Vec<Vec<u32>>,
}

impl LeopardIndex {
    /// Build the transitive closure. This is the offline pipeline in
    /// §3.2.4 — periodic, snapshot-based, and the reason Leopard needs a
    /// separate incremental layer to stay fresh.
    pub fn build(store: &RelStore) -> LeopardIndex {
        let _ = store;
        todo!(
            "group2group[g] = the transitive closure of `children` from g, \n\
             including g itself, sorted. member2group[u] = the groups u is \n\
             a direct member of, sorted and deduped. Both must tolerate \n\
             nesting cycles."
        )
    }

    pub fn size_entries(&self) -> usize {
        self.group2group.iter().map(|v| v.len()).sum::<usize>()
            + self.member2group.iter().map(|v| v.len()).sum::<usize>()
    }

    /// `MEMBER2GROUP(U) ∩ GROUP2GROUP(G) ≠ ∅`, with the cost in probes.
    pub fn check(&self, user: u32, group: u32) -> (bool, usize) {
        let a = &self.member2group[user as usize];
        let b = &self.group2group[group as usize];
        intersect_galloping(a, b)
    }
}

/// Sorted-set intersection test by galloping (exponential) search:
/// iterate the *smaller* set and seek each of its elements in the
/// larger, doubling the stride. O(min(|A|,|B|) · log) probes — the
/// paper's O(min(|A|,|B|)) skip-list seeks, done with the primitive from
/// topic 23's roaring guide.
///
/// Returns `(non_empty, probes)`.
pub fn intersect_galloping(a: &[u32], b: &[u32]) -> (bool, usize) {
    let _ = (a, b);
    todo!(
        "iterate the SMALLER set; for each element widen a window over the \n\
         larger one by 1, 2, 4, 8, ... from where the last search stopped, \n\
         until the window's right edge is at or past the element, then \n\
         binary_search inside the window. Keep the resume position so the \n\
         whole pass is O(min(|A|,|B|) * log), not O(|A| * log|B|). Count \n\
         every comparison as a probe, and return on the first match."
    )
}

/// The straw man: linear merge. O(|A| + |B|) — fine when the sets are
/// the same size, catastrophic when a user is in 3 groups and the group
/// has 100,000 descendants.
pub fn intersect_linear(a: &[u32], b: &[u32]) -> (bool, usize) {
    let (mut i, mut j, mut steps) = (0usize, 0usize, 0usize);
    while i < a.len() && j < b.len() {
        steps += 1;
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => return (true, steps),
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    (false, steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ad_graph::seeded_rng;

    #[test]
    fn the_index_answers_exactly_what_pointer_chasing_answers() {
        // Denormalization is only allowed if it is invisible. Every
        // (user, group) pair must agree, positive and negative.
        let mut rng = seeded_rng(3);
        let (store, deep_user) = nested_groups(&mut rng, 6, 3, 4, 200);
        let index = LeopardIndex::build(&store);
        for u in 0..200u32 {
            for g in 0..store.n_groups() as u32 {
                let (want, _) = check_pointer(&store, u, g, true);
                let (got, _) = index.check(u, g);
                assert_eq!(want, got, "user {u} group {g}");
            }
        }
        assert!(index.check(deep_user, 0).0, "deep member must be found");
    }

    #[test]
    fn nesting_cycles_terminate() {
        // group:0 ⊃ group:1 ⊃ group:0. A misconfiguration, but the
        // evaluator does not get to hang on it.
        let store = RelStore {
            direct_users: vec![vec![], vec![7]],
            children: vec![vec![1], vec![0]],
            n_users: 8,
        };
        assert!(check_pointer(&store, 7, 0, true).0);
        assert!(!check_pointer(&store, 6, 0, true).0);
        let index = LeopardIndex::build(&store);
        assert!(index.check(7, 0).0);
        assert!(!index.check(6, 0).0);
    }

    #[test]
    fn galloping_beats_the_merge_when_the_sets_are_lopsided() {
        // The Leopard case: a user in a handful of groups, checked
        // against a group with a 500k-strong descendant set. What
        // matters is the answer that lives at the far end — a linear
        // merge has to walk the whole way there.
        let large: Vec<u32> = (0..1_000_000u32).filter(|x| x % 2 == 1).collect();
        for (needle, expect) in [(999_999u32, true), (999_998u32, false)] {
            let small = vec![needle];
            let (hit_g, probes) = intersect_galloping(&small, &large);
            let (hit_l, steps) = intersect_linear(&small, &large);
            assert_eq!(hit_g, expect);
            assert_eq!(hit_g, hit_l);
            assert!(
                probes * 1_000 < steps,
                "needle {needle}: galloping {probes} probes vs merge {steps} steps"
            );
        }
        // And it must still agree with the merge on ordinary inputs.
        let small: Vec<u32> = vec![2, 4, 6];
        assert_eq!(
            intersect_galloping(&small, &large).0,
            intersect_linear(&small, &large).0
        );
    }

    #[test]
    fn memoization_removes_the_path_explosion() {
        // A diamond lattice: without memoization a traversal counts
        // paths, with it counts nodes.
        let depth = 12;
        let mut children = vec![Vec::new(); 2 * depth + 1];
        for i in 0..depth {
            children[2 * i] = vec![(2 * i + 1) as u32, (2 * i + 2) as u32];
            if i + 1 < depth {
                children[2 * i + 1] = vec![(2 * i + 2) as u32];
                children[2 * i + 2] = vec![(2 * i + 2) as u32];
            }
        }
        let store = RelStore {
            direct_users: vec![Vec::new(); 2 * depth + 1],
            children,
            n_users: 4,
        };
        let (_, memo) = check_pointer(&store, 1, 0, true);
        let (_, naive) = check_pointer(&store, 1, 0, false);
        assert!(
            naive.groups_visited > 2 * memo.groups_visited,
            "memo {} vs naive {}",
            memo.groups_visited,
            naive.groups_visited
        );
    }
}
