//! Memtable: sorted in-memory buffer, flushed at capacity.
//!
//! BTreeMap to start; swap in your topic-2 skiplist later and compare flush
//! (ordered-iteration) throughput — that was the skiplist's whole pitch.

use std::collections::BTreeMap;

pub const MEMTABLE_BYTES: usize = 1 << 20; // 1 MiB

#[derive(Default)]
pub struct Memtable {
    map: BTreeMap<Vec<u8>, Option<Vec<u8>>>, // None = tombstone
    bytes: usize,
}

impl Memtable {
    pub fn put(&mut self, key: &[u8], value: Option<&[u8]>) {
        self.bytes += key.len() + value.map_or(1, |v| v.len());
        self.map.insert(key.to_vec(), value.map(|v| v.to_vec()));
    }

    /// Some(None) = tombstone seen here; None = key unknown at this level.
    pub fn get(&self, key: &[u8]) -> Option<Option<&[u8]>> {
        self.map.get(key).map(|v| v.as_deref())
    }

    pub fn is_full(&self) -> bool {
        self.bytes >= MEMTABLE_BYTES
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u8], Option<&[u8]>)> {
        self.map.iter().map(|(k, v)| (k.as_slice(), v.as_deref()))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
