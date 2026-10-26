//! LANE 3 — a regular path query, `a·b*`, two ways.
//!
//! A **regular path query** (RPQ) asks for the pairs `(x, y)` joined by a path
//! whose edge labels spell a word of a regular language — here `a·b*`: one
//! `a` edge, then any number of `b` edges. It is the variable-length part of a
//! Cypher/GQL pattern (`(x)-[:a]->()-[:b*0..]->(y)`) and the RPQ half of
//! Pang et al.'s CRPQs (VLDB'26 §2, Definitions 1–2).
//!
//! PROVIDED — [`semi_naive`]: how SQL evaluates it, as `WITH RECURSIVE`:
//!
//! ```text
//! r(s, y) = { (s, x) | s ∈ S, (s, x) ∈ A }          -- base case
//!         ∪ { (s, z) | (s, y) ∈ r, (y, z) ∈ B }      -- recursive step, UNION (set)
//! ```
//!
//! evaluated semi-naively: each round joins only the *new* pairs (the delta)
//! with `B`, then subtracts what is already known. `derived` counts every
//! tuple the recursive join produced, including the ones the `UNION` threw
//! away as duplicates — that is the work, `pairs` is the answer.
//!
//! EXERCISE — [`product_bfs`]: how an RPQ engine evaluates it. Compile `a·b*`
//! to an automaton (state 0 --a--> state 1, state 1 --b--> state 1, state 1
//! accepting) and search the **product graph** whose nodes are `(vertex,
//! state)` pairs: from `(v, q)` follow every edge `v → w` whose label has a
//! transition `q → q'`, to `(w, q')`. Every `(w, 1)` reached from `(s, 0)` is
//! an answer `(s, w)`. Each product node is visited at most once per source,
//! so the work is bounded by the product graph's size, however many paths
//! there are. Pang et al. call this the automaton-based paradigm (§6); DuckPGQ's
//! `iterativelength` is the same search for the one-state language `b*`,
//! batched 512 sources at a time.
//!
//! The second test pins down the finding this lane exists for: with set
//! semantics and deltas, the recursive join already *is* this BFS — it
//! derives exactly as many tuples as the BFS scans adjacency entries. What
//! the automaton buys is the cost of each step (an array slot instead of a
//! hash-set insert), not fewer steps. The blow-up the relational side really
//! risks is bag semantics: [`walks_up_to`] counts the rows a `UNION ALL`
//! recursion — or a path-enumerating variable-length expansion — would emit.
//!
//! The tests are the specification.

use crate::flat::FxBuild;
use crate::gen::{label_is_a, Edges};
use crate::graph::Graph;
use std::collections::{HashMap, HashSet};

/// The edge relation split by label: `a` edges and `b` edges.
pub struct Labeled {
    pub n: u32,
    pub a: Edges,
    pub b: Edges,
    pub ga: Graph,
    pub gb: Graph,
}

impl Labeled {
    pub fn new(n: u32, edges: &Edges, seed: u64) -> Labeled {
        let (a, b): (Edges, Edges) = edges.iter().partition(|&&(s, d)| label_is_a(s, d, seed));
        let (ga, gb) = (Graph::new(n, &a), Graph::new(n, &b));
        Labeled { n, a, b, ga, gb }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SemiNaive {
    pub pairs: u64,
    pub rounds: u32,
    /// New pairs found in each round: round 1 is the base case (`a`, j = 0),
    /// round r + 1 adds the pairs first reached by `a·b^r`.
    pub new_per_round: Vec<u64>,
    /// Tuples produced by the recursive join, duplicates included.
    pub derived: u64,
}

/// PROVIDED. `WITH RECURSIVE` semantics, semi-naive, with hash joins.
/// The delta is a `Vec` in a deterministic order; the hash set is only probed.
pub fn semi_naive(g: &Labeled, sources: &[u32]) -> SemiNaive {
    let mut b_by_src: HashMap<u32, Vec<u32>, FxBuild> = HashMap::default();
    for &(s, d) in &g.b {
        b_by_src.entry(s).or_default().push(d);
    }
    let mut known: HashSet<(u32, u32), FxBuild> = HashSet::default();
    let mut delta: Vec<(u32, u32)> = Vec::new();
    for &s in sources {
        for &x in g.ga.out.neighbours(s) {
            if known.insert((s, x)) {
                delta.push((s, x));
            }
        }
    }
    let mut st = SemiNaive { pairs: 0, rounds: 1, derived: delta.len() as u64, new_per_round: vec![delta.len() as u64] };
    while !delta.is_empty() {
        let mut next = Vec::new();
        for &(s, y) in &delta {
            if let Some(zs) = b_by_src.get(&y) {
                st.derived += zs.len() as u64;
                for &z in zs {
                    if known.insert((s, z)) {
                        next.push((s, z));
                    }
                }
            }
        }
        if !next.is_empty() {
            st.rounds += 1;
            st.new_per_round.push(next.len() as u64);
        }
        delta = next;
    }
    st.pairs = known.len() as u64;
    st
}

/// PROVIDED. How many *walks* spell `a·b^j` for `j ≤ k`, summed over the
/// sources — what a bag-semantics, path-enumerating evaluation (a Cypher
/// variable-length expansion without a path mode) would emit, against the set
/// of pairs an RPQ returns. Dynamic programming over walk counts, so it never
/// enumerates them.
pub fn walks_up_to(g: &Labeled, sources: &[u32], k: usize) -> u64 {
    let n = g.n as usize;
    let mut total = 0u64;
    let mut cur = vec![0u64; n];
    for &s in sources {
        for &x in g.ga.out.neighbours(s) {
            cur[x as usize] += 1;
        }
    }
    total += cur.iter().sum::<u64>();
    for _ in 0..k {
        let mut nxt = vec![0u64; n];
        for v in 0..n {
            if cur[v] > 0 {
                for &w in g.gb.out.neighbours(v as u32) {
                    nxt[w as usize] = nxt[w as usize].saturating_add(cur[v]);
                }
            }
        }
        total = total.saturating_add(nxt.iter().fold(0u64, |acc, &x| acc.saturating_add(x)));
        cur = nxt;
    }
    total
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProductBfs {
    /// Distinct `(source, vertex)` answer pairs.
    pub pairs: u64,
    /// Product-graph nodes `(vertex, state)` visited, summed over sources.
    pub states: u64,
    /// Adjacency entries scanned, summed over sources.
    pub edges_scanned: u64,
}

/// EXERCISE. Evaluate `a·b*` from every source by BFS over the product of the
/// graph and the two-state automaton. Visit each `(vertex, state)` at most once
/// per source.
pub fn product_bfs(g: &Labeled, sources: &[u32]) -> ProductBfs {
    let _ = (g, sources);
    todo!("lane 3: BFS over (vertex, automaton state)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{power_law, PowerLaw};

    fn small() -> (Labeled, Vec<u32>) {
        let n = 2000;
        let e = power_law(PowerLaw { vertices: n, edges: 10_000, pref_percent: 70, seed: 5 });
        let g = Labeled::new(n, &e, 9);
        let sources: Vec<u32> = (0..n).step_by(97).collect();
        (g, sources)
    }

    #[test]
    fn provided_semi_naive_on_a_hand_graph() {
        // 0 -a-> 1 -b-> 2 -b-> 3, 2 -b-> 1 (a cycle), 0 -b-> 4 (wrong first label)
        let g = Labeled { n: 5, a: vec![(0, 1)], b: vec![(0, 4), (1, 2), (2, 1), (2, 3)], ga: Graph::new(5, &vec![(0, 1)]), gb: Graph::new(5, &vec![(0, 4), (1, 2), (2, 1), (2, 3)]) };
        let r = semi_naive(&g, &[0]);
        assert_eq!(r.pairs, 3, "(0,1) (0,2) (0,3)");
        assert_eq!(walks_up_to(&g, &[0], 4), 1 + 1 + 2 + 1 + 2, "walks keep going around the 1-2 cycle");
    }

    #[test]
    fn product_bfs_finds_exactly_the_recursive_answer() {
        let (g, s) = small();
        assert_eq!(product_bfs(&g, &s).pairs, semi_naive(&g, &s).pairs);
    }

    #[test]
    fn product_bfs_visits_each_product_node_once_and_does_the_semi_naive_work() {
        let (g, s) = small();
        let r = product_bfs(&g, &s);
        let sn = semi_naive(&g, &s);
        // state 0 holds only the source; state 1 holds exactly the answers.
        assert_eq!(r.states, s.len() as u64 + r.pairs);
        // Each visited (vertex, state) scans its adjacency once: the source's
        // a-edges, then each answer's b-edges. That is, tuple for tuple, what
        // the semi-naive recursive join derives — set semantics plus deltas
        // already make the relational plan a BFS. The difference is the cost
        // of each step, not the number of steps.
        assert_eq!(r.edges_scanned, sn.derived);
    }
}
