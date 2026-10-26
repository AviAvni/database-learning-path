//! Lane 3 (STUB) — cross-thread frees without blowup.
//!
//! Extend lane 2's design to many threads, following mimalloc §2.4 and Hoard §2.2:
//!
//! * A [`SharedHeap`] owns all pages. Each thread works through its own
//!   [`Handle`], which owns a set of pages (like lane 2's `PageHeap`) and
//!   allocates **only** from pages it owns — no locks on the fast path.
//! * Any handle may free any block. A handle that frees a block from a page it
//!   owns pushes it on the page's local free list. A handle that frees a block
//!   from *someone else's* page pushes it on that page's **thread-free list**
//!   with a compare-and-swap (an atomic stack, one per page — so threads freeing
//!   into different pages never contend).
//! * The owner moves a page's whole thread-free list over to its local list in
//!   one atomic swap, only on its slow path: the remote frees are *batched*.
//! * Memory freed remotely must come back to the owner, or a producer/consumer
//!   pair — one thread allocates, another frees — grows without bound. That is
//!   Hoard's **blowup** (§2.2: "a factor of P ... to unbounded"). The tests
//!   bound it.
//!
//! Pages may stay owned by the heap until it is dropped; lane 3 does not ask for
//! returning them to the OS. Drop must release every page.

use crate::heap::Heap;

pub struct SharedHeap {
    _todo: (),
}

impl SharedHeap {
    pub fn new() -> Self {
        todo!("lane 3: a registry of pages plus whatever shared state the handles need")
    }
    /// A per-thread handle. Cheap; create one per thread.
    pub fn handle(&self) -> Handle<'_> {
        todo!("lane 3: give the handle a unique owner id")
    }
    /// Pages owned by all handles together.
    pub fn pages(&self) -> usize {
        todo!("lane 3: total pages in the registry")
    }
}

impl Default for SharedHeap {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Handle<'a> {
    _heap: &'a SharedHeap,
}

impl Heap for Handle<'_> {
    fn name(&self) -> String {
        "shared".into()
    }
    unsafe fn alloc(&mut self, _size: usize) -> *mut u8 {
        todo!("lane 3: pop from an owned page; slow path collects thread-free lists, then new page")
    }
    unsafe fn free(&mut self, _p: *mut u8, _size: usize) {
        todo!("lane 3: owner == me ? local push : atomic push on the page's thread-free list")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn single_thread_roundtrip() {
        let heap = SharedHeap::new();
        let mut h = heap.handle();
        let v: Vec<_> = (0..5000).map(|i| (unsafe { h.alloc(16 + i % 200) }, 16 + i % 200)).collect();
        for &(p, s) in &v {
            unsafe { std::ptr::write_bytes(p, 3, s) };
        }
        for (p, s) in v {
            unsafe { h.free(p, s) };
        }
    }

    #[test]
    fn remote_frees_return_to_the_owner() {
        let heap = SharedHeap::new();
        let mut a = heap.handle();
        let first: Vec<usize> = (0..10_000).map(|_| unsafe { a.alloc(48) } as usize).collect();
        let pages = heap.pages();
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut b = heap.handle();
                for &p in &first {
                    unsafe { b.free(p as *mut u8, 48) };
                }
            });
        });
        for _ in 0..10_000 {
            unsafe { a.alloc(48) };
        }
        assert!(heap.pages() <= pages + 1, "{} pages, started with {pages}", heap.pages());
    }

    #[test]
    fn producer_consumer_memory_is_bounded() {
        // 2 producers allocate 300k 48-byte objects each and hand them, 512 at a
        // time, over a channel holding at most 4 batches; 2 consumers free them.
        // At most ~5k objects (~4 pages) are in flight per pair, so a heap that
        // never reuses remotely freed blocks would need ~440 pages.
        let heap = SharedHeap::new();
        std::thread::scope(|s| {
            for _ in 0..2 {
                let (tx, rx) = sync_channel::<Vec<usize>>(4);
                let heap = &heap;
                s.spawn(move || {
                    let mut h = heap.handle();
                    for b in 0..600 {
                        let batch: Vec<usize> = (0..512)
                            .map(|_| unsafe {
                                let p = h.alloc(48);
                                std::ptr::write_bytes(p, (b % 250) as u8, 48);
                                p as usize
                            })
                            .collect();
                        tx.send(batch).unwrap();
                    }
                });
                s.spawn(move || {
                    let mut h = heap.handle();
                    for (b, batch) in rx.iter().enumerate() {
                        for p in batch {
                            let bytes = unsafe { std::slice::from_raw_parts(p as *const u8, 48) };
                            assert!(bytes.iter().all(|&x| x == (b % 250) as u8), "block clobbered");
                            unsafe { h.free(p as *mut u8, 48) };
                        }
                    }
                });
            }
        });
        assert!(heap.pages() <= 40, "{} pages for an in-flight set of a few pages", heap.pages());
    }

    #[test]
    fn concurrent_local_churn_is_sound() {
        let heap = SharedHeap::new();
        std::thread::scope(|s| {
            for t in 0..4u8 {
                let heap = &heap;
                s.spawn(move || {
                    let mut h = heap.handle();
                    let mut v: Vec<(usize, usize)> = Vec::new();
                    for i in 0..50_000usize {
                        let size = 16 + (i * 7) % 300;
                        let p = unsafe { h.alloc(size) };
                        unsafe { std::ptr::write_bytes(p, t, size) };
                        v.push((p as usize, size));
                        if i % 3 == 2 {
                            let (q, qs) = v.swap_remove((i * 31) % v.len());
                            let bytes = unsafe { std::slice::from_raw_parts(q as *const u8, qs) };
                            assert!(bytes.iter().all(|&x| x == t));
                            unsafe { h.free(q as *mut u8, qs) };
                        }
                    }
                });
            }
        });
    }
}
