//! Txn throughput: your MVCC vs one big lock.
//!
//! Runs the global-lock baseline immediately; the MVCC half panics until
//! src/mvcc.rs is implemented. Then: cargo run --release --bin txn_bench
//!
//! Predict in notes.md BEFORE running:
//! - read-heavy (95/5): who wins, by how much? (MVCC readers never block;
//!   the mutex serializes even pure readers.)
//! - write-heavy (50/50) on a small hot keyspace: does first-committer-wins
//!   abort so much that the mutex wins? Where's the crossover keyspace size?

use mvcc_experiments::mvcc::{CommitError, Mode, Mvcc};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const THREADS: usize = 4;
const TXNS_PER_THREAD: usize = 50_000;
const OPS_PER_TXN: usize = 4;

#[derive(Clone, Copy)]
struct Mix {
    name: &'static str,
    write_pct: u32,
    keyspace: u64,
}

const MIXES: &[Mix] = &[
    Mix { name: "read-heavy  95/5, 10K keys", write_pct: 5, keyspace: 10_000 },
    Mix { name: "write-heavy 50/50, 10K keys", write_pct: 50, keyspace: 10_000 },
    Mix { name: "write-heavy 50/50, 64 keys (HOT)", write_pct: 50, keyspace: 64 },
];

fn key(n: u64) -> Vec<u8> {
    format!("key{n:08}").into_bytes()
}

fn run_global_lock(mix: Mix) -> f64 {
    let store: Arc<Mutex<HashMap<Vec<u8>, Vec<u8>>>> = Arc::default();
    let start = Instant::now();
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let store = store.clone();
            std::thread::spawn(move || {
                let mut rng = StdRng::seed_from_u64(t as u64);
                for _ in 0..TXNS_PER_THREAD {
                    // "transaction" = hold the one lock across all ops
                    let mut guard = store.lock().unwrap();
                    for _ in 0..OPS_PER_TXN {
                        let k = key(rng.gen_range(0..mix.keyspace));
                        if rng.gen_range(0..100) < mix.write_pct {
                            guard.insert(k, b"v".to_vec());
                        } else {
                            let _ = guard.get(&k);
                        }
                    }
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    (THREADS * TXNS_PER_THREAD) as f64 / start.elapsed().as_secs_f64()
}

fn run_mvcc(mix: Mix) -> (f64, u64) {
    let db = Mvcc::new();
    let aborts = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let db = db.clone();
            let aborts = aborts.clone();
            std::thread::spawn(move || {
                let mut rng = StdRng::seed_from_u64(t as u64);
                for _ in 0..TXNS_PER_THREAD {
                    loop {
                        let mut txn = db.begin(Mode::Snapshot);
                        let mut rng2 = rng.clone();
                        for _ in 0..OPS_PER_TXN {
                            let k = key(rng2.gen_range(0..mix.keyspace));
                            if rng2.gen_range(0..100) < mix.write_pct {
                                txn.put(&k, b"v");
                            } else {
                                let _ = txn.get(&k);
                            }
                        }
                        match txn.commit() {
                            Ok(()) => {
                                rng = rng2;
                                break;
                            }
                            Err(CommitError::WriteConflict | CommitError::ReadConflict) => {
                                aborts.fetch_add(1, Ordering::Relaxed);
                                // retry with the SAME ops (rng not advanced)
                            }
                        }
                    }
                }
                db.gc();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let tps = (THREADS * TXNS_PER_THREAD) as f64 / start.elapsed().as_secs_f64();
    (tps, aborts.load(Ordering::Relaxed))
}

fn main() {
    println!(
        "{} threads x {} txns x {} ops/txn\n",
        THREADS, TXNS_PER_THREAD, OPS_PER_TXN
    );
    println!("{:<36} {:>14} {:>14} {:>9}", "mix", "global-lock/s", "mvcc txn/s", "aborts");
    for &mix in MIXES {
        let lock_tps = run_global_lock(mix);
        let (mvcc_tps, aborts) = run_mvcc(mix);
        println!(
            "{:<36} {:>14.0} {:>14.0} {:>9}",
            mix.name, lock_tps, mvcc_tps, aborts
        );
    }
    println!("\nRecord all three rows + the abort counts in notes.md.");
}
