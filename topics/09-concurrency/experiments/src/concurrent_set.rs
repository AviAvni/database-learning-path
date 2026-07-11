//! A lock-free concurrent ordered set (skiplist) over crossbeam-epoch.
//! YOU implement this — it's topic 2's skiplist made concurrent.
//!
//! The contract (fixed by the tests):
//! - insert/contains/remove take &self — shared across threads freely.
//! - insert returns true iff the key was newly inserted (same-key race:
//!   exactly ONE thread wins).
//! - remove returns true iff THIS call removed it.
//! - removal must not free memory a concurrent reader can still see —
//!   unlink, then `guard.defer_destroy` (never Box::from_raw directly).
//!
//! Suggested plan (the lazy route — memgraph school with epochs instead
//! of accessor ids; see reading-concurrent-skiplists.md):
//! 1. Nodes: key + tower of `crossbeam_epoch::Atomic<Node>` next-pointers.
//! 2. contains: plain lock-free descent under a pin — validate nothing,
//!    skip marked nodes.
//! 3. insert: find preds/succs per level, CAS level 0 first (that's the
//!    linearization point — same-key winner decided HERE), then upper
//!    levels best-effort (RocksDB does the same; a missing upper link is
//!    only a performance bug).
//! 4. remove: mark the node (CAS a tag bit on the level-0 next pointer —
//!    crossbeam `Shared::with_tag`, bit-smuggling once more), then unlink,
//!    then defer_destroy.
//!
//! Memory ordering: publish with Release, read with Acquire. Run the tests
//! on this ARM Mac — Relaxed mistakes that pass on x86 fail here.

use crossbeam_epoch as epoch;

pub struct ConcurrentSet {
    // YOU design this. epoch::Atomic<Node> head tower, max height, ...
    _pd: std::marker::PhantomData<epoch::Guard>,
}

// The tests require these; implement them for your node design honestly —
// `unsafe impl` is only sound if your Drop/reclamation logic upholds it.
unsafe impl Send for ConcurrentSet {}
unsafe impl Sync for ConcurrentSet {}

impl ConcurrentSet {
    pub fn new() -> Self {
        todo!()
    }

    pub fn insert(&self, key: u64) -> bool {
        let _ = key;
        todo!()
    }

    pub fn contains(&self, key: u64) -> bool {
        let _ = key;
        todo!()
    }

    pub fn remove(&self, key: u64) -> bool {
        let _ = key;
        todo!()
    }

    pub fn len_slow(&self) -> usize {
        todo!()
    }
}

impl Default for ConcurrentSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn single_thread_semantics() {
        let s = ConcurrentSet::new();
        assert!(s.insert(5));
        assert!(!s.insert(5), "duplicate insert returns false");
        assert!(s.contains(5));
        assert!(!s.contains(6));
        assert!(s.remove(5));
        assert!(!s.remove(5), "double remove returns false");
        assert!(!s.contains(5));
        assert_eq!(s.len_slow(), 0);
    }

    #[test]
    fn disjoint_concurrent_inserts_all_land() {
        let s = Arc::new(ConcurrentSet::new());
        let handles: Vec<_> = (0..8u64)
            .map(|t| {
                let s = s.clone();
                thread::spawn(move || {
                    for i in 0..1000u64 {
                        assert!(s.insert(t * 1000 + i));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(s.len_slow(), 8000);
        for k in 0..8000u64 {
            assert!(s.contains(k), "missing {k}");
        }
    }

    #[test]
    fn same_key_race_has_exactly_one_winner() {
        let s = Arc::new(ConcurrentSet::new());
        for round in 0..200u64 {
            let wins = Arc::new(AtomicUsize::new(0));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let s = s.clone();
                    let wins = wins.clone();
                    thread::spawn(move || {
                        if s.insert(round) {
                            wins.fetch_add(1, Ordering::Relaxed);
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
            assert_eq!(wins.load(Ordering::Relaxed), 1, "round {round}");
        }
    }

    #[test]
    fn remove_race_has_exactly_one_winner() {
        let s = Arc::new(ConcurrentSet::new());
        for round in 0..200u64 {
            s.insert(round);
            let wins = Arc::new(AtomicUsize::new(0));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let s = s.clone();
                    let wins = wins.clone();
                    thread::spawn(move || {
                        if s.remove(round) {
                            wins.fetch_add(1, Ordering::Relaxed);
                        }
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }
            assert_eq!(wins.load(Ordering::Relaxed), 1, "round {round}");
        }
    }

    #[test]
    fn readers_survive_concurrent_removal_churn() {
        // The reclamation test: readers walk while writers insert+remove
        // the same keys. A use-after-free crashes or trips ASAN/miri;
        // passing here + a clean `cargo miri test` run is the bar.
        let s = Arc::new(ConcurrentSet::new());
        for k in 0..512u64 {
            s.insert(k);
        }
        let writers: Vec<_> = (0..2)
            .map(|_| {
                let s = s.clone();
                thread::spawn(move || {
                    for round in 0..300u64 {
                        for k in 0..512u64 {
                            s.remove(k);
                            s.insert(k + 512 + round); // churn fresh nodes too
                            s.insert(k);
                        }
                    }
                })
            })
            .collect();
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let s = s.clone();
                thread::spawn(move || {
                    for _ in 0..300 {
                        for k in 0..1024u64 {
                            let _ = s.contains(k); // must never touch freed memory
                        }
                    }
                })
            })
            .collect();
        for h in writers.into_iter().chain(readers) {
            h.join().unwrap();
        }
    }
}
