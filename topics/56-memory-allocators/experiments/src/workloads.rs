//! The measured workloads, generic over [`Heap`] (PROVIDED).
//!
//! * `load`  — bulk load: allocate every object of a trace and keep it.
//! * `churn` — delete half the objects, then insert replacements whose
//!   property values have *grown*, so freed blocks never fit the new requests.
//! * `query` — a query arena pattern: allocate a burst of short-lived
//!   intermediates, free them all at the end, repeat.

use crate::heap::Heap;
use crate::trace::{pick, Kind, Obj, GROWN_SIZES};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

#[derive(Clone, Copy)]
pub struct Live {
    pub p: *mut u8,
    pub size: u32,
    pub kind: Kind,
}

// SAFETY: a Live is just a pointer and its size; ownership discipline is the caller's.
unsafe impl Send for Live {}

/// Initialise the way a real object would be: write the head and the tail, so
/// the pages are genuinely resident and the compiler cannot elide the allocation.
#[inline(always)]
pub unsafe fn touch(p: *mut u8, size: usize) {
    (p as *mut u64).write(size as u64);
    p.add(size - 1).write(0xA5);
}

pub fn load<H: Heap>(h: &mut H, trace: &[Obj]) -> Vec<Live> {
    let mut live = Vec::with_capacity(trace.len());
    for o in trace {
        // SAFETY: sizes are ≥ 16; the block is ours until `churn` frees it.
        unsafe {
            let p = h.alloc(o.size as usize);
            touch(p, o.size as usize);
            live.push(Live { p, size: o.size, kind: o.kind });
        }
    }
    live
}

/// `rounds` rounds of: free a random 50% of objects, then allocate replacements
/// (properties redrawn from `GROWN_SIZES`). Returns the number of allocations made.
pub fn churn<H: Heap>(h: &mut H, live: &mut [Live], rounds: usize, seed: u64) -> u64 {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut n_alloc = 0u64;
    let mut victims: Vec<usize> = Vec::with_capacity(live.len() / 2 + 1);
    for _ in 0..rounds {
        victims.clear();
        for i in 0..live.len() {
            if rng.gen_bool(0.5) {
                victims.push(i);
            }
        }
        for &i in &victims {
            // SAFETY: each live slot holds a block from this heap, freed once per round.
            unsafe { h.free(live[i].p, live[i].size as usize) };
        }
        for &i in &victims {
            let size = match live[i].kind {
                Kind::Prop => pick(&mut rng, &GROWN_SIZES),
                _ => live[i].size,
            };
            // SAFETY: as in `load`.
            unsafe {
                let p = h.alloc(size as usize);
                touch(p, size as usize);
                live[i].p = p;
                live[i].size = size;
            }
            n_alloc += 1;
        }
    }
    n_alloc
}

pub fn live_bytes(live: &[Live]) -> u64 {
    live.iter().map(|l| l.size as u64).sum()
}

pub fn free_all<H: Heap>(h: &mut H, live: &[Live]) {
    for l in live {
        // SAFETY: each slot holds a live block from this heap.
        unsafe { h.free(l.p, l.size as usize) };
    }
}

/// `queries` queries of `sizes.len()` intermediates each. Returns objects allocated.
pub fn query<H: Heap>(h: &mut H, sizes: &[u32], queries: usize) -> u64 {
    let mut ptrs: Vec<*mut u8> = Vec::with_capacity(sizes.len());
    for _ in 0..queries {
        ptrs.clear();
        for &s in sizes {
            // SAFETY: freed (or reset) at the end of this iteration.
            unsafe {
                let p = h.alloc(s as usize);
                touch(p, s as usize);
                ptrs.push(p);
            }
        }
        for (p, &s) in ptrs.iter().zip(sizes) {
            // SAFETY: allocated above, freed once.
            unsafe { h.free(*p, s as usize) };
        }
        h.end_scope();
    }
    (queries * sizes.len()) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::{Bump, Global};
    use crate::trace::bulk_load;

    #[test]
    fn load_and_churn_on_global_keep_bytes_consistent() {
        let t = bulk_load(2000, 56);
        let mut g = Global;
        let mut live = load(&mut g, &t);
        let before = live_bytes(&live);
        assert_eq!(before, crate::trace::live_bytes(&t));
        let n = churn(&mut g, &mut live, 2, 7);
        assert!(n > 0);
        assert!(live_bytes(&live) > before, "grown properties must raise live bytes");
        free_all(&mut g, &live);
    }

    #[test]
    fn bump_never_reclaims_on_churn() {
        let t = bulk_load(2000, 56);
        let mut b = Bump::new();
        let mut live = load(&mut b, &t);
        let c0 = b.chunks();
        churn(&mut b, &mut live, 2, 7);
        assert!(b.chunks() > c0);
    }

    #[test]
    fn query_burst_resets_the_arena() {
        let sizes = crate::trace::query_sizes(5000, 3);
        let mut b = Bump::new();
        query(&mut b, &sizes, 3);
        let c = b.chunks();
        query(&mut b, &sizes, 3);
        assert_eq!(b.chunks(), c);
    }
}
