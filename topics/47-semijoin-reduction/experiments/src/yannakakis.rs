//! Yannakakis' semi-join reduction (PROVIDED).
//!
//! Two passes over a join tree. The forward (bottom-up) pass replaces each
//! relation, children first, by its semi-join with each child; the backward
//! (top-down) pass replaces each child by its semi-join with its parent.
//! After both, every remaining row of every relation appears in at least one
//! output tuple — the relations are *fully reduced* — so any left-deep plan
//! that follows the tree's edges emits at most OUT tuples per join.

use crate::hash::FastSet;
use crate::rel::Relation;

/// Hash-set work done by semi-joins: one insert per row of the filtering
/// side, one probe per row of the filtered side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemiJoinCost {
    pub inserts: usize,
    pub probes: usize,
}

impl SemiJoinCost {
    pub fn total(&self) -> usize {
        self.inserts + self.probes
    }
}

/// `r ⋉ s`: the rows of `r` whose shared-variable value occurs in `s`.
pub fn semijoin(r: &Relation, s: &Relation, cost: &mut SemiJoinCost) -> Relation {
    let v = r.shared_var(s).expect("semi-join of relations that share no variable");
    let keys: FastSet<u32> = s.col(v).iter().copied().collect();
    cost.inserts += s.len();
    cost.probes += r.len();
    let col = r.col(v);
    r.filter(|row| keys.contains(&col[row]))
}

/// A rooted join tree over the query's relations.
#[derive(Clone, Debug)]
pub struct JoinTree {
    pub root: usize,
    pub parent: Vec<Option<usize>>,
    /// Relations in post-order (children before parents; root last).
    pub post_order: Vec<usize>,
}

impl JoinTree {
    /// Root the tree at the largest relation (ties: lowest index) — RPT's
    /// "LargestRoot" rule — and connect relations that share a variable, by
    /// breadth-first search in index order. For a chain query, which is all
    /// this crate builds, that *is* the join tree; a general acyclic query
    /// needs a tree with the running-intersection property (README Step 2).
    pub fn largest_root(rels: &[Relation]) -> JoinTree {
        let n = rels.len();
        let root = (0..n).max_by_key(|&i| (rels[i].len(), std::cmp::Reverse(i))).unwrap();
        Self::rooted_at(rels, root)
    }

    pub fn rooted_at(rels: &[Relation], root: usize) -> JoinTree {
        let n = rels.len();
        let mut parent = vec![None; n];
        let mut seen = vec![false; n];
        let mut bfs = vec![root];
        seen[root] = true;
        let mut i = 0;
        while i < bfs.len() {
            let u = bfs[i];
            i += 1;
            for w in 0..n {
                if !seen[w] && rels[u].shared_var(&rels[w]).is_some() {
                    seen[w] = true;
                    parent[w] = Some(u);
                    bfs.push(w);
                }
            }
        }
        assert!(seen.iter().all(|&s| s), "query is not connected");
        // reverse BFS order puts every child before its parent
        let post_order: Vec<usize> = bfs.iter().rev().copied().collect();
        JoinTree { root, parent, post_order }
    }

    pub fn children(&self, u: usize) -> Vec<usize> {
        (0..self.parent.len()).filter(|&w| self.parent[w] == Some(u)).collect()
    }
}

#[derive(Clone, Debug)]
pub struct Reduction {
    /// The fully reduced relations, same positions as the input.
    pub rels: Vec<Relation>,
    /// Sizes after the forward pass only — what a single bottom-up sweep
    /// (SQL Server's bitmap cascade, lane 3) leaves behind.
    pub after_forward: Vec<usize>,
    pub forward: SemiJoinCost,
    pub backward: SemiJoinCost,
}

/// The full two-pass reduction.
pub fn full_reduce(rels: &[Relation], tree: &JoinTree) -> Reduction {
    let mut cur: Vec<Relation> = rels.to_vec();
    let mut forward = SemiJoinCost::default();
    for &u in &tree.post_order {
        for c in tree.children(u) {
            cur[u] = semijoin(&cur[u], &cur[c], &mut forward);
        }
    }
    let after_forward = cur.iter().map(|r| r.len()).collect();

    let mut backward = SemiJoinCost::default();
    for &u in tree.post_order.iter().rev() {
        if let Some(p) = tree.parent[u] {
            cur[u] = semijoin(&cur[u], &cur[p], &mut backward);
        }
    }
    Reduction { rels: cur, after_forward, forward, backward }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{social_graph, GraphParams, BAD, GOOD};
    use crate::hash::FastSet;
    use crate::join::left_deep;

    #[test]
    fn reduced_relations_keep_exactly_the_participating_rows() {
        // Brute force on the small graph: a base row participates iff the
        // full join, run with row ids, uses it. Compare with the reduction.
        let q = social_graph(GraphParams::SMALL);
        let tree = JoinTree::largest_root(&q.rels);
        let red = full_reduce(&q.rels, &tree);

        // participation via the good plan, tracking which ids each output row used
        let tag = &q.rels[3];
        let has_tag = &q.rels[2];
        let likes = &q.rels[1];
        let knows = &q.rels[0];
        let rare: FastSet<u32> = tag.col(3).iter().copied().collect();
        let mut likes_by_m: crate::hash::FastMap<u32, Vec<usize>> = Default::default();
        for l in 0..likes.len() {
            likes_by_m.entry(likes.cols[1][l]).or_default().push(l);
        }
        let mut knows_by_b: crate::hash::FastMap<u32, Vec<usize>> = Default::default();
        for k in 0..knows.len() {
            knows_by_b.entry(knows.cols[1][k]).or_default().push(k);
        }
        // used[0..3] hold base row ids; used[3] holds tag *values*
        let mut used = vec![FastSet::<u32>::default(); 4];
        // enumerate every output tuple, walking the chain from the rare tags
        for h in 0..has_tag.len() {
            if !rare.contains(&has_tag.cols[1][h]) {
                continue;
            }
            for &l in likes_by_m.get(&has_tag.cols[0][h]).map_or(&[][..], |v| v) {
                for &k in knows_by_b.get(&likes.cols[0][l]).map_or(&[][..], |v| v) {
                    used[0].insert(knows.ids[k]);
                    used[1].insert(likes.ids[l]);
                    used[2].insert(has_tag.ids[h]);
                    used[3].insert(has_tag.cols[1][h]);
                }
            }
        }
        for (i, want) in used.iter().enumerate().take(3) {
            let got: FastSet<u32> = red.rels[i].ids.iter().copied().collect();
            assert_eq!(&got, want, "relation {}", q.rels[i].name);
        }
        let got_tags: FastSet<u32> = red.rels[3].col(3).iter().copied().collect();
        assert_eq!(got_tags, used[3]);
    }

    #[test]
    fn after_reduction_no_join_exceeds_the_output() {
        let q = social_graph(GraphParams::SMALL);
        let red = full_reduce(&q.rels, &JoinTree::largest_root(&q.rels));
        for order in [GOOD, BAD] {
            let plain = left_deep(&q.rels, q.nvars(), &order);
            let reduced = left_deep(&red.rels, q.nvars(), &order);
            assert_eq!(plain.out, reduced.out);
            assert_eq!(plain.checksum, reduced.checksum);
            assert!(reduced.join_sizes.iter().all(|&s| s <= reduced.out), "{:?}", reduced.join_sizes);
        }
    }

    #[test]
    fn largest_root_is_knows_and_post_order_ends_there() {
        let q = social_graph(GraphParams::SMALL);
        let t = JoinTree::largest_root(&q.rels);
        assert_eq!(q.rels[t.root].name, "knows");
        assert_eq!(*t.post_order.last().unwrap(), t.root);
        assert_eq!(t.post_order, vec![3, 2, 1, 0]);
    }
}
