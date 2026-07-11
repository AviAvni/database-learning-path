//! Disk B+tree on a page file — YOUR implementation.
//!
//! Simplifications vs SQLite (deliberate — this is a lookup/scan study, not a
//! durability study; WAL arrives in topic 5):
//! - B+tree: values ONLY in leaves; leaves carry a right-sibling page number
//!   for range scans (SQLite doesn't have this — interior re-descent instead).
//! - No delete-rebalance (delete within leaf only), no overflow pages
//!   (cap value size), no freelist. Grow-only file.
//! - No cache: read_page/write_page hit the file every time. The OS page cache
//!   will make this fast anyway — NOTE THIS in your bench writeup (topic 6
//!   builds the real buffer pool).
//!
//! File layout: page 0 = header (root page no, page count); pages 1.. = nodes.

use crate::page::{Page, PAGE_SIZE};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub struct DiskBTree {
    file: File,
    root: u32,
    npages: u32,
}

impl DiskBTree {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        let _ = path;
        todo!("write header page + empty leaf root")
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        let _ = path;
        todo!()
    }

    fn read_page(&mut self, no: u32) -> std::io::Result<Page> {
        let mut buf = [0u8; PAGE_SIZE];
        self.file.seek(SeekFrom::Start(no as u64 * PAGE_SIZE as u64))?;
        self.file.read_exact(&mut buf)?;
        Ok(Page { buf })
    }

    fn write_page(&mut self, no: u32, p: &Page) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(no as u64 * PAGE_SIZE as u64))?;
        self.file.write_all(&p.buf)
    }

    fn alloc_page(&mut self) -> u32 {
        let n = self.npages;
        self.npages += 1;
        n
    }

    pub fn get(&mut self, key: &[u8]) -> std::io::Result<Option<Vec<u8>>> {
        let _ = key;
        todo!("descend interior pages via child_for, then Page::get on the leaf")
    }

    pub fn insert(&mut self, key: &[u8], val: &[u8]) -> std::io::Result<()> {
        let _ = (key, val);
        todo!("descend recording the path; on leaf full: split_into, push separator up; root split grows the tree UP (new root)")
    }

    /// Range scan [start, end): walk leaf sibling links.
    pub fn scan(&mut self, start: &[u8], end: &[u8]) -> std::io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let _ = (start, end);
        todo!()
    }

    pub fn height(&mut self) -> std::io::Result<u32> {
        todo!("descend leftmost — report this in notes.md for the fanout arithmetic")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::prelude::*;

    #[test]
    fn insert_get_10k_random() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = DiskBTree::create(&dir.path().join("t.db")).unwrap();
        let mut keys: Vec<u32> = (0..10_000).collect();
        keys.shuffle(&mut StdRng::seed_from_u64(3));
        for k in &keys {
            t.insert(&k.to_be_bytes(), &k.to_le_bytes()).unwrap();
        }
        for k in &keys {
            assert_eq!(t.get(&k.to_be_bytes()).unwrap(), Some(k.to_le_bytes().to_vec()));
        }
        assert_eq!(t.get(&99_999u32.to_be_bytes()).unwrap(), None);
    }

    #[test]
    fn scan_is_sorted_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = DiskBTree::create(&dir.path().join("t.db")).unwrap();
        for k in (0..10_000u32).rev() {
            t.insert(&k.to_be_bytes(), b"v").unwrap();
        }
        let out = t.scan(&100u32.to_be_bytes(), &200u32.to_be_bytes()).unwrap();
        assert_eq!(out.len(), 100);
        assert!(out.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn reopen_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let mut t = DiskBTree::create(&path).unwrap();
            t.insert(b"hello", b"world").unwrap();
        }
        let mut t = DiskBTree::open(&path).unwrap();
        assert_eq!(t.get(b"hello").unwrap(), Some(b"world".to_vec()));
    }
}
