//! Lane 1b (PROVIDED): an in-memory B+tree built for cache behaviour, against
//! the std one it claims to replace.
//!
//!   cargo run --release --bin bbtree_vs_std
//!
//! `bbtree::unsync::BBTreeMap` (crate 0.1.5, source read at commit f0a5d36) is
//! a drop-in for `std::collections::BTreeMap`. This lane measures three things
//! on it, each against std on the same keys:
//!
//!   1. insert, random order
//!   2. get, probes in a different random order than the inserts
//!   3. get_many, 16 probes per call: the batch API that interleaves the
//!      descents with software prefetch (reading-bbtree.md, Step 5)
//!
//! on u64 keys at 100K / 1M / 10M, and String keys at 1M. Nothing here touches
//! your `src/page.rs` or `src/btree.rs`; it runs on a fresh clone and exits 0.
//!
//! Predict in notes.md BEFORE running: at 10M u64, what does a single `get`
//! cost in std (how many dependent cache misses?), and how much of that can
//! 16-wide batching hide?
//!
//! Method: seeded splitmix64 keys (same keys every run), black_box on every
//! probe, best of REPS for the read lanes (a lookup lane is idempotent),
//! single run for insert. Every probe must hit; the value checksum is printed
//! so two runs can be compared exactly. Timings are single-thread, system
//! allocator (the bbtree authors benchmark with mimalloc), no CPU pinning.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use bbtree::unsync::BBTreeMap;

const REPS: usize = 3;
const BATCH: usize = 16;

/// splitmix64: seeded, dependency-free, identical on every machine.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn shuffle<T>(v: &mut [T], seed: u64) {
    let mut r = Rng(seed);
    for i in (1..v.len()).rev() {
        v.swap(i, (r.next() % (i as u64 + 1)) as usize);
    }
}

fn ns_per(t: Instant, n: usize) -> f64 {
    t.elapsed().as_nanos() as f64 / n as f64
}

fn best_of(mut f: impl FnMut() -> (f64, u64)) -> (f64, u64) {
    let mut best = f64::MAX;
    let mut sum = 0;
    for _ in 0..REPS {
        let (ns, s) = f();
        best = best.min(ns);
        sum = s;
    }
    (best, sum)
}

fn run_u64(n: usize) {
    let mut r = Rng(42);
    let keys: Vec<u64> = (0..n).map(|_| r.next()).collect();
    let mut probe = keys.clone();
    shuffle(&mut probe, 7);

    let t = Instant::now();
    let mut std_map = BTreeMap::new();
    for &k in &keys {
        std_map.insert(k, k);
    }
    let std_ins = ns_per(t, n);

    let t = Instant::now();
    let mut bb = BBTreeMap::new();
    for &k in &keys {
        bb.insert(k, k);
    }
    let bb_ins = ns_per(t, n);
    assert_eq!(std_map.len(), bb.len());

    let (std_get, std_sum) = best_of(|| {
        let t = Instant::now();
        let mut acc = 0u64;
        for k in &probe {
            acc = acc.wrapping_add(*std_map.get(black_box(k)).expect("probe is a key"));
        }
        (ns_per(t, n), black_box(acc))
    });
    let (bb_get, bb_sum) = best_of(|| {
        let t = Instant::now();
        let mut acc = 0u64;
        for k in &probe {
            acc = acc.wrapping_add(*bb.get(black_box(k)).expect("probe is a key"));
        }
        (ns_per(t, n), black_box(acc))
    });
    let used = n / BATCH * BATCH;
    let (bb_many, many_sum) = best_of(|| {
        let t = Instant::now();
        let mut acc = 0u64;
        for c in probe.chunks_exact(BATCH) {
            let ks: [&u64; BATCH] = std::array::from_fn(|i| &c[i]);
            for v in bb.get_many(black_box(ks)) {
                acc = acc.wrapping_add(*v.expect("probe is a key"));
            }
        }
        (ns_per(t, used), black_box(acc))
    });
    assert_eq!(std_sum, bb_sum, "get must agree with std");
    // get_many covers the first `used` probes only; compare against them.
    let expect: u64 = probe[..used].iter().fold(0, |a, &k| a.wrapping_add(k));
    assert_eq!(many_sum, expect, "get_many must agree with get");

    println!(
        "u64    n={n:>9}  insert std {std_ins:6.1} bb {bb_ins:6.1} ns ({:.2}x) | get std {std_get:6.1} bb {bb_get:6.1} ns ({:.2}x) | get_many16 bb {bb_many:6.1} ns ({:.2}x vs std get)  checksum {std_sum:016x}",
        std_ins / bb_ins,
        std_get / bb_get,
        std_get / bb_many
    );
}

fn run_string(n: usize) {
    let mut r = Rng(43);
    let keys: Vec<String> = (0..n).map(|_| format!("user:{:016x}", r.next())).collect();
    let mut order: Vec<usize> = (0..n).collect();
    shuffle(&mut order, 8);

    let t = Instant::now();
    let mut std_map = BTreeMap::new();
    for (i, k) in keys.iter().enumerate() {
        std_map.insert(k.clone(), i as u64);
    }
    let std_ins = ns_per(t, n);
    let t = Instant::now();
    let mut bb = BBTreeMap::new();
    for (i, k) in keys.iter().enumerate() {
        bb.insert(k.clone(), i as u64);
    }
    let bb_ins = ns_per(t, n);

    let (std_get, std_sum) = best_of(|| {
        let t = Instant::now();
        let mut acc = 0u64;
        for &i in &order {
            acc = acc.wrapping_add(*std_map.get(black_box(keys[i].as_str())).expect("hit"));
        }
        (ns_per(t, n), black_box(acc))
    });
    let (bb_get, bb_sum) = best_of(|| {
        let t = Instant::now();
        let mut acc = 0u64;
        for &i in &order {
            acc = acc.wrapping_add(*bb.get(black_box(keys[i].as_str())).expect("hit"));
        }
        (ns_per(t, n), black_box(acc))
    });
    assert_eq!(std_sum, bb_sum, "get must agree with std");
    println!(
        "String n={n:>9}  insert std {std_ins:6.1} bb {bb_ins:6.1} ns ({:.2}x) | get std {std_get:6.1} bb {bb_get:6.1} ns ({:.2}x)  (Auto strategy: inline keys on for String)  checksum {std_sum:016x}",
        std_ins / bb_ins,
        std_get / bb_get
    );
}

fn main() {
    println!(
        "lane 1b: bbtree 0.1.5 unsync::BBTreeMap vs std BTreeMap (single thread, system allocator)"
    );
    println!("get = best of {REPS}; insert = one run; get_many = {BATCH} keys per call\n");
    for n in [100_000, 1_000_000, 10_000_000] {
        run_u64(n);
    }
    run_string(1_000_000);
}
