//! Lane 1 (PROVIDED): the no-partitioning hash join.
//!
//! One table shared by every probe, no partitioning pass. Open addressing in
//! **buckets of four 8-byte slots** (32 bytes, never straddling a cache line):
//! a tuple goes into the first free slot of its home bucket, or of the next
//! bucket if that one is full; a probe scans whole buckets and stops at the
//! first bucket that still has an empty slot. A slot holds the whole tuple
//! (key in the high 32 bits, payload in the low 32). Capacity is the next power
//! of two at or above 2 × |R| — a load factor between 25% and 50%, the same
//! sizing rule as DuckDB's `DEFAULT_LOAD_FACTOR = 2.0` (`join_hashtable.hpp:626`).
//!
//! Why buckets: a one-slot-at-a-time probe loop has a data-dependent trip count
//! and mispredicts its exit branch often; measured on the M3 it costs ~10 ns
//! per probe *even when the whole table is 16 slots in L1* (README, lane 1a).
//! Reading two buckets (eight slots) per step with arithmetic instead of
//! branches, and branching only when both are full, takes that floor away, so
//! the cache effects the lanes are about can show. `probe_scalar` keeps the
//! one-slot-at-a-time loop for that comparison.
//!
//! Duplicate build keys are allowed (each takes its own slot) and a probe
//! reports every match, so the table is a general equi-join, not just a
//! foreign-key one.

use crate::hash::{hash32, slot_of};
use crate::tuple::{JoinOutput, Tuple};

pub const EMPTY: u64 = u64::MAX;
const B: usize = 4; // slots per bucket

#[inline(always)]
fn pack(t: Tuple) -> u64 {
    ((t.key as u64) << 32) | t.pay as u64
}

/// Capacity in slots for `n` build tuples: next_power_of_two(2n), min 16.
pub fn capacity_for(n: usize) -> usize {
    (2 * n).next_power_of_two().max(16)
}

/// Insert `t` into a table of `1 << bits` slots (`bits >= 4`), each of which is
/// `EMPTY` or occupied. The table must have room (load factor below 1).
#[inline]
pub fn insert(slots: &mut [u64], bits: u32, t: Tuple) {
    let mask = slots.len() - 1;
    let mut b = slot_of(hash32(t.key), bits) & !(B - 1);
    loop {
        for j in 0..B {
            if slots[b + j] == EMPTY {
                slots[b + j] = pack(t);
                return;
            }
        }
        b = (b + B) & mask;
    }
}

/// Probe one tuple: add every match to `out`. Each step reads the home bucket
/// *and* the next one (8 slots, 64 bytes) unconditionally and stops if either
/// still has a free slot; the extra bucket cannot hold a false match, because a
/// key is only ever stored past a bucket that was full when it was inserted.
#[inline(always)]
pub fn probe_one(slots: &[u64], bits: u32, t: Tuple, out: &mut JoinOutput) {
    let mask = slots.len() - 1;
    let mut b = slot_of(hash32(t.key), bits) & !(B - 1);
    loop {
        let b2 = (b + B) & mask;
        let v = [
            slots[b], slots[b + 1], slots[b + 2], slots[b + 3],
            slots[b2], slots[b2 + 1], slots[b2 + 2], slots[b2 + 3],
        ];
        // which of the 8 slots hold the probe key, as a bit mask built without
        // branches; then one iteration per set bit. A foreign-key probe has
        // exactly one, so the loop's pattern is fixed and predicted; branching
        // slot by slot instead would mispredict on *which* slot matched.
        let mut hits = 0u32;
        for j in 0..2 * B {
            hits |= (((v[j] >> 32) as u32 == t.key) as u32) << j;
        }
        while hits != 0 {
            let j = hits.trailing_zeros() as usize;
            hits &= hits - 1;
            out.add(v[j] as u32, t.pay);
        }
        // a bucket has a free slot iff its last slot is free (slots fill in
        // order); `|` rather than `||` keeps this a single branch
        if (v[B - 1] == EMPTY) | (v[2 * B - 1] == EMPTY) {
            return;
        }
        b = (b2 + B) & mask;
    }
}

pub struct Table {
    slots: Vec<u64>,
    bits: u32,
}

impl Table {
    pub fn capacity_for(n: usize) -> usize {
        capacity_for(n)
    }

    /// Build the table from R. The empty marker is the all-ones slot, i.e.
    /// key == payload == u32::MAX, which the generator never produces.
    pub fn build(r: &[Tuple]) -> Table {
        Self::build_with(r, 2)
    }

    /// Build with `slots_per_tuple` slots per build tuple (rounded up to a
    /// power of two in total): 2 is the default and DuckDB's rule; larger
    /// values trade memory for fewer full buckets, so fewer unpredictable
    /// branches in the probe.
    pub fn build_with(r: &[Tuple], slots_per_tuple: usize) -> Table {
        assert!(slots_per_tuple >= 2);
        let cap = (slots_per_tuple * r.len()).next_power_of_two().max(16);
        let bits = cap.trailing_zeros();
        let mut slots = vec![EMPTY; cap];
        for &t in r {
            insert(&mut slots, bits, t);
        }
        Table { slots, bits }
    }

    /// The home bucket of a key in this table (for ordering probes by table
    /// address; see lane 1d).
    pub fn home_bucket(&self, key: u32) -> usize {
        slot_of(hash32(key), self.bits) & !(B - 1)
    }

    pub fn bytes(&self) -> usize {
        self.slots.len() * 8
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Probe S one tuple at a time, a bucket per step.
    pub fn probe(&self, s: &[Tuple]) -> JoinOutput {
        let mut out = JoinOutput::default();
        for &t in s {
            probe_one(&self.slots, self.bits, t, &mut out);
        }
        out
    }

    /// Probe S one *slot* per step, branching on every slot: the textbook
    /// linear-probing loop, kept to measure what its mispredicted exit costs.
    pub fn probe_scalar(&self, s: &[Tuple]) -> JoinOutput {
        let mask = self.slots.len() - 1;
        let mut out = JoinOutput::default();
        for &t in s {
            let mut i = slot_of(hash32(t.key), self.bits) & !(B - 1);
            loop {
                let v = self.slots[i];
                if v == EMPTY {
                    break;
                }
                if (v >> 32) as u32 == t.key {
                    out.add(v as u32, t.pay);
                }
                i = (i + 1) & mask;
            }
        }
        out
    }

    /// Probe S in batches of 32: first compute every home bucket and load its
    /// first slot (32 independent loads, so the cache misses overlap), then
    /// finish each probe. The portable cousin of the software prefetching
    /// Umbra's non-partitioned join uses (Bandle et al. §4.3).
    pub fn probe_batched(&self, s: &[Tuple]) -> JoinOutput {
        const BATCH: usize = 32;
        let mut out = JoinOutput::default();
        let mut touched = 0u64;
        for chunk in s.chunks(BATCH) {
            for t in chunk {
                let b = slot_of(hash32(t.key), self.bits) & !(B - 1);
                touched ^= self.slots[b];
            }
            for &t in chunk {
                probe_one(&self.slots, self.bits, t, &mut out);
            }
        }
        std::hint::black_box(touched);
        out
    }

    /// Probe S with `threads` threads, each taking an equal contiguous chunk
    /// of S and probing the same read-only table. Build stays single-threaded.
    pub fn probe_parallel(&self, s: &[Tuple], threads: usize) -> JoinOutput {
        let chunk = s.len().div_ceil(threads.max(1)).max(1);
        let mut total = JoinOutput::default();
        std::thread::scope(|sc| {
            let hs: Vec<_> = s
                .chunks(chunk)
                .map(|c| {
                    sc.spawn(move || {
                        crate::timing::prefer_performance_cores();
                        self.probe_batched(c)
                    })
                })
                .collect();
            for h in hs {
                total.merge(h.join().unwrap());
            }
        });
        total
    }
}

/// The whole join: build then probe.
pub fn npj(r: &[Tuple], s: &[Tuple]) -> JoinOutput {
    Table::build(r).probe(s)
}

/// Reference join through `std::collections::HashMap`, used by every lane's
/// tests as the oracle.
pub fn reference(r: &[Tuple], s: &[Tuple]) -> JoinOutput {
    let mut m: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    for t in r {
        m.entry(t.key).or_default().push(t.pay);
    }
    let mut out = JoinOutput::default();
    for t in s {
        if let Some(v) = m.get(&t.key) {
            for &rp in v {
                out.add(rp, t.pay);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{generate, Workload};

    #[test]
    fn matches_std_hashmap() {
        for (w, name) in [
            (Workload::uniform(5_000, 40_000), "uniform"),
            (Workload::zipf(5_000, 40_000, 0.9), "zipf"),
            (Workload { n_build: 5_000, n_probe: 40_000, theta: 0.0, hit_pct: 30 }, "30% hits"),
        ] {
            let (r, s) = generate(&w);
            let want = reference(&r, &s);
            assert_eq!(npj(&r, &s), want, "{name}");
            let t = Table::build(&r);
            assert_eq!(t.probe_batched(&s), want, "{name} batched");
            assert_eq!(t.probe_scalar(&s), want, "{name} scalar");
            assert_eq!(t.probe_parallel(&s, 3), want, "{name} parallel");
        }
    }

    #[test]
    fn duplicate_build_keys_report_every_match() {
        let r: Vec<Tuple> = (0..100).map(|i| Tuple { key: 7, pay: i }).collect();
        let s = [Tuple { key: 7, pay: 0 }, Tuple { key: 8, pay: 1 }];
        assert_eq!(npj(&r, &s).matches, 100);
    }

    #[test]
    fn load_factor_between_quarter_and_half() {
        for n in [100usize, 1000, 1025, 1 << 20] {
            let c = Table::capacity_for(n);
            assert!(c.is_power_of_two());
            let lf = n as f64 / c as f64;
            assert!((0.25..=0.5).contains(&lf) || c == 16, "n={n} lf={lf}");
        }
    }
}
