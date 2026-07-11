//! The delta-join: joins are BILINEAR, so their incremental form is pure
//! algebra — no re-execution, just three smaller joins:
//!
//!   (A + dA) ⋈ (B + dB) = A⋈B + dA⋈B + A⋈dB + dA⋈dB
//!                          ^^^^ already have    ^^^^^^^^^^ the delta
//!
//! This identity is the heart of differential dataflow's join
//! (join_traces, differential operators/join.rs:69: each incoming batch
//! of one input joins the ARRANGED history of the other) and DBSP's
//! join_generic (feldera dbsp operator/join.rs:350). Materialize's dogs^3
//! delta-join plans (compute/render/join/delta_join.rs:47) are this same
//! identity applied per-input across n-way joins to avoid intermediate
//! arrangements.

use crate::zset::ZSet;
use std::collections::HashMap;
use std::hash::Hash;

/// PROVIDED oracle — hash join with weight multiplication. Weight
/// multiplication is what makes the bilinear identity hold: a pair
/// (delete, insert) = (-1)·(+1) = -1 retracts an output row.
pub fn join_oracle<K, A, B>(a: &ZSet<(K, A)>, b: &ZSet<(K, B)>) -> ZSet<(K, A, B)>
where
    K: Ord + Clone + Hash,
    A: Ord + Clone,
    B: Ord + Clone,
{
    let mut index: HashMap<&K, Vec<(&A, i64)>> = HashMap::new();
    for ((k, va), w) in a.iter() {
        index.entry(k).or_default().push((va, *w));
    }
    let mut out = Vec::new();
    for ((k, vb), wb) in b.iter() {
        if let Some(matches) = index.get(k) {
            for (va, wa) in matches {
                out.push(((k.clone(), (*va).clone(), vb.clone()), wa * wb));
            }
        }
    }
    ZSet::from_updates(out)
}

/// STUB — the bilinear delta: dA⋈B + A⋈dB + dA⋈dB (A, B are the states
/// BEFORE the deltas). Compose from `join_oracle` — the point is the
/// algebra, not a new join kernel.
pub fn delta_join<K, A, B>(
    _a: &ZSet<(K, A)>,
    _da: &ZSet<(K, A)>,
    _b: &ZSet<(K, B)>,
    _db: &ZSet<(K, B)>,
) -> ZSet<(K, A, B)>
where
    K: Ord + Clone + Hash,
    A: Ord + Clone,
    B: Ord + Clone,
{
    todo!("dA join B  +  A join dB  +  dA join dB")
}

/// An operator that OWNS its arranged inputs — differential's Arranged
/// (arrange/arrangement.rs:45) reduced to its essence: the integrated
/// input collections kept resident so each delta batch only does
/// delta-sized work against them.
pub struct IncrementalJoin<K: Ord + Clone + Hash, A: Ord + Clone, B: Ord + Clone> {
    pub a: ZSet<(K, A)>,
    pub b: ZSet<(K, B)>,
}

impl<K: Ord + Clone + Hash, A: Ord + Clone, B: Ord + Clone> IncrementalJoin<K, A, B> {
    pub fn new() -> Self {
        IncrementalJoin { a: ZSet::new(), b: ZSet::new() }
    }

    /// STUB — emit d(A⋈B) for this batch via `delta_join`, THEN fold the
    /// deltas into the arranged state (order matters — the identity
    /// wants pre-batch A and B).
    pub fn step(&mut self, _da: &ZSet<(K, A)>, _db: &ZSet<(K, B)>) -> ZSet<(K, A, B)> {
        todo!("delta_join against current state, then integrate the deltas")
    }
}

impl<K: Ord + Clone + Hash, A: Ord + Clone, B: Ord + Clone> Default for IncrementalJoin<K, A, B> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;

    fn rand_zset(n: usize, keys: u32, vals: u32, seed: u64) -> ZSet<(u32, u32)> {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        ZSet::from_updates(
            (0..n)
                .map(|_| {
                    ((rng.gen_range(0..keys), rng.gen_range(0..vals)), if rng.gen_bool(0.8) { 1 } else { -1 })
                })
                .collect(),
        )
    }

    #[test]
    fn delta_join_matches_the_algebra() {
        let a = rand_zset(500, 40, 20, 1);
        let b = rand_zset(500, 40, 20, 2);
        let da = rand_zset(60, 40, 20, 3);
        let db = rand_zset(60, 40, 20, 4);
        let expect = join_oracle(&a.merge(&da), &b.merge(&db)).merge(&join_oracle(&a, &b).negate());
        assert_eq!(delta_join(&a, &da, &b, &db), expect);
    }

    #[test]
    fn incremental_join_over_batches() {
        let mut ij = IncrementalJoin::new();
        let mut a_full = ZSet::new();
        let mut b_full = ZSet::new();
        let mut out = ZSet::new();
        for seed in 0..30u64 {
            let da = rand_zset(40, 25, 10, 100 + seed);
            let db = rand_zset(40, 25, 10, 200 + seed);
            out = out.merge(&ij.step(&da, &db));
            a_full = a_full.merge(&da);
            b_full = b_full.merge(&db);
            assert_eq!(out, join_oracle(&a_full, &b_full), "diverged at batch {}", seed);
        }
    }

    #[test]
    fn delete_retracts_output_rows() {
        let mut ij = IncrementalJoin::new();
        let ins = ZSet::from_updates(vec![((7u32, 1u32), 1)]);
        ij.step(&ins, &ins);
        let del = ins.negate();
        ij.step(&del, &ZSet::new());
        // one side gone: joining state must now be empty
        let d = ij.step(&ZSet::new(), &ZSet::from_updates(vec![((7, 9), 1)]));
        assert!(d.is_empty(), "join against deleted row produced {:?}", d.entries);
    }
}
