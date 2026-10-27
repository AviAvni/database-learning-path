//! Lane 2 (EXERCISE): the radix-partitioned hash join.
//!
//! Instead of one big table that every probe hits at a random address, first
//! scatter R and S into 2^bits partitions by hash bits, so partition `p` of R
//! can only match partition `p` of S, then join each pair with a table that
//! is small enough to live in cache. The cost is an extra read and write of
//! both inputs; the hope is that it buys back more than that in cache and TLB
//! misses. Whether it does on this machine is the lane's question.
//!
//! What to build, in the order the tests ask for it:
//!
//! 1. `partition` — a histogram pass counting tuples per partition, a prefix
//!    sum turning counts into start offsets, then a scatter pass writing each
//!    tuple to the next free slot of its partition. Partition number is
//!    `radix_of(hash32(key), shift, bits)`. Stable: tuples keep their input
//!    order inside a partition.
//! 2. Two-pass partitioning — partition by the *high* `bits_pass1` bits of the
//!    `total_bits` window (hash bits `bits_pass2 .. total_bits`), then
//!    partition each of those pieces by the low `bits_pass2` bits. Taking the
//!    high bits first makes the final partition number `p1 << bits_pass2 | p2`
//!    — the same number a single pass over `total_bits` bits assigns. Use it
//!    when one pass would need a fan-out larger than the TLB (or the L1) can
//!    keep track of (Balkesen et al. §5.1).
//! 3. `radix_join` — partition both sides, then for each partition pair build
//!    a `npj::Table`-style table from the R part and probe it with the S part.
//!    Reuse one scratch table across partitions rather than allocating per
//!    partition.
//! 4. (optional, for the numbers) software write-combine buffers: stage each
//!    partition's output in a cache-line-sized buffer and flush full lines
//!    (Balkesen et al. §5.2).

use crate::tuple::{JoinOutput, Tuple};

/// How many radix bits to use, and in how many passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RadixConfig {
    /// Bits consumed by the first partitioning pass (fan-out 2^bits_pass1).
    pub bits_pass1: u32,
    /// Bits consumed by the second pass over each first-pass partition;
    /// 0 means a single pass.
    pub bits_pass2: u32,
}

impl RadixConfig {
    pub fn total_bits(&self) -> u32 {
        self.bits_pass1 + self.bits_pass2
    }
}

/// Partition `input` by `bits` hash bits starting at hash bit `shift`.
/// Returns the tuples grouped by partition (stable inside each group) and the
/// `2^bits + 1` start offsets: partition `p` is `out[offsets[p]..offsets[p+1]]`.
pub fn partition(input: &[Tuple], shift: u32, bits: u32) -> (Vec<Tuple>, Vec<usize>) {
    let _ = (input, shift, bits);
    todo!("lane 2: histogram, prefix sum, scatter")
}

/// Partition `input` by `cfg.total_bits()` hash bits in one or two passes.
/// The result must be exactly what a single `partition(input, 0, total_bits)`
/// returns as a *set* per partition (order inside a partition may differ only
/// if you skip stability in pass 2, which the tests allow).
pub fn partition_multipass(input: &[Tuple], cfg: &RadixConfig) -> (Vec<Tuple>, Vec<usize>) {
    let _ = (input, cfg);
    todo!("lane 2: pass 1, then pass 2 inside each pass-1 partition")
}

/// The whole join.
pub fn radix_join(r: &[Tuple], s: &[Tuple], cfg: &RadixConfig) -> JoinOutput {
    let _ = (r, s, cfg);
    todo!("lane 2: partition both sides, join each partition pair with a small table")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{generate, Workload};
    use crate::hash::{hash32, radix_of};
    use crate::npj::reference;

    fn check_partitioned(input: &[Tuple], out: &[Tuple], offsets: &[usize], bits: u32) {
        assert_eq!(offsets.len(), (1usize << bits) + 1);
        assert_eq!(offsets[0], 0);
        assert_eq!(*offsets.last().unwrap(), input.len());
        for p in 0..(1usize << bits) {
            for t in &out[offsets[p]..offsets[p + 1]] {
                assert_eq!(radix_of(hash32(t.key), 0, bits), p, "tuple in the wrong partition");
            }
        }
        // a permutation of the input
        let key = |t: &Tuple| ((t.key as u64) << 32) | t.pay as u64;
        let mut a: Vec<u64> = input.iter().map(key).collect();
        let mut b: Vec<u64> = out.iter().map(key).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "partitioning must be a permutation");
    }

    #[test]
    fn single_pass_partition_groups_and_is_stable() {
        let (r, _) = generate(&Workload::uniform(20_000, 1));
        for bits in [1u32, 4, 8] {
            let (out, off) = partition(&r, 0, bits);
            check_partitioned(&r, &out, &off, bits);
            // stable: payloads inside a partition appear in input order
            let pos: std::collections::HashMap<u32, usize> =
                r.iter().enumerate().map(|(i, t)| (t.pay, i)).collect();
            for p in 0..(1usize << bits) {
                let part = &out[off[p]..off[p + 1]];
                assert!(part.windows(2).all(|w| pos[&w[0].pay] < pos[&w[1].pay]), "not stable");
            }
        }
    }

    #[test]
    fn two_pass_partition_matches_one_pass_as_a_set_per_partition() {
        let (r, _) = generate(&Workload::uniform(30_000, 1));
        let cfg = RadixConfig { bits_pass1: 4, bits_pass2: 5 };
        let (out2, off2) = partition_multipass(&r, &cfg);
        check_partitioned(&r, &out2, &off2, 9);
        let (out1, off1) = partition(&r, 0, 9);
        for p in 0..512usize {
            let mut a: Vec<u32> = out1[off1[p]..off1[p + 1]].iter().map(|t| t.pay).collect();
            let mut b: Vec<u32> = out2[off2[p]..off2[p + 1]].iter().map(|t| t.pay).collect();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "partition {p}");
        }
    }

    #[test]
    fn zero_bits_is_the_plain_join() {
        let (r, s) = generate(&Workload::uniform(3_000, 10_000));
        assert_eq!(radix_join(&r, &s, &RadixConfig { bits_pass1: 0, bits_pass2: 0 }), reference(&r, &s));
    }

    #[test]
    fn join_equals_reference_for_every_config() {
        for w in [
            Workload::uniform(40_000, 120_000),
            Workload::zipf(40_000, 120_000, 0.9),
            Workload { n_build: 40_000, n_probe: 120_000, theta: 0.0, hit_pct: 20 },
        ] {
            let (r, s) = generate(&w);
            let want = reference(&r, &s);
            for cfg in [
                RadixConfig { bits_pass1: 3, bits_pass2: 0 },
                RadixConfig { bits_pass1: 8, bits_pass2: 0 },
                RadixConfig { bits_pass1: 5, bits_pass2: 5 },
                RadixConfig { bits_pass1: 6, bits_pass2: 3 },
            ] {
                assert_eq!(radix_join(&r, &s, &cfg), want, "{cfg:?} {w:?}");
            }
        }
    }

    #[test]
    fn duplicate_keys_on_both_sides() {
        let r: Vec<Tuple> = (0..2_000).map(|i| Tuple { key: (i % 50) as u32 + 1, pay: i }).collect();
        let s: Vec<Tuple> = (0..3_000).map(|i| Tuple { key: (i % 70) as u32 + 1, pay: i }).collect();
        let cfg = RadixConfig { bits_pass1: 4, bits_pass2: 2 };
        assert_eq!(radix_join(&r, &s, &cfg), reference(&r, &s));
    }

    #[test]
    fn empty_inputs() {
        let (r, _) = generate(&Workload::uniform(100, 1));
        let cfg = RadixConfig { bits_pass1: 3, bits_pass2: 0 };
        assert_eq!(radix_join(&r, &[], &cfg).matches, 0);
        assert_eq!(radix_join(&[], &r, &cfg).matches, 0);
    }
}
