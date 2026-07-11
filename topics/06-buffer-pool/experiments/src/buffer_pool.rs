//! A CLOCK buffer pool over a page file. YOU implement this.
//!
//! Design (fixed by the tests):
//! - Fixed array of `capacity` frames, each PAGE_SIZE bytes.
//! - A map page_no → frame index (postgres: partitioned hash; here a plain
//!   HashMap is fine — you're single-threaded until topic 9).
//! - Per frame: page_no, pin_count, usage_count (saturate at MAX_USAGE = 5),
//!   dirty flag.
//! - `with_page` / `with_page_mut` pin the page for the duration of the
//!   closure (loading it on a miss), then unpin. `with_page_mut` marks the
//!   frame dirty. `pin`/`unpin` are the long-lived variants (a real system
//!   uses RAII guards — topic 9 revisits this with concurrency).
//! - Victim search: CLOCK — advance a hand over the frames; pinned ⇒ skip,
//!   usage_count > 0 ⇒ decrement and continue, else evict (write back if
//!   dirty FIRST, then reuse). All frames pinned ⇒ Err(PoolError::AllPinned).
//! - Pages beyond EOF read as zeroes (that's how allocation works here).
//! - `flush_all` writes every dirty frame.
//!
//! What to notice while building it (notes.md):
//! - where the WAL rule would hook in (before which write, with which LSN?),
//! - what a background writer would take off the miss path,
//! - how a scan of N ≫ capacity pages wrecks usage counts (then reread the
//!   postgres buffer-ring section).

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::Path;

pub const PAGE_SIZE: usize = 4096;
pub const MAX_USAGE: u8 = 5;

#[derive(Debug)]
pub enum PoolError {
    AllPinned,
    Io(io::Error),
}

impl From<io::Error> for PoolError {
    fn from(e: io::Error) -> Self {
        PoolError::Io(e)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PoolStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub dirty_writebacks: u64,
}

pub struct BufferPool {
    file: File,
    capacity: usize,
    map: HashMap<u64, usize>,
    // your frame array + clock hand + stats
}

impl BufferPool {
    /// `path` must exist (may be empty). Pool holds `capacity` frames.
    pub fn open(path: &Path, capacity: usize) -> Result<BufferPool, PoolError> {
        let (_, _) = (path, capacity);
        todo!()
    }

    /// Pin `page_no`, run `f` on its bytes, unpin.
    pub fn with_page<R>(
        &mut self,
        page_no: u64,
        f: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, PoolError> {
        let (_, _) = (page_no, &f);
        let _ = (&self.file, self.capacity, &self.map);
        todo!()
    }

    /// Pin `page_no`, run `f` on its bytes mutably, mark dirty, unpin.
    pub fn with_page_mut<R>(
        &mut self,
        page_no: u64,
        f: impl FnOnce(&mut [u8]) -> R,
    ) -> Result<R, PoolError> {
        let (_, _) = (page_no, &f);
        todo!()
    }

    /// Long-lived pin: page stays resident until `unpin`. Counts nest.
    pub fn pin(&mut self, page_no: u64) -> Result<(), PoolError> {
        let _ = page_no;
        todo!()
    }

    /// Release one pin taken with `pin`. Panics if not pinned (a bug).
    pub fn unpin(&mut self, page_no: u64) {
        let _ = page_no;
        todo!()
    }

    /// Write back every dirty frame. Does not evict.
    pub fn flush_all(&mut self) -> Result<(), PoolError> {
        todo!()
    }

    pub fn stats(&self) -> PoolStats {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(capacity: usize) -> (tempfile::TempDir, BufferPool) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pages");
        File::create(&path).unwrap();
        let pool = BufferPool::open(&path, capacity).unwrap();
        (dir, pool)
    }

    #[test]
    fn write_then_read_roundtrip() {
        let (_dir, mut pool) = pool(4);
        pool.with_page_mut(0, |p| {
            p[0] = 0xAB;
            p[PAGE_SIZE - 1] = 0xCD;
        })
        .unwrap();
        let (a, b) = pool.with_page(0, |p| (p[0], p[PAGE_SIZE - 1])).unwrap();
        assert_eq!((a, b), (0xAB, 0xCD));
    }

    #[test]
    fn eviction_writes_dirty_pages_back() {
        let (_dir, mut pool) = pool(2);
        for p in 0..10u64 {
            pool.with_page_mut(p, |bytes| bytes[0] = p as u8).unwrap();
        } // capacity 2 ⇒ most pages evicted along the way
        for p in 0..10u64 {
            let v = pool.with_page(p, |bytes| bytes[0]).unwrap();
            assert_eq!(v, p as u8, "page {p} lost its write during eviction");
        }
        assert!(pool.stats().dirty_writebacks >= 8);
    }

    #[test]
    fn pinned_pages_are_never_evicted() {
        let (_dir, mut pool) = pool(2);
        pool.pin(0).unwrap();
        pool.pin(1).unwrap();
        // both frames pinned ⇒ nothing evictable
        match pool.with_page(2, |_| ()) {
            Err(PoolError::AllPinned) => {}
            other => panic!("expected AllPinned, got {other:?}"),
        }
        pool.unpin(1);
        // now page 2 can displace page 1, and page 0 must still be resident
        pool.with_page(2, |_| ()).unwrap();
        let misses = pool.stats().misses;
        pool.with_page(0, |_| ()).unwrap();
        assert_eq!(pool.stats().misses, misses, "pinned page 0 was evicted");
        pool.unpin(0);
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pages");
        File::create(&path).unwrap();
        {
            let mut pool = BufferPool::open(&path, 4).unwrap();
            for p in 0..8u64 {
                pool.with_page_mut(p, |b| b[7] = (p * 3) as u8).unwrap();
            }
            pool.flush_all().unwrap();
        }
        let mut pool = BufferPool::open(&path, 4).unwrap();
        for p in 0..8u64 {
            let v = pool.with_page(p, |b| b[7]).unwrap();
            assert_eq!(v, (p * 3) as u8);
        }
    }

    #[test]
    fn hot_page_survives_scan_pressure() {
        let (_dir, mut pool) = pool(8);
        pool.with_page_mut(0, |b| b[0] = 99).unwrap();
        // saturate page 0's usage count
        for _ in 0..10 {
            pool.with_page(0, |_| ()).unwrap();
        }
        let misses_before = pool.stats().misses;
        // scan 6 cold pages (fits alongside page 0 in 8 frames)
        for p in 100..106u64 {
            pool.with_page(p, |_| ()).unwrap();
        }
        pool.with_page(0, |_| ()).unwrap();
        assert_eq!(
            pool.stats().misses,
            misses_before + 6,
            "page 0 should have survived the scan (CLOCK second chance)"
        );
    }
}
