//! SST writer/reader — YOUR implementation (with the bloom provided).
//!
//! Format (single forward pass, like lsm-tree/RocksDB):
//!
//! ```text
//! [data block]…[data block][bloom block][index block][footer]
//!
//! data block (~4KB, LZ4-compressed, xxh3 checksum in a block header):
//!   restart point every 16 entries; between restarts entries are
//!   prefix-truncated: shared_len u16 ∥ rest_len u16 ∥ vlen u32 ∥ rest ∥ value
//!   (vlen == u32::MAX = tombstone)
//!   trailer: restart offsets u32 × n ∥ n u32
//!
//! index block: (last_key ∥ block_offset u64 ∥ block_len u32) per data block
//! footer: index_offset u64 ∥ index_len u32 ∥ bloom_offset u64 ∥ bloom_len u32
//! ```
//!
//! Reader: binary search index → read one block → binary search restarts →
//! linear decode. `bytes_written` feeds the write-amp experiment — count
//! EVERYTHING you write.

use crate::bloom::Bloom;
use std::path::Path;

pub const BLOCK_SIZE: usize = 4096;
pub const RESTART_INTERVAL: usize = 16;

pub struct SstWriter {
    // TODO: file, current block buf, restart offsets, index entries, bloom, ...
}

impl SstWriter {
    pub fn create(path: &Path, n_keys_hint: usize, bloom_bits_per_key: f64) -> std::io::Result<Self> {
        let _ = (path, n_keys_hint, bloom_bits_per_key);
        todo!()
    }

    /// Keys MUST arrive in sorted order (the flush/merge guarantees it).
    pub fn add(&mut self, key: &[u8], value: Option<&[u8]>) -> std::io::Result<()> {
        let _ = (key, value);
        todo!("append to block buf with prefix truncation; flush block at BLOCK_SIZE")
    }

    /// Returns total bytes written (for write-amp accounting).
    pub fn finish(self) -> std::io::Result<u64> {
        todo!("flush last block, write bloom + index + footer")
    }
}

pub struct SstReader {
    pub bloom: Bloom,
    // TODO: file/mmap, index entries, min/max key
}

impl SstReader {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let _ = path;
        todo!("read footer → index → bloom; keep index + bloom in memory")
    }

    pub fn min_key(&self) -> &[u8] {
        todo!()
    }

    pub fn max_key(&self) -> &[u8] {
        todo!()
    }

    /// Some(None) = tombstone. Consult the bloom FIRST (count hits/misses in
    /// Stats for the read-amp experiment).
    pub fn get(&mut self, key: &[u8]) -> std::io::Result<Option<Option<Vec<u8>>>> {
        let _ = key;
        todo!()
    }

    /// Full ordered iteration — used by compaction merges.
    pub fn iter(&mut self) -> std::io::Result<Vec<(Vec<u8>, Option<Vec<u8>>)>> {
        todo!("fine to materialize; streaming iterator is extra credit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_with_tombstones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.sst");
        let mut w = SstWriter::create(&path, 10_000, 10.0).unwrap();
        for i in 0..10_000u64 {
            let k = format!("key{i:08}");
            if i % 7 == 0 {
                w.add(k.as_bytes(), None).unwrap();
            } else {
                w.add(k.as_bytes(), Some(&i.to_le_bytes())).unwrap();
            }
        }
        let written = w.finish().unwrap();
        assert!(written > 0);

        let mut r = SstReader::open(&path).unwrap();
        assert_eq!(
            r.get(b"key00000001").unwrap(),
            Some(Some(1u64.to_le_bytes().to_vec()))
        );
        assert_eq!(r.get(b"key00000000").unwrap(), Some(None), "tombstone");
        assert_eq!(r.get(b"nope").unwrap(), None);
        assert_eq!(r.iter().unwrap().len(), 10_000);
    }

    #[test]
    fn prefix_truncation_saves_space() {
        // shared 24-byte prefixes: truncated blocks must be much smaller
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.sst");
        let mut w = SstWriter::create(&path, 10_000, 10.0).unwrap();
        for i in 0..10_000u64 {
            let k = format!("shared/long/prefix/here/{i:08}");
            w.add(k.as_bytes(), Some(b"v")).unwrap();
        }
        let written = w.finish().unwrap();
        let raw: u64 = 10_000 * (32 + 1);
        assert!(
            written < raw,
            "truncation+compression should beat raw {raw}, got {written}"
        );
    }
}
