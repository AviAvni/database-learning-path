//! Provided: a WAL-backed KV store over `SimFile`, with four
//! INJECTABLE BUGS. Your dst.rs harness must catch all four and
//! pass a clean run. Each bug is a real one from topic 5's crash
//! matrix or a famous postmortem shape.
//!
//! WAL format: 10-byte records `[type][key u32][val u32][xor cksum]`
//! type 1 = put, 2 = delete, 3 = commit marker. A batch counts only
//! if its commit marker is durable and every record's checksum
//! verifies; a torn tail invalidates the incomplete batch.

use crate::sim_fs::SimFile;
use crate::Op;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bug {
    None,
    /// deletes skip the WAL — deleted keys resurrect after crash
    LostDelete,
    /// commit writes the marker but never fsyncs — acked batches
    /// vanish on crash (valkey-default behavior, sold as a bug here)
    NoSyncOnCommit,
    /// recovery applies batches without requiring the commit marker
    /// — torn/uncommitted prefixes leak into the state
    TornWriteAccepted,
    /// recovery keeps the pre-crash in-memory map instead of
    /// replaying the WAL — reads serve unsynced state
    StaleRead,
}

pub struct KvStore {
    pub bug: Bug,
    wal: SimFile,
    mem: BTreeMap<u32, u32>,
}

fn record(ty: u8, k: u32, v: u32) -> [u8; 10] {
    let mut r = [0u8; 10];
    r[0] = ty;
    r[1..5].copy_from_slice(&k.to_le_bytes());
    r[5..9].copy_from_slice(&v.to_le_bytes());
    r[9] = r[..9].iter().fold(0, |a, b| a ^ b) ^ 0xA5;
    r
}

impl KvStore {
    pub fn new(seed: u64, bug: Bug) -> KvStore {
        KvStore { bug, wal: SimFile::new(seed), mem: BTreeMap::new() }
    }

    pub fn apply(&mut self, op: &Op) {
        match *op {
            Op::Put(k, v) => {
                self.wal.append(&record(1, k, v));
                self.mem.insert(k, v);
            }
            Op::Delete(k) => {
                if self.bug != Bug::LostDelete {
                    self.wal.append(&record(2, k, 0));
                }
                self.mem.remove(&k);
            }
            Op::Commit => {
                self.wal.append(&record(3, 0, 0));
                if self.bug != Bug::NoSyncOnCommit {
                    self.wal.sync();
                }
            }
            Op::Crash => {
                self.wal.crash();
                self.recover();
            }
        }
    }

    fn recover(&mut self) {
        if self.bug == Bug::StaleRead {
            return; // "the cache is probably fine"
        }
        self.mem.clear();
        let mut batch: Vec<(u8, u32, u32)> = Vec::new();
        let mut last_good = 0usize;
        for (i, chunk) in self.wal.durable().to_vec().chunks(10).enumerate() {
            if chunk.len() < 10 || chunk[9] != chunk[..9].iter().fold(0, |a, b| a ^ b) ^ 0xA5 {
                break; // torn tail — stop replay
            }
            let k = u32::from_le_bytes(chunk[1..5].try_into().unwrap());
            let v = u32::from_le_bytes(chunk[5..9].try_into().unwrap());
            match chunk[0] {
                3 => {
                    for &(ty, k, v) in &batch {
                        match ty {
                            1 => {
                                self.mem.insert(k, v);
                            }
                            _ => {
                                self.mem.remove(&k);
                            }
                        }
                    }
                    batch.clear();
                    last_good = (i + 1) * 10;
                }
                ty => batch.push((ty, k, v)),
            }
        }
        // tail repair: drop torn/uncommitted bytes so leftovers can't
        // join the next batch (skipping this was itself a bug we
        // caught with crash_matrix — see notes.md)
        self.wal.truncate(last_good);
        if self.bug == Bug::TornWriteAccepted {
            // apply the incomplete batch anyway
            for (ty, k, v) in batch {
                match ty {
                    1 => {
                        self.mem.insert(k, v);
                    }
                    _ => {
                        self.mem.remove(&k);
                    }
                }
            }
        }
    }

    pub fn state(&self) -> &BTreeMap<u32, u32> {
        &self.mem
    }

    pub fn wal_stats(&self) -> (u64, u64) {
        (self.wal.syncs, self.wal.crashes)
    }
}
