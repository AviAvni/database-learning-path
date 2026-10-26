//! The one interface every allocator in this crate is measured through, plus
//! the two provided implementations: the process's global allocator and a
//! bump arena.

use std::alloc::{alloc, dealloc, Layout};

/// A sized-free allocator, like Rust's `Layout`-based API: the caller hands the
/// size back on free, so no allocator needs a per-block header to find it.
pub trait Heap {
    fn name(&self) -> String;
    /// # Safety
    /// `size > 0`. The block is valid until freed (or until `end_scope` for arenas).
    unsafe fn alloc(&mut self, size: usize) -> *mut u8;
    /// # Safety
    /// `p` came from `alloc(size)` on this heap (or a sibling handle) and is freed once.
    unsafe fn free(&mut self, p: *mut u8, size: usize);
    /// End of a query: arenas reset here; everything else does nothing.
    fn end_scope(&mut self) {}
    /// Does `free` give memory back for reuse? A bump arena does not.
    fn reclaims(&self) -> bool {
        true
    }
}

/// Whatever `#[global_allocator]` the binary was built with (libmalloc on macOS,
/// glibc on Linux, or mimalloc / jemalloc under the cargo features).
#[derive(Clone, Copy, Default)]
pub struct Global;

impl Heap for Global {
    fn name(&self) -> String {
        "global".into()
    }
    unsafe fn alloc(&mut self, size: usize) -> *mut u8 {
        alloc(Layout::from_size_align_unchecked(size, 8))
    }
    unsafe fn free(&mut self, p: *mut u8, size: usize) {
        dealloc(p, Layout::from_size_align_unchecked(size, 8))
    }
}

const CHUNK: usize = 1 << 20;

/// A bump arena: pointer increment to allocate, nothing to free, `reset` to
/// reuse every chunk. This is the "query arena" a planner would hand each query.
pub struct Bump {
    chunks: Vec<(*mut u8, usize)>,
    cur: usize,
    off: usize,
}

impl Bump {
    pub fn new() -> Self {
        Bump { chunks: Vec::new(), cur: 0, off: 0 }
    }
    pub fn chunks(&self) -> usize {
        self.chunks.len()
    }
    pub fn reset(&mut self) {
        self.cur = 0;
        self.off = 0;
    }
    #[cold]
    fn grow(&mut self, size: usize) {
        // Reuse a retained chunk after a reset if one exists and fits.
        if self.cur + 1 < self.chunks.len() && self.chunks[self.cur + 1].1 >= size {
            self.cur += 1;
            self.off = 0;
            return;
        }
        let cap = size.max(CHUNK);
        // SAFETY: cap > 0, alignment 16 is a power of two.
        let p = unsafe { alloc(Layout::from_size_align_unchecked(cap, 16)) };
        assert!(!p.is_null());
        self.chunks.push((p, cap));
        self.cur = self.chunks.len() - 1;
        self.off = 0;
    }
}

impl Default for Bump {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Bump {
    fn drop(&mut self) {
        for &(p, cap) in &self.chunks {
            // SAFETY: allocated in `grow` with this layout.
            unsafe { dealloc(p, Layout::from_size_align_unchecked(cap, 16)) }
        }
    }
}

impl Heap for Bump {
    fn name(&self) -> String {
        "bump".into()
    }
    #[inline]
    unsafe fn alloc(&mut self, size: usize) -> *mut u8 {
        let size = (size + 7) & !7;
        if self.chunks.is_empty() || self.off + size > self.chunks[self.cur].1 {
            self.grow(size);
        }
        let p = self.chunks[self.cur].0.add(self.off);
        self.off += size;
        p
    }
    #[inline]
    unsafe fn free(&mut self, _p: *mut u8, _size: usize) {}
    fn end_scope(&mut self) {
        self.reset();
    }
    fn reclaims(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_hands_out_distinct_aligned_blocks() {
        let mut b = Bump::new();
        let mut seen = std::collections::HashSet::new();
        for i in 0..10_000usize {
            let p = unsafe { b.alloc(1 + i % 100) };
            assert_eq!(p as usize % 8, 0);
            assert!(seen.insert(p as usize));
        }
        assert!(b.chunks() >= 1);
    }

    #[test]
    fn bump_reset_reuses_chunks() {
        let mut b = Bump::new();
        for _ in 0..100_000 {
            unsafe { b.alloc(64) };
        }
        let n = b.chunks();
        b.end_scope();
        for _ in 0..100_000 {
            unsafe { b.alloc(64) };
        }
        assert_eq!(b.chunks(), n, "second pass must reuse the first pass's chunks");
    }

    #[test]
    fn global_roundtrip() {
        let mut g = Global;
        unsafe {
            let p = g.alloc(48);
            std::ptr::write_bytes(p, 7, 48);
            g.free(p, 48);
        }
    }
}
