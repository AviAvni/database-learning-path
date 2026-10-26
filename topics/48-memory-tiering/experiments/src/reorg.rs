//! LANE 2 (exercise) — address-space engineering: learn a layout from a
//! trace.
//!
//! Lane 1 measured what a *known* hot set costs packed versus scattered. A
//! real system does not know its hot set; OBASE (Banakar et al., OSDI'26)
//! learns it. Every dereference of a managed object sets an accessed bit in
//! the object's guide (§3.3); a background collector scans those bits each
//! window, counts consecutive inactive windows per object, and migrates
//! objects into HOT and COLD heaps that each occupy a contiguous virtual
//! range (§3.4), so a page-granular backend — kswapd, TPP, MEMTIS — sees
//! pages that are uniformly hot or uniformly cold.
//!
//! Implement [`learn_layout`]: given the ids accessed in one window, return a
//! new id -> slot map (a permutation of `0..n`) that packs the objects the
//! window found hot into the lowest slots.
//!
//!  1. Count accesses per id over `trace` (a `Vec<u32>` of length `n`; never
//!     iterate a `HashMap` — its order is not seeded, and the layout must be
//!     identical on every run).
//!  2. Order ids by count, descending, ties broken by id ascending.
//!  3. Assign slots in that order: the most-accessed id gets slot 0.
//!  4. Ids the window never touched follow, in id order.
//!
//! That is frequency packing, the simplest policy that satisfies the tests.
//! OBASE's own classifier is coarser on purpose — it keeps one accessed bit
//! and a consecutive-inactive-window counter per object, not a count — and
//! the exercises in the README ask you to build that and compare.
//!
//! The tests below are the specification. They restate two of the paper's
//! claims for this model: page utilization of the pages that serve most
//! accesses rises well above the scattered layout's (§5.2, "2–4×"), and the
//! fast-tier footprint needed to serve 90% of accesses shrinks to near the
//! oracle's.

/// Learn an id -> slot permutation of `0..n` from one window of accessed ids.
#[allow(unused_variables)] // stub: the arguments are the specification
pub fn learn_layout(trace: &[u32], n: u32) -> Vec<u32> {
    todo!("lane 2: count per id, order by (count desc, id asc), assign slots in that order")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{trace, Chain, Workload, SEED};
    use crate::layout::{count_window, count_window_by, Layout};

    const N: u32 = 1 << 16;
    const PAGE: u32 = 256; // 16 KiB pages of 64 B objects

    fn windows(hot_frac: f64) -> (Workload, Vec<u32>, Vec<u32>) {
        let w = Workload::hotspot(N, hot_frac);
        let mut c = Chain::new(SEED);
        let learn = trace(&w, &mut c, 200_000);
        let eval = trace(&w, &mut c, 200_000);
        (w, learn, eval)
    }

    #[test]
    fn returns_a_permutation() {
        let (_, learn, _) = windows(0.01);
        let slot_of = learn_layout(&learn, N);
        assert_eq!(slot_of.len(), N as usize);
        let mut seen = vec![false; N as usize];
        for &s in &slot_of {
            assert!(s < N && !seen[s as usize], "slot {s} assigned twice or out of range");
            seen[s as usize] = true;
        }
    }

    #[test]
    fn deterministic() {
        let (_, learn, _) = windows(0.01);
        assert_eq!(learn_layout(&learn, N), learn_layout(&learn, N));
    }

    #[test]
    fn hot_objects_land_in_the_first_pages() {
        let (w, learn, _) = windows(0.01);
        let slot_of = learn_layout(&learn, N);
        let hot_pages = w.hot.div_ceil(PAGE);
        let packed_hot = (0..w.hot).filter(|&id| slot_of[id as usize] < hot_pages * PAGE).count();
        assert!(
            packed_hot as f64 >= 0.95 * w.hot as f64,
            "only {packed_hot} of {} hot ids in the first {hot_pages} pages",
            w.hot
        );
    }

    #[test]
    fn held_out_window_needs_near_oracle_fast_memory() {
        let (_, learn, eval) = windows(0.01);
        let slot_of = learn_layout(&learn, N);
        let learned = count_window_by(&eval, N as usize, |id| slot_of[id as usize], PAGE, 0.9);
        let oracle = count_window(&eval, &Layout::packed(N), PAGE, 0.9);
        let scattered = count_window(&eval, &Layout::scattered(N), PAGE, 0.9);
        assert!(
            learned.pages_for_coverage as f64 <= 1.25 * oracle.pages_for_coverage as f64,
            "learned layout needs {} pages for 90% of accesses, oracle {}",
            learned.pages_for_coverage,
            oracle.pages_for_coverage
        );
        assert!(
            learned.hot_page_utilization >= 2.0 * scattered.hot_page_utilization,
            "hot-page utilization {:.3} not 2x scattered {:.3} (OBASE §5.2: 2–4x)",
            learned.hot_page_utilization,
            scattered.hot_page_utilization
        );
    }
}
