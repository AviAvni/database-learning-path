//! Eviction policy shootout: CLOCK vs strict-LRU vs FIFO — PROVIDED, runs now.
//!
//! Measures BOTH dimensions of the RUM tradeoff for replacement policies:
//!   - hit rate on a Zipf(0.99) trace (printed once per policy)
//!   - ns per access (criterion timing) — strict LRU pays list surgery on
//!     every HIT; CLOCK pays only a saturating increment.
//!
//! This is a *policy simulator* over u64 page ids (no real I/O — a miss just
//! counts). Your real pool's numbers come from pool_vs_mmap.

use criterion::{criterion_group, criterion_main, Criterion};
use rand::prelude::*;
use rand_distr::Zipf;
use std::collections::{HashMap, VecDeque};

const CAPACITY: usize = 4096;
const SPACE: u64 = 65_536; // page id universe = 16× capacity
const TRACE: usize = 1_000_000;

trait Policy {
    fn new(cap: usize) -> Self;
    /// Returns true on hit.
    fn access(&mut self, page: u64) -> bool;
}

// ---------------- CLOCK ----------------
struct Clock {
    frames: Vec<(u64, u8)>, // (page, usage)
    map: HashMap<u64, usize>,
    hand: usize,
    cap: usize,
}

impl Policy for Clock {
    fn new(cap: usize) -> Self {
        Clock { frames: Vec::with_capacity(cap), map: HashMap::new(), hand: 0, cap }
    }
    fn access(&mut self, page: u64) -> bool {
        if let Some(&i) = self.map.get(&page) {
            let u = &mut self.frames[i].1;
            *u = (*u + 1).min(5);
            return true;
        }
        if self.frames.len() < self.cap {
            self.map.insert(page, self.frames.len());
            self.frames.push((page, 1));
            return false;
        }
        loop {
            let (p, u) = self.frames[self.hand];
            if u == 0 {
                self.map.remove(&p);
                self.frames[self.hand] = (page, 1);
                self.map.insert(page, self.hand);
                self.hand = (self.hand + 1) % self.cap;
                return false;
            }
            self.frames[self.hand].1 = u - 1;
            self.hand = (self.hand + 1) % self.cap;
        }
    }
}

// ---------------- strict LRU ----------------
// VecDeque + map; every HIT removes + repushes (the cost nobody wants).
struct Lru {
    order: VecDeque<u64>,
    map: HashMap<u64, ()>,
    cap: usize,
}

impl Policy for Lru {
    fn new(cap: usize) -> Self {
        Lru { order: VecDeque::new(), map: HashMap::new(), cap }
    }
    fn access(&mut self, page: u64) -> bool {
        let hit = self.map.contains_key(&page);
        if hit {
            // O(n) removal — this is the honest cost of "strict" without an
            // intrusive doubly-linked list; even with one, it's pointer
            // surgery + cache misses on every hit.
            let pos = self.order.iter().position(|&p| p == page).unwrap();
            self.order.remove(pos);
        } else if self.map.len() >= self.cap {
            let victim = self.order.pop_front().unwrap();
            self.map.remove(&victim);
        }
        self.order.push_back(page);
        self.map.insert(page, ());
        hit
    }
}

// ---------------- FIFO ----------------
struct Fifo {
    order: VecDeque<u64>,
    map: HashMap<u64, ()>,
    cap: usize,
}

impl Policy for Fifo {
    fn new(cap: usize) -> Self {
        Fifo { order: VecDeque::new(), map: HashMap::new(), cap }
    }
    fn access(&mut self, page: u64) -> bool {
        if self.map.contains_key(&page) {
            return true; // hits touch nothing — that's FIFO's whole appeal
        }
        if self.map.len() >= self.cap {
            let victim = self.order.pop_front().unwrap();
            self.map.remove(&victim);
        }
        self.order.push_back(page);
        self.map.insert(page, ());
        false
    }
}

fn trace() -> Vec<u64> {
    let mut rng = StdRng::seed_from_u64(7);
    let dist = Zipf::new(SPACE, 0.99).unwrap();
    (0..TRACE).map(|_| rng.sample(dist) as u64 - 1).collect()
}

fn hit_rate<P: Policy>(t: &[u64]) -> f64 {
    let mut p = P::new(CAPACITY);
    let hits = t.iter().filter(|&&pg| p.access(pg)).count();
    hits as f64 / t.len() as f64
}

fn bench(c: &mut Criterion) {
    let t = trace();
    println!("hit rates on Zipf(0.99), capacity {CAPACITY}, universe {SPACE}:");
    println!("  CLOCK {:.3}", hit_rate::<Clock>(&t));
    println!("  LRU   {:.3}", hit_rate::<Lru>(&t));
    println!("  FIFO  {:.3}", hit_rate::<Fifo>(&t));

    let mut g = c.benchmark_group("eviction_ns_per_access");
    g.bench_function("clock", |b| {
        b.iter(|| {
            let mut p = Clock::new(CAPACITY);
            t.iter().filter(|&&pg| p.access(pg)).count()
        })
    });
    g.bench_function("lru_strict", |b| {
        b.iter(|| {
            let mut p = Lru::new(CAPACITY);
            t.iter().filter(|&&pg| p.access(pg)).count()
        })
    });
    g.bench_function("fifo", |b| {
        b.iter(|| {
            let mut p = Fifo::new(CAPACITY);
            t.iter().filter(|&&pg| p.access(pg)).count()
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
