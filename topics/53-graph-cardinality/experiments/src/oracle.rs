//! Exact counts — the oracle every estimator is scored against (PROVIDED).
//!
//! A *walk pattern* is a list of edge types `[t0, t1, …]` with
//! `TYPES[t_i].1 == TYPES[t_{i+1}].0`: the Cypher
//! `(a:L)-[:t0]->(b)-[:t1]->(c)…`. Its count is the number of homomorphisms
//! (no distinctness constraint between the pattern's nodes), which is what
//! G-CARE and every estimator here count.

use crate::gen::{Graph, TYPES};

/// All label-consistent type walks of exactly `len` edges, in lexicographic
/// order of their type lists.
pub fn walks(len: usize) -> Vec<Vec<usize>> {
    fn rec(cur: &mut Vec<usize>, len: usize, out: &mut Vec<Vec<usize>>) {
        if cur.len() == len {
            out.push(cur.clone());
            return;
        }
        for t in 0..TYPES.len() {
            if cur.last().map_or(true, |&p| TYPES[p].1 == TYPES[t].0) {
                cur.push(t);
                rec(cur, len, out);
                cur.pop();
            }
        }
    }
    let mut out = Vec::new();
    rec(&mut Vec::new(), len, &mut out);
    out
}

/// `P_j` for j = 1..=k: the number of walks of the first j edges, by a
/// frontier DP — `cnt[v]` is the number of walks that end at v.
pub fn prefix_counts(g: &Graph, types: &[usize]) -> Vec<u64> {
    let mut cnt = vec![0u64; g.n];
    for v in g.label_range(TYPES[types[0]].0) {
        cnt[v] = 1;
    }
    let mut res = Vec::new();
    for &t in types {
        let mut next = vec![0u64; g.n];
        for v in 0..g.n {
            let c = cnt[v];
            if c == 0 {
                continue;
            }
            for &w in g.out[t].nbrs(v) {
                next[w as usize] += c;
            }
        }
        res.push(next.iter().sum());
        cnt = next;
    }
    res
}

/// `S_j` for j = 1..=k: the number of walks of the last j edges (the same DP
/// run backwards over the in-adjacency).
pub fn suffix_counts(g: &Graph, types: &[usize]) -> Vec<u64> {
    let mut cnt = vec![0u64; g.n];
    for v in g.label_range(TYPES[*types.last().unwrap()].1) {
        cnt[v] = 1;
    }
    let mut res = Vec::new();
    for &t in types.iter().rev() {
        let mut next = vec![0u64; g.n];
        for v in 0..g.n {
            let c = cnt[v];
            if c == 0 {
                continue;
            }
            for &w in g.inn[t].nbrs(v) {
                next[w as usize] += c;
            }
        }
        res.push(next.iter().sum());
        cnt = next;
    }
    res
}

pub fn path_count(g: &Graph, types: &[usize]) -> u64 {
    *prefix_counts(g, types).last().unwrap()
}

/// Number of (a, b, c) with `a -t0-> b -t1-> c -t2-> a`. For each t0-edge
/// (a, b) it intersects b's t1-successors with a's t2-predecessors, both
/// sorted.
pub fn triangle_count(g: &Graph, t0: usize, t1: usize, t2: usize) -> u64 {
    assert_eq!(TYPES[t0].1, TYPES[t1].0);
    assert_eq!(TYPES[t1].1, TYPES[t2].0);
    assert_eq!(TYPES[t2].1, TYPES[t0].0);
    let mut total = 0u64;
    for a in 0..g.n {
        let preds = g.inn[t2].nbrs(a);
        if preds.is_empty() {
            continue;
        }
        for &b in g.out[t0].nbrs(a) {
            let succ = g.out[t1].nbrs(b as usize);
            let (mut i, mut j) = (0, 0);
            while i < succ.len() && j < preds.len() {
                match succ[i].cmp(&preds[j]) {
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                    std::cmp::Ordering::Equal => {
                        total += 1;
                        i += 1;
                        j += 1;
                    }
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{generate, Params, Popularity};

    /// Brute force by explicit enumeration, for a pattern small enough to walk.
    fn brute(g: &Graph, types: &[usize]) -> u64 {
        fn go(g: &Graph, types: &[usize], v: usize) -> u64 {
            match types.split_first() {
                None => 1,
                Some((&t, rest)) => g.out[t].nbrs(v).iter().map(|&w| go(g, rest, w as usize)).sum(),
            }
        }
        g.label_range(TYPES[types[0]].0).map(|v| go(g, types, v)).sum()
    }

    #[test]
    fn dp_matches_enumeration_forward_and_backward() {
        let mut p = Params::small(5, Popularity::Shared);
        p.label_sizes = [20, 30, 40, 30];
        p.edge_draws = [60, 80, 100, 80, 50];
        let g = generate(&p);
        for len in 2..=3 {
            for w in walks(len) {
                let b = brute(&g, &w);
                assert_eq!(path_count(&g, &w), b, "{w:?}");
                assert_eq!(*suffix_counts(&g, &w).last().unwrap(), b, "{w:?} reversed");
            }
        }
    }

    #[test]
    fn two_hop_count_is_sum_of_in_times_out() {
        let g = generate(&Params::small(9, Popularity::Shared));
        for w in walks(2) {
            let s: u64 = g
                .label_range(TYPES[w[0]].1)
                .map(|v| (g.inn[w[0]].deg(v) * g.out[w[1]].deg(v)) as u64)
                .sum();
            assert_eq!(path_count(&g, &w), s);
        }
    }

    #[test]
    fn triangle_matches_enumeration() {
        let mut p = Params::small(5, Popularity::Shared);
        p.label_sizes = [20, 30, 40, 30];
        p.edge_draws = [100, 140, 200, 80, 150];
        let g = generate(&p);
        let mut brute = 0u64;
        for a in 0..g.n {
            for &b in g.out[0].nbrs(a) {
                for &c in g.out[1].nbrs(b as usize) {
                    if g.out[4].nbrs(c as usize).binary_search(&(a as u32)).is_ok() {
                        brute += 1;
                    }
                }
            }
        }
        assert!(brute > 0);
        assert_eq!(triangle_count(&g, 0, 1, 4), brute);
    }

    #[test]
    fn prefix_and_suffix_agree_on_the_total() {
        let g = generate(&Params::small(2, Popularity::Independent));
        for w in walks(4) {
            assert_eq!(prefix_counts(&g, &w).last(), suffix_counts(&g, &w).last());
        }
    }
}
