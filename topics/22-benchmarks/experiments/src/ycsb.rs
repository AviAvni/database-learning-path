//! YCSB core-workload driver, closed-loop, single thread. The six
//! canonical mixes A-F (go-ycsb `workloads/workload{a..f}`) against a
//! BTreeMap store (ordered, because workload E needs scans).
//!
//! Deliberate simplifications vs real YCSB: no threads, no target
//! rate, no field-level ops — this driver exists to expose the SHAPE
//! of each mix and what skew does to it, not to certify anything.

use crate::hist::Hist;
use crate::zipf::KeyGen;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::BTreeMap;
use std::time::Instant;

pub struct Store {
    map: BTreeMap<u64, Vec<u8>>,
}

impl Store {
    pub fn preload(n: usize) -> Store {
        let mut map = BTreeMap::new();
        for i in 0..n {
            map.insert(i as u64, vec![0u8; 100]);
        }
        Store { map }
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn read(&self, k: u64) -> Option<&Vec<u8>> {
        self.map.get(&k)
    }
    pub fn update(&mut self, k: u64, v: Vec<u8>) {
        self.map.insert(k, v);
    }
    pub fn insert(&mut self, k: u64, v: Vec<u8>) {
        self.map.insert(k, v);
    }
    pub fn scan(&self, start: u64, len: usize) -> usize {
        self.map.range(start..).take(len).count()
    }
}

#[derive(Clone, Copy)]
pub struct Mix {
    pub name: &'static str,
    pub read: f64,
    pub update: f64,
    pub insert: f64,
    pub scan: f64,
    pub rmw: f64,
}

/// The six core workloads (proportions from the shipped property
/// files, e.g. go-ycsb workloada:31-34). D's "latest" distribution
/// is approximated by whatever KeyGen you pass — see notes.md.
pub const WORKLOADS: [Mix; 6] = [
    Mix { name: "A update-heavy", read: 0.5, update: 0.5, insert: 0.0, scan: 0.0, rmw: 0.0 },
    Mix { name: "B read-mostly", read: 0.95, update: 0.05, insert: 0.0, scan: 0.0, rmw: 0.0 },
    Mix { name: "C read-only", read: 1.0, update: 0.0, insert: 0.0, scan: 0.0, rmw: 0.0 },
    Mix { name: "D read-latest", read: 0.95, update: 0.0, insert: 0.05, scan: 0.0, rmw: 0.0 },
    Mix { name: "E short-ranges", read: 0.0, update: 0.0, insert: 0.05, scan: 0.95, rmw: 0.0 },
    Mix { name: "F read-mod-write", read: 0.5, update: 0.0, insert: 0.0, scan: 0.0, rmw: 0.5 },
];

pub struct Report {
    pub ops: usize,
    pub elapsed_s: f64,
    pub hist: Hist, // nanoseconds per op
    pub counts: [usize; 5], // read, update, insert, scan, rmw
}

impl Report {
    pub fn mops(&self) -> f64 {
        self.ops as f64 / self.elapsed_s / 1e6
    }
}

pub fn run_workload(
    store: &mut Store,
    mix: &Mix,
    keygen: &mut dyn KeyGen,
    ops: usize,
    seed: u64,
) -> Report {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut hist = Hist::default();
    let mut counts = [0usize; 5];
    let mut next_id = store.len() as u64;
    let t0 = Instant::now();
    for _ in 0..ops {
        let k = keygen.next(next_id as usize) as u64;
        let r: f64 = rng.gen();
        let t = Instant::now();
        if r < mix.read {
            std::hint::black_box(store.read(k));
            counts[0] += 1;
        } else if r < mix.read + mix.update {
            store.update(k, vec![1u8; 100]);
            counts[1] += 1;
        } else if r < mix.read + mix.update + mix.insert {
            store.insert(next_id, vec![2u8; 100]);
            next_id += 1;
            counts[2] += 1;
        } else if r < mix.read + mix.update + mix.insert + mix.scan {
            std::hint::black_box(store.scan(k, 100));
            counts[3] += 1;
        } else {
            let v = store.read(k).cloned().unwrap_or_default();
            store.update(k, v);
            counts[4] += 1;
        }
        hist.record(t.elapsed().as_nanos() as u64);
    }
    Report { ops, elapsed_s: t0.elapsed().as_secs_f64(), hist, counts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zipf::Uniform;

    #[test]
    fn proportions_honored() {
        let mut s = Store::preload(10_000);
        let r = run_workload(&mut s, &WORKLOADS[0], &mut Uniform::new(1), 100_000, 2);
        let frac = r.counts[0] as f64 / r.ops as f64;
        assert!((frac - 0.5).abs() < 0.02, "read fraction {frac}");
    }

    #[test]
    fn read_only_never_mutates() {
        let mut s = Store::preload(1000);
        run_workload(&mut s, &WORKLOADS[2], &mut Uniform::new(3), 10_000, 4);
        assert_eq!(s.len(), 1000);
    }

    #[test]
    fn inserts_grow_the_keyspace() {
        let mut s = Store::preload(1000);
        let r = run_workload(&mut s, &WORKLOADS[3], &mut Uniform::new(5), 10_000, 6);
        assert_eq!(s.len(), 1000 + r.counts[2]);
    }
}
