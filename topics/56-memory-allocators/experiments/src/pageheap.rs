//! Lane 2 (STUB) — a single-threaded, page-sharded size-class allocator.
//!
//! Build the allocator mimalloc's §2 describes, at toy scale:
//!
//! * Requests ≤ `MAX_SMALL` are rounded to a class from [`crate::classes`].
//!   Larger requests go straight to the global allocator.
//! * Memory comes in `PAGE_SIZE` pages aligned to `PAGE_SIZE` (allocate them
//!   with `std::alloc` and a 64 KiB-aligned `Layout`). Each page holds blocks of
//!   exactly one class and starts with a `HEADER`-byte bookkeeping area: a
//!   free list, the block size, a used count.
//! * **Free-list sharding:** each page has its *own* free list, and an
//!   allocation pops from the current page of its class until that page is
//!   exhausted — wherever other blocks were freed. `free` finds the page with
//!   `p as usize & PAGE_MASK`, so it needs only the pointer.
//! * A page that becomes completely free is handed back to the global
//!   allocator, except that one page per class may be kept.
//!
//! Optional refinements the tests do not require: a separate `local_free` list
//! that is swapped into `free` only when `free` runs dry (mimalloc §2.3,
//! "temporal cadence"), and lazy extension of a fresh page instead of
//! threading its whole free list at creation (mimalloc's bump-vs-list
//! trade-off, §2.2).

use crate::heap::Heap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageStats {
    /// Pages currently owned (not counting large-object fallbacks).
    pub pages: usize,
    /// Small blocks currently handed out.
    pub live_blocks: usize,
}

pub struct PageHeap {
    _todo: (),
}

impl PageHeap {
    pub fn new() -> Self {
        todo!("lane 2: per-class page lists, current page, page headers")
    }
    pub fn stats(&self) -> PageStats {
        todo!("lane 2: count owned pages and live blocks")
    }
}

impl Default for PageHeap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap for PageHeap {
    fn name(&self) -> String {
        "pageheap".into()
    }
    unsafe fn alloc(&mut self, _size: usize) -> *mut u8 {
        todo!("lane 2: pop from the current page's free list; slow path finds or creates a page")
    }
    unsafe fn free(&mut self, _p: *mut u8, _size: usize) {
        todo!("lane 2: locate the page by masking, push on its free list, retire it if empty")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classes::{MAX_SMALL, PAGE_MASK};
    use rand::seq::SliceRandom;
    use rand::{Rng, SeedableRng};
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashSet;

    fn page(p: *mut u8) -> usize {
        p as usize & PAGE_MASK
    }

    #[test]
    fn blocks_are_distinct_aligned_and_writable() {
        let mut h = PageHeap::new();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let mut v = Vec::new();
        let mut seen = HashSet::new();
        for i in 0..50_000usize {
            let size = rng.gen_range(1..=MAX_SMALL + 200);
            let p = unsafe { h.alloc(size) };
            assert_eq!(p as usize % 8, 0);
            assert!(seen.insert(p as usize), "duplicate block");
            unsafe { std::ptr::write_bytes(p, (i % 251) as u8, size) };
            v.push((p, size, (i % 251) as u8));
        }
        for &(p, size, b) in &v {
            assert!(unsafe { std::slice::from_raw_parts(p, size) }.iter().all(|&x| x == b));
        }
        for &(p, size, _) in &v {
            unsafe { h.free(p, size) };
        }
    }

    #[test]
    fn consecutive_small_allocations_share_a_page() {
        let mut h = PageHeap::new();
        let a = unsafe { h.alloc(48) };
        let b = unsafe { h.alloc(48) };
        assert_eq!(page(a), page(b));
        assert_eq!(h.stats().live_blocks, 2);
    }

    #[test]
    fn freed_blocks_are_reused_without_new_pages() {
        let mut h = PageHeap::new();
        let v: Vec<_> = (0..10_000).map(|_| unsafe { h.alloc(48) }).collect();
        let pages = h.stats().pages;
        for p in v.iter().step_by(2) {
            unsafe { h.free(*p, 48) };
        }
        for _ in 0..5_000 {
            unsafe { h.alloc(48) };
        }
        assert_eq!(h.stats().pages, pages, "5000 frees must satisfy 5000 allocs");
        assert_eq!(h.stats().live_blocks, 10_000);
    }

    #[test]
    fn allocation_stays_local_after_scattered_frees() {
        // The point of sharding: after frees scattered over ~15 pages, the next
        // 1000 allocations fill a page or two instead of landing everywhere.
        let mut h = PageHeap::new();
        let mut v: Vec<_> = (0..20_000).map(|_| unsafe { h.alloc(48) }).collect();
        let mut rng = ChaCha8Rng::seed_from_u64(9);
        v.shuffle(&mut rng);
        for p in v.drain(..10_000) {
            unsafe { h.free(p, 48) };
        }
        let touched: HashSet<usize> = (0..1000).map(|_| page(unsafe { h.alloc(48) })).collect();
        assert!(touched.len() <= 3, "1000 allocations touched {} pages", touched.len());
    }

    #[test]
    fn empty_pages_are_returned() {
        let mut h = PageHeap::new();
        let v: Vec<_> = (0..100_000).map(|_| unsafe { h.alloc(48) }).collect();
        assert!(h.stats().pages >= 70);
        for p in v {
            unsafe { h.free(p, 48) };
        }
        assert!(h.stats().pages <= 2, "{} pages left after freeing everything", h.stats().pages);
        assert_eq!(h.stats().live_blocks, 0);
    }

    #[test]
    fn large_requests_fall_back() {
        let mut h = PageHeap::new();
        let p = unsafe { h.alloc(5000) };
        unsafe { std::ptr::write_bytes(p, 1, 5000) };
        unsafe { h.free(p, 5000) };
        assert_eq!(h.stats().pages, 0);
    }
}
