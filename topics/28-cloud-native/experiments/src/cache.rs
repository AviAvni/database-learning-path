//! STUB 1 — LRU block cache + read-through tiered reader.
//!
//! This is the "local NVMe cache over S3" tier every disaggregated engine
//! ships: slatedb's CachedObjectStore (cached_object_store/object_store.rs:34
//! caches fixed `part_size_bytes` parts; db_cache/ has pluggable moka/foyer
//! block caches), quickwit's split_cache + byte_range_cache, Neon's
//! pageserver page_cache.rs. The economics: S3 GETs cost money *per request*
//! and ~15 ms per read; a local hit costs ~2 µs and nothing.
//!
//! Implement LRU *semantics*; the structure is your choice. The simplest
//! thing that passes: HashMap<block, (last_used_tick, data)> + an O(n) scan
//! for the eviction victim. That's fine here (cache is thousands of entries).
//! Production uses an intrusive linked list (quickwit memory_sized_cache) or
//! clock/S3-FIFO — note why after measuring.

use crate::sim::{block_of, lookup_in_block, BlockStore, LatencyModel};
use std::collections::HashMap;

pub struct LruBlockCache {
    pub capacity: usize,
    pub hits: u64,
    pub misses: u64,
    tick: u64,
    map: HashMap<u64, (u64, Vec<u8>)>,
}

impl LruBlockCache {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self { capacity, hits: 0, misses: 0, tick: 0, map: HashMap::new() }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Return the cached block and mark it most-recently-used.
    /// Counts a hit or a miss.
    pub fn get(&mut self, _block: u64) -> Option<Vec<u8>> {
        // Recipe: bump self.tick; on hit update the entry's tick and clone the
        // data; count hits/misses.
        todo!("stub: LRU get")
    }

    /// Insert a block, evicting the least-recently-used entry if full.
    pub fn insert(&mut self, _block: u64, _data: Vec<u8>) {
        // Recipe: bump tick, insert; if len > capacity, remove the entry with
        // the smallest tick (O(n) scan is acceptable here).
        todo!("stub: LRU insert + evict")
    }
}

/// Read-through tiered reader: cache in front of a remote BlockStore.
pub struct TieredReader<L: LatencyModel> {
    pub remote: BlockStore<L>,
    pub cache: LruBlockCache,
    /// Simulated cost of serving from the local cache (memory/NVMe hit).
    pub cache_hit_micros: u64,
}

impl<L: LatencyModel> TieredReader<L> {
    pub fn new(remote: BlockStore<L>, cache_blocks: usize) -> Self {
        Self { remote, cache: LruBlockCache::new(cache_blocks), cache_hit_micros: 2 }
    }

    /// Point lookup of `key`: returns (value, simulated_micros).
    pub fn read(&mut self, _key: u64) -> ([u8; 16], u64) {
        // Recipe: block = block_of(key). Cache hit -> cost = cache_hit_micros.
        // Miss -> remote.get(block) (cost = remote latency + cache_hit_micros
        // is fine to ignore; just charge the remote micros), insert into
        // cache. Either way lookup_in_block for the value (keys are always
        // present in this workload — unwrap is fine).
        let _ = (block_of(0), lookup_in_block(&[], 0)); // types in scope for the recipe
        todo!("stub: tiered read-through")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::{value_for, Zipf, S3, ENTRIES_PER_BLOCK};

    #[test]
    fn lru_semantics_touch_protects() {
        let mut c = LruBlockCache::new(2);
        c.insert(1, vec![1]);
        c.insert(2, vec![2]);
        assert!(c.get(1).is_some()); // touch 1 -> 2 is now LRU
        c.insert(3, vec![3]);
        assert!(c.get(1).is_some(), "recently-touched entry evicted");
        assert!(c.get(2).is_none(), "LRU entry survived eviction");
        assert!(c.get(3).is_some());
    }

    #[test]
    fn capacity_respected() {
        let mut c = LruBlockCache::new(10);
        for b in 0..100 {
            c.insert(b, vec![b as u8]);
        }
        assert_eq!(c.len(), 10);
    }

    #[test]
    fn hit_miss_accounting() {
        let mut c = LruBlockCache::new(4);
        assert!(c.get(7).is_none());
        c.insert(7, vec![7]);
        assert!(c.get(7).is_some());
        assert_eq!((c.hits, c.misses), (1, 1));
    }

    #[test]
    fn tiered_read_correct_and_caches() {
        let mut t = TieredReader::new(BlockStore::new(S3::new(1)), 64);
        let key = 5 * ENTRIES_PER_BLOCK + 17;
        let (v1, cost1) = t.read(key);
        assert_eq!(v1, value_for(key));
        assert!(cost1 > 1_000, "first read must pay the remote trip");
        let (v2, cost2) = t.read(key);
        assert_eq!(v2, value_for(key));
        assert_eq!(cost2, t.cache_hit_micros, "second read must be a cache hit");
        assert_eq!(t.remote.gets, 1, "no second remote GET for a cached block");
        // a different key in the SAME block is also a hit
        let (_, cost3) = t.read(key + 1);
        assert_eq!(cost3, t.cache_hit_micros);
    }

    #[test]
    fn zipfian_hit_rate_beats_half() {
        // 2000 blocks of keys, cache 250 blocks (1/8), zipf-skewed keys.
        let n_keys = 2_000 * ENTRIES_PER_BLOCK;
        let mut t = TieredReader::new(BlockStore::new(S3::new(2)), 250);
        let mut z = Zipf::new(n_keys as usize, 0.99, 3);
        for _ in 0..30_000 {
            t.read(z.sample());
        }
        let hit_rate = t.cache.hits as f64 / (t.cache.hits + t.cache.misses) as f64;
        assert!(hit_rate > 0.5, "zipfian hit rate {hit_rate:.2} <= 0.5");
    }
}
