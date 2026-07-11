//! PROVIDED infrastructure — a sharded, MVCC, Percolator-shaped KV cluster.
//!
//! Everything here is deterministic and in-process: "shards" are structs,
//! "the network" is a method call, "crashing" is simply not calling the next
//! step (the M16 DST style). The three Percolator column families are
//! modeled exactly (Bigtable's data/lock/write columns; TiKV keeps the same
//! trio as CF_DEFAULT / CF_LOCK / CF_WRITE):
//!
//!   data  : (key, start_ts)  -> value         the actual bytes
//!   lock  : key              -> LockInfo      uncommitted intent
//!   write : (key, commit_ts) -> start_ts      the commit record / index
//!
//! A committed read at ts is: newest write with commit_ts <= ts, then fetch
//! data at that record's start_ts. The protocol logic (prewrite / commit /
//! resolve, 2PC) lives in the stubs — this file only stores state.

use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use std::collections::{BTreeMap, HashMap};

pub type Key = u64;
pub type Ts = u64;
pub type TxnId = u64;

/// Timestamp oracle — Percolator/TiKV's TSO. Strictly monotonic.
#[derive(Default)]
pub struct Tso {
    next: Ts,
}

impl Tso {
    pub fn get_ts(&mut self) -> Ts {
        self.next += 1;
        self.next
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockInfo {
    /// Primary key of the transaction holding this lock — the linearization
    /// point for the whole transaction's fate.
    pub primary: Key,
    pub start_ts: Ts,
}

#[derive(Default)]
pub struct Shard {
    pub data: HashMap<(Key, Ts), i64>,
    pub locks: HashMap<Key, LockInfo>,
    pub writes: BTreeMap<(Key, Ts), Ts>, // (key, commit_ts) -> start_ts
}

impl Shard {
    /// Newest committed write on `key` with commit_ts <= ts.
    pub fn latest_write_before(&self, key: Key, ts: Ts) -> Option<(Ts, Ts)> {
        self.writes
            .range((key, 0)..=(key, ts))
            .next_back()
            .map(|(&(_, commit_ts), &start_ts)| (commit_ts, start_ts))
    }

    /// Any committed write on `key` at commit_ts > ts? (prewrite's WW check)
    pub fn newer_write_exists(&self, key: Key, ts: Ts) -> bool {
        self.writes
            .range((key, ts + 1)..=(key, Ts::MAX))
            .next()
            .is_some()
    }
}

pub struct Cluster {
    pub shards: Vec<Shard>,
    pub tso: Tso,
}

impl Cluster {
    pub fn new(n_shards: usize) -> Self {
        Self {
            shards: (0..n_shards).map(|_| Shard::default()).collect(),
            tso: Tso::default(),
        }
    }

    pub fn shard_of(&self, key: Key) -> usize {
        (key % self.shards.len() as u64) as usize
    }

    pub fn shard(&self, key: Key) -> &Shard {
        &self.shards[self.shard_of(key)]
    }

    pub fn shard_mut(&mut self, key: Key) -> &mut Shard {
        let i = self.shard_of(key);
        &mut self.shards[i]
    }

    /// Committed-only read (ignores locks — the protocol layer must check
    /// locks itself; that's the point of the percolator stub).
    pub fn read_committed(&self, key: Key, ts: Ts) -> Option<i64> {
        let shard = self.shard(key);
        let (_, start_ts) = shard.latest_write_before(key, ts)?;
        shard.data.get(&(key, start_ts)).copied()
    }

    /// Total committed balance across all keys as of `ts` — the bank
    /// invariant oracle.
    pub fn total_committed(&self, keys: &[Key], ts: Ts) -> i64 {
        keys.iter()
            .map(|&k| self.read_committed(k, ts).unwrap_or(0))
            .sum()
    }

    pub fn lock_count(&self) -> usize {
        self.shards.iter().map(|s| s.locks.len()).sum()
    }
}

/// Zipfian key sampler (CDF + binary search) — the contention dial.
pub struct Zipf {
    cdf: Vec<f64>,
    rng: ChaCha8Rng,
}

impl Zipf {
    pub fn new(n: usize, theta: f64, seed: u64) -> Self {
        let mut cdf = Vec::with_capacity(n);
        let mut acc = 0.0f64;
        for i in 0..n {
            acc += 1.0 / ((i + 1) as f64).powf(theta);
            cdf.push(acc);
        }
        for v in cdf.iter_mut() {
            *v /= acc;
        }
        Self { cdf, rng: ChaCha8Rng::seed_from_u64(seed) }
    }

    pub fn sample(&mut self) -> u64 {
        let u: f64 = self.rng.gen();
        self.cdf.partition_point(|&c| c < u) as u64
    }

    /// A transfer pair (from != to).
    pub fn transfer_pair(&mut self) -> (Key, Key) {
        let a = self.sample();
        loop {
            let b = self.sample();
            if b != a {
                return (a, b);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_roundtrip() {
        let mut c = Cluster::new(2);
        // manually commit key 7: value 42, start_ts 10, commit_ts 12
        c.shard_mut(7).data.insert((7, 10), 42);
        c.shard_mut(7).writes.insert((7, 12), 10);
        assert_eq!(c.read_committed(7, 11), None, "not visible before commit_ts");
        assert_eq!(c.read_committed(7, 12), Some(42));
        assert_eq!(c.read_committed(7, 99), Some(42));
    }

    #[test]
    fn latest_write_picks_newest_at_or_below() {
        let mut s = Shard::default();
        s.writes.insert((5, 10), 8);
        s.writes.insert((5, 20), 18);
        s.writes.insert((6, 15), 13); // other key must not interfere
        assert_eq!(s.latest_write_before(5, 9), None);
        assert_eq!(s.latest_write_before(5, 10), Some((10, 8)));
        assert_eq!(s.latest_write_before(5, 19), Some((10, 8)));
        assert_eq!(s.latest_write_before(5, 20), Some((20, 18)));
        assert!(s.newer_write_exists(5, 19));
        assert!(!s.newer_write_exists(5, 20));
    }

    #[test]
    fn tso_is_strictly_monotonic() {
        let mut tso = Tso::default();
        let a = tso.get_ts();
        let b = tso.get_ts();
        assert!(b > a);
    }

    #[test]
    fn zipf_contention_is_real() {
        let mut z = Zipf::new(10_000, 1.1, 7);
        let mut hot = 0;
        for _ in 0..10_000 {
            if z.sample() < 100 {
                hot += 1;
            }
        }
        assert!(hot > 5_000, "theta=1.1 should send most traffic to the top 1%");
    }
}
