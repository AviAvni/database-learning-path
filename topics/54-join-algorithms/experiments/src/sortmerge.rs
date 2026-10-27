//! Lane 3 (EXERCISE): the sort-merge join.
//!
//! Sort R and S by key, then walk both with two cursors. No hash table at all:
//! the random accesses of a hash join become the sequential passes of a
//! sort, and the join itself is a linear scan. Balkesen et al. found that
//! sorting is where all the cost is (their m-way variant spends it in SIMD
//! bitonic networks and multi-way merging); this lane asks what a plain
//! scalar implementation costs on this machine.
//!
//! What to build:
//!
//! 1. `sort_by_key` — sort a tuple slice by `key`. Two honest candidates: the
//!    standard library's `sort_unstable_by_key`, and an LSD radix sort (three
//!    passes of 11 bits cover a 32-bit key; each pass is a histogram and a
//!    scatter, so it is the same machinery as lane 2's partitioning).
//! 2. `merge_join` — advance the cursor with the smaller key; on equal keys,
//!    emit the cross product of the two equal-key runs (both sides may hold
//!    duplicates), then move both cursors past the runs.
//! 3. `sort_merge_join` — copy, sort, merge. Report sort and merge time
//!    separately: the interesting number is their ratio.

use crate::tuple::{JoinOutput, Tuple};

/// Sort `data` ascending by `key`. Any algorithm; ties may be in any order.
pub fn sort_by_key(data: &mut [Tuple]) {
    let _ = data;
    todo!("lane 3: std sort or an LSD radix sort")
}

/// Merge-join two slices already sorted by key. Emits every `(r, s)` pair with
/// equal keys, so equal-key runs on both sides produce their cross product.
pub fn merge_join(r_sorted: &[Tuple], s_sorted: &[Tuple]) -> JoinOutput {
    let _ = (r_sorted, s_sorted);
    todo!("lane 3: two cursors, cross product of equal-key runs")
}

/// The whole join: sort copies of both inputs, then merge.
pub fn sort_merge_join(r: &[Tuple], s: &[Tuple]) -> JoinOutput {
    let _ = (r, s);
    todo!("lane 3: copy, sort, merge")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{generate, Workload};
    use crate::npj::reference;

    #[test]
    fn sort_orders_by_key_and_keeps_every_tuple() {
        let (r, _) = generate(&Workload::uniform(50_000, 1));
        let mut sorted = r.clone();
        sort_by_key(&mut sorted);
        assert!(sorted.windows(2).all(|w| w[0].key <= w[1].key));
        let mut a: Vec<(u32, u32)> = r.iter().map(|t| (t.key, t.pay)).collect();
        let mut b: Vec<(u32, u32)> = sorted.iter().map(|t| (t.key, t.pay)).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b);
    }

    #[test]
    fn sort_handles_keys_with_the_top_bit_set() {
        let mut v: Vec<Tuple> = [u32::MAX - 1, 0x8000_0000, 1, 0x7FFF_FFFF, 0xFFFF_0000]
            .iter()
            .enumerate()
            .map(|(i, &k)| Tuple { key: k, pay: i as u32 })
            .collect();
        sort_by_key(&mut v);
        assert!(v.windows(2).all(|w| w[0].key <= w[1].key));
    }

    #[test]
    fn join_equals_reference() {
        for w in [
            Workload::uniform(40_000, 120_000),
            Workload::zipf(40_000, 120_000, 0.9),
            Workload { n_build: 40_000, n_probe: 120_000, theta: 0.0, hit_pct: 20 },
        ] {
            let (r, s) = generate(&w);
            assert_eq!(sort_merge_join(&r, &s), reference(&r, &s), "{w:?}");
        }
    }

    #[test]
    fn duplicate_runs_on_both_sides_give_the_cross_product() {
        let r: Vec<Tuple> = (0..2_000).map(|i| Tuple { key: (i % 50) as u32 + 1, pay: i }).collect();
        let s: Vec<Tuple> = (0..3_000).map(|i| Tuple { key: (i % 70) as u32 + 1, pay: i }).collect();
        let got = sort_merge_join(&r, &s);
        assert_eq!(got, reference(&r, &s));
        assert!(got.matches > 50_000);
    }

    #[test]
    fn empty_inputs() {
        let (r, _) = generate(&Workload::uniform(100, 1));
        assert_eq!(sort_merge_join(&r, &[]).matches, 0);
        assert_eq!(sort_merge_join(&[], &r).matches, 0);
    }

    #[test]
    fn does_not_mutate_its_inputs() {
        let (r, s) = generate(&Workload::uniform(1_000, 2_000));
        let (r0, s0) = (r.clone(), s.clone());
        let _ = sort_merge_join(&r, &s);
        assert_eq!((r, s), (r0, s0));
    }
}
