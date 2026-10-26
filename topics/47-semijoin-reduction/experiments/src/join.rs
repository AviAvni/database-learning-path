//! Left-deep hash-join plans (PROVIDED).
//!
//! `order[0]` is scanned into the first intermediate; each later relation is
//! the build side of one hash join and the running intermediate probes it. The
//! number that matters is `join_sizes`: how many tuples each join emitted.
//! Every one of them was materialised, hashed or copied — that is the work a
//! bad join order buys.

use crate::hash::{mix64, FastMap};
use crate::rel::{Relation, Var};

/// Upper bound on query variables; the lane-1 query has four.
pub const MAXV: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanRun {
    /// Tuples emitted by join 1, 2, …, n−1 (the last one is the output).
    pub join_sizes: Vec<usize>,
    /// Final output cardinality.
    pub out: usize,
    /// Order-independent checksum of the output: equal across plans that
    /// compute the same result, whatever the join order.
    pub checksum: u64,
    /// Rows inserted into hash tables (the build sides).
    pub build_rows: usize,
    /// Probes into hash tables (one per probing tuple per join).
    pub probes: usize,
}

impl PlanRun {
    /// Sum of all join outputs — the lane-1 headline.
    pub fn intermediates(&self) -> usize {
        self.join_sizes.iter().sum()
    }
}

/// Hash of one output tuple, variables taken in id order.
pub fn tuple_hash(t: &[u32; MAXV], nvars: usize) -> u64 {
    let mut h = 0u64;
    for &v in t.iter().take(nvars) {
        h = mix64(h ^ v as u64);
    }
    h
}

/// Run the left-deep plan `order` over `rels` (the query's relations, or
/// reduced copies of them with the same variables).
pub fn left_deep(rels: &[Relation], nvars: usize, order: &[usize]) -> PlanRun {
    assert!(nvars <= MAXV);
    let first = &rels[order[0]];
    let mut bound = [false; MAXV];
    let mut inter: Vec<[u32; MAXV]> = Vec::with_capacity(first.len());
    for r in 0..first.len() {
        let mut t = [0u32; MAXV];
        for (c, &v) in first.vars.iter().enumerate() {
            t[v] = first.cols[c][r];
        }
        inter.push(t);
    }
    for &v in &first.vars {
        bound[v] = true;
    }

    let mut join_sizes = Vec::new();
    let (mut build_rows, mut probes) = (0, 0);
    for &ri in &order[1..] {
        let rel = &rels[ri];
        let shared: Vec<Var> = rel.vars.iter().copied().filter(|&v| bound[v]).collect();
        assert!(!shared.is_empty(), "plan step {} would be a cross product", rel.name);
        let key = shared[0];
        let key_col = rel.col(key);

        // build: key -> row indices, in row order
        let mut ht: FastMap<u32, Vec<u32>> = FastMap::default();
        for (r, &k) in key_col.iter().enumerate() {
            ht.entry(k).or_default().push(r as u32);
        }
        build_rows += rel.len();

        // probe
        let mut next = Vec::new();
        for t in &inter {
            probes += 1;
            if let Some(rows) = ht.get(&t[key]) {
                'row: for &r in rows {
                    let mut u = *t;
                    for (c, &v) in rel.vars.iter().enumerate() {
                        let x = rel.cols[c][r as usize];
                        if bound[v] {
                            if u[v] != x {
                                continue 'row; // a second shared variable disagrees
                            }
                        } else {
                            u[v] = x;
                        }
                    }
                    next.push(u);
                }
            }
        }
        for &v in &rel.vars {
            bound[v] = true;
        }
        join_sizes.push(next.len());
        inter = next;
    }

    let checksum = inter.iter().fold(0u64, |acc, t| acc.wrapping_add(tuple_hash(t, nvars)));
    PlanRun { out: inter.len(), join_sizes, checksum, build_rows, probes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rel::Relation;

    /// R(a,b) = {(1,10),(2,10),(3,20)}, S(b,c) = {(10,100),(10,101),(30,102)},
    /// T(c) = {100}. Output: (1,10,100), (2,10,100) — 2 rows.
    fn tiny() -> Vec<Relation> {
        vec![
            Relation::new("R", vec![0, 1], vec![vec![1, 2, 3], vec![10, 10, 20]]),
            Relation::new("S", vec![1, 2], vec![vec![10, 10, 30], vec![100, 101, 102]]),
            Relation::new("T", vec![2], vec![vec![100]]),
        ]
    }

    #[test]
    fn every_order_gives_the_same_answer() {
        let rels = tiny();
        let a = left_deep(&rels, 3, &[0, 1, 2]);
        let b = left_deep(&rels, 3, &[2, 1, 0]);
        assert_eq!(a.out, 2);
        assert_eq!(a.out, b.out);
        assert_eq!(a.checksum, b.checksum);
        // R⋈S = 4 rows (two R rows × two S rows on b=10), then T keeps 2.
        assert_eq!(a.join_sizes, vec![4, 2]);
        // T⋈S = 1 row, then R matches it twice.
        assert_eq!(b.join_sizes, vec![1, 2]);
    }
}
