//! Scaling shootout: 1→16 threads, 90% contains / 10% insert, u64 keys.
//!
//!   cargo run --release --bin scaling
//!
//! Contestants:
//!   global    — Mutex<BTreeSet>          (runs now)
//!   sharded   — 16 x Mutex<BTreeSet>     (runs now)
//!   crossbeam — crossbeam_skiplist::SkipSet, the reference (runs now)
//!   mine      — your ConcurrentSet       (panics until implemented)
//!
//! Predict in notes.md first: the SHAPE of each line as threads grow
//! (flat? linear? inverse?), and where sharded stops helping.

use concurrency_experiments::concurrent_set::ConcurrentSet;
use crossbeam_skiplist::SkipSet;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Instant;

const OPS_PER_THREAD: usize = 200_000;
const KEYSPACE: u64 = 100_000;
const PRELOAD: u64 = 50_000;
const WRITE_PCT: u32 = 10;

trait Set: Send + Sync + 'static {
    fn insert(&self, k: u64);
    fn contains(&self, k: u64) -> bool;
}

struct Global(Mutex<BTreeSet<u64>>);
impl Set for Global {
    fn insert(&self, k: u64) {
        self.0.lock().unwrap().insert(k);
    }
    fn contains(&self, k: u64) -> bool {
        self.0.lock().unwrap().contains(&k)
    }
}

struct Sharded([Mutex<BTreeSet<u64>>; 16]);
impl Sharded {
    fn shard(&self, k: u64) -> &Mutex<BTreeSet<u64>> {
        &self.0[(k.wrapping_mul(0x9e3779b97f4a7c15) >> 60) as usize]
    }
}
impl Set for Sharded {
    fn insert(&self, k: u64) {
        self.shard(k).lock().unwrap().insert(k);
    }
    fn contains(&self, k: u64) -> bool {
        self.shard(k).lock().unwrap().contains(&k)
    }
}

impl Set for SkipSet<u64> {
    fn insert(&self, k: u64) {
        SkipSet::insert(self, k);
    }
    fn contains(&self, k: u64) -> bool {
        self.get(&k).is_some()
    }
}

impl Set for ConcurrentSet {
    fn insert(&self, k: u64) {
        ConcurrentSet::insert(self, k);
    }
    fn contains(&self, k: u64) -> bool {
        ConcurrentSet::contains(self, k)
    }
}

fn run(set: Arc<dyn Set>, threads: usize) -> f64 {
    for k in 0..PRELOAD {
        set.insert(k * 2);
    }
    let barrier = Arc::new(Barrier::new(threads + 1));
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let set = set.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut rng = StdRng::seed_from_u64(t as u64);
                barrier.wait();
                for _ in 0..OPS_PER_THREAD {
                    let k = rng.gen_range(0..KEYSPACE);
                    if rng.gen_range(0..100) < WRITE_PCT {
                        set.insert(k);
                    } else {
                        std::hint::black_box(set.contains(k));
                    }
                }
            })
        })
        .collect();
    barrier.wait();
    let start = Instant::now();
    for h in handles {
        h.join().unwrap();
    }
    (threads * OPS_PER_THREAD) as f64 / start.elapsed().as_secs_f64() / 1e6
}

fn main() {
    let thread_counts = [1usize, 2, 4, 8, 16];
    println!(
        "Mops/s total, {}% writes, keyspace {}\n",
        WRITE_PCT, KEYSPACE
    );
    println!(
        "{:<10} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "impl", "1t", "2t", "4t", "8t", "16t"
    );
    let contestants: Vec<(&str, Box<dyn Fn() -> Arc<dyn Set>>)> = vec![
        ("global", Box::new(|| Arc::new(Global(Mutex::default())))),
        ("sharded", Box::new(|| Arc::new(Sharded(Default::default())))),
        ("crossbeam", Box::new(|| Arc::new(SkipSet::new()))),
        ("mine", Box::new(|| Arc::new(ConcurrentSet::new()))),
    ];
    for (name, mk) in contestants {
        print!("{name:<10}");
        for &t in &thread_counts {
            print!(" {:>8.2}", run(mk(), t));
        }
        println!();
    }
    println!("\nRecord the table + line shapes in notes.md.");
}
