//! Graph-as-Z-set infrastructure: seeded generators, a churn stream
//! (batches of edge inserts+deletes), and the full-recompute oracles the
//! incremental stubs must match. Undirected edges stored as (min, max).

use crate::zset::ZSet;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::{HashMap, HashSet, VecDeque};

pub type Edge = (u32, u32);

pub fn gen_edges(n: u32, m: usize, seed: u64) -> ZSet<Edge> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut set = HashSet::with_capacity(m);
    while set.len() < m {
        let u = rng.gen_range(0..n);
        let v = rng.gen_range(0..n);
        if u != v {
            set.insert((u.min(v), u.max(v)));
        }
    }
    ZSet::from_updates(set.into_iter().map(|e| (e, 1)).collect())
}

/// Produces delta batches against an evolving graph: `deletes` random
/// present edges get weight -1, `inserts` fresh edges get +1. The
/// generator tracks the live edge set so weights stay in {0, 1} — the
/// oracles below assume set semantics.
pub struct ChurnGen {
    n: u32,
    present: HashSet<Edge>,
    present_vec: Vec<Edge>,
    rng: ChaCha8Rng,
}

impl ChurnGen {
    pub fn new(base: &ZSet<Edge>, n: u32, seed: u64) -> Self {
        let present: HashSet<Edge> = base.iter().map(|(e, _)| *e).collect();
        let present_vec = present.iter().copied().collect();
        ChurnGen { n, present, present_vec, rng: ChaCha8Rng::seed_from_u64(seed) }
    }

    pub fn next_batch(&mut self, inserts: usize, deletes: usize) -> ZSet<Edge> {
        let mut updates = Vec::with_capacity(inserts + deletes);
        let mut touched = HashSet::new();
        for _ in 0..deletes.min(self.present_vec.len()) {
            let i = self.rng.gen_range(0..self.present_vec.len());
            let e = self.present_vec.swap_remove(i);
            self.present.remove(&e);
            touched.insert(e);
            updates.push((e, -1));
        }
        let mut added = 0;
        while added < inserts {
            let u = self.rng.gen_range(0..self.n);
            let v = self.rng.gen_range(0..self.n);
            if u == v {
                continue;
            }
            let e = (u.min(v), u.max(v));
            if self.present.contains(&e) || touched.contains(&e) {
                continue;
            }
            self.present.insert(e);
            self.present_vec.push(e);
            updates.push((e, 1));
            added += 1;
        }
        ZSet::from_updates(updates)
    }
}

/// Sorted adjacency lists, both directions. O(m) rebuild — the cost the
/// incremental structures avoid paying per batch.
pub fn adjacency(edges: &ZSet<Edge>) -> HashMap<u32, Vec<u32>> {
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for &((u, v), w) in edges.iter() {
        debug_assert_eq!(w, 1, "oracles assume set semantics");
        adj.entry(u).or_default().push(v);
        adj.entry(v).or_default().push(u);
    }
    for l in adj.values_mut() {
        l.sort_unstable();
    }
    adj
}

/// Full-recompute triangle count: for each edge (u,v), common neighbors
/// w > v so each triangle counts once. O(m · d̄) — every batch pays it all
/// over again; that's the enemy this topic names.
pub fn count_triangles(edges: &ZSet<Edge>) -> i64 {
    let adj = adjacency(edges);
    let empty: Vec<u32> = Vec::new();
    let mut count = 0i64;
    for &((u, v), _) in edges.iter() {
        let (a, b) = (adj.get(&u).unwrap_or(&empty), adj.get(&v).unwrap_or(&empty));
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                if a[i] > v {
                    count += 1;
                }
                i += 1;
                j += 1;
            } else if a[i] < b[j] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    count
}

/// Full BFS from src — the recompute oracle for reach.rs.
pub fn bfs_reachable(edges: &ZSet<Edge>, src: u32) -> HashSet<u32> {
    let adj = adjacency(edges);
    let mut seen = HashSet::from([src]);
    let mut q = VecDeque::from([src]);
    while let Some(u) = q.pop_front() {
        if let Some(ns) = adj.get(&u) {
            for &v in ns {
                if seen.insert(v) {
                    q.push_back(v);
                }
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn churn_keeps_set_semantics() {
        let base = gen_edges(1000, 5000, 1);
        let mut gen = ChurnGen::new(&base, 1000, 2);
        let mut g = base;
        for _ in 0..20 {
            let d = gen.next_batch(50, 50);
            g = g.merge(&d);
            assert!(g.iter().all(|(_, w)| *w == 1));
        }
        assert_eq!(g.support(), 5000);
    }

    #[test]
    fn triangle_oracle_on_known_graph() {
        // K4 has 4 triangles.
        let mut e = Vec::new();
        for u in 0..4u32 {
            for v in u + 1..4 {
                e.push(((u, v), 1));
            }
        }
        assert_eq!(count_triangles(&ZSet::from_updates(e)), 4);
    }

    #[test]
    fn bfs_finds_component() {
        let g = ZSet::from_updates(vec![((0, 1), 1), ((1, 2), 1), ((5, 6), 1)]);
        let r = bfs_reachable(&g, 0);
        assert_eq!(r, HashSet::from([0, 1, 2]));
    }
}
