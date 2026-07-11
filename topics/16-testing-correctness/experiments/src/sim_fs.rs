//! Provided: a simulated file — the turso runner/file.rs idea in 60
//! lines. Writes are BUFFERED until sync; crash drops unsynced bytes
//! and may TEAR the tail (a partial record survives). All
//! nondeterminism comes from the caller's seed.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub struct SimFile {
    /// durable bytes — survive any crash
    synced: Vec<u8>,
    /// buffered bytes — lost (or torn) on crash
    buffered: Vec<u8>,
    rng: StdRng,
    pub syncs: u64,
    pub crashes: u64,
}

impl SimFile {
    pub fn new(seed: u64) -> SimFile {
        SimFile {
            synced: Vec::new(),
            buffered: Vec::new(),
            rng: StdRng::seed_from_u64(seed),
            syncs: 0,
            crashes: 0,
        }
    }

    pub fn append(&mut self, bytes: &[u8]) {
        self.buffered.extend_from_slice(bytes);
    }

    pub fn sync(&mut self) {
        self.synced.extend_from_slice(&self.buffered);
        self.buffered.clear();
        self.syncs += 1;
    }

    /// kill -9. Unsynced bytes vanish — except that with probability
    /// 1/2 a random PREFIX of the buffer made it to disk (the torn
    /// tail every recovery path must tolerate — topic 5).
    pub fn crash(&mut self) {
        self.crashes += 1;
        if !self.buffered.is_empty() && self.rng.gen_bool(0.5) {
            let torn = self.rng.gen_range(1..=self.buffered.len());
            self.synced.extend_from_slice(&self.buffered[..torn]);
        }
        self.buffered.clear();
    }

    /// What recovery sees after a crash (or clean read of all data).
    pub fn durable(&self) -> &[u8] {
        &self.synced
    }

    /// Recovery's tail repair: drop torn/uncommitted durable bytes
    /// past the last valid point (a real WAL truncates here too —
    /// otherwise leftovers join the NEXT batch).
    pub fn truncate(&mut self, len: usize) {
        self.synced.truncate(len);
    }
}
