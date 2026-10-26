//! Where each object lives: a layout maps an object id to a slot (its index
//! in a flat array of 64-byte objects), and the slot decides the cache line
//! and the page.
//!
//! The two lane-1 layouts are arithmetic, not tables, so the timed loop pays
//! no extra memory access to find an object:
//!
//! - **packed** — `slot = id`. The hot set `[0, hot)` occupies the first
//!   `hot` slots, contiguous: hot objects share lines and pages with each
//!   other only.
//! - **scattered** — `slot = id × M mod n` for an odd `M` (a bijection when
//!   `n` is a power of two). Consecutive ids land far apart, so each hot
//!   object shares its line and its page with cold ones: the "hotness
//!   fragmentation" OBASE (OSDI'26, §1) measures in Google's fleet, where an
//!   allocator places objects by size and arrival rather than by heat.
//!
//! Both run the identical instruction sequence (`id × mult & mask`); only the
//! multiplier differs.

/// A multiplicative layout over a power-of-two number of slots.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub mult: u32,
    pub inv: u32,
    pub mask: u32,
    pub name: &'static str,
}

/// Golden-ratio multiplier, forced odd. Any odd constant is a bijection
/// mod 2^k; this one spreads consecutive ids evenly (Knuth's multiplicative
/// hashing), which is the worst case for page-granular tiering.
const SCATTER: u32 = 0x9E37_79B1;

/// Multiplicative inverse of an odd `a` modulo 2^32 (Newton's iteration:
/// each step doubles the number of correct low bits).
fn inverse(a: u32) -> u32 {
    assert!(a & 1 == 1);
    let mut x = a; // correct to 3 bits for odd a
    for _ in 0..5 {
        x = x.wrapping_mul(2u32.wrapping_sub(a.wrapping_mul(x)));
    }
    x
}

impl Layout {
    pub fn packed(n: u32) -> Self {
        assert!(n.is_power_of_two());
        Layout { mult: 1, inv: 1, mask: n - 1, name: "packed" }
    }

    pub fn scattered(n: u32) -> Self {
        assert!(n.is_power_of_two());
        Layout { mult: SCATTER, inv: inverse(SCATTER), mask: n - 1, name: "scattered" }
    }

    #[inline(always)]
    pub fn slot(&self, id: u32) -> u32 {
        id.wrapping_mul(self.mult) & self.mask
    }

    /// The id stored at `slot` — used to initialise memory in slot order
    /// (sequential writes) rather than id order (random writes).
    #[inline(always)]
    pub fn id_at(&self, slot: u32) -> u32 {
        slot.wrapping_mul(self.inv) & self.mask
    }
}

/// Exact, deterministic counts for one window of accesses under one layout.
/// Nothing here is timed; these reproduce to the digit on any machine.
#[derive(Clone, Debug, Default)]
pub struct WindowCounts {
    pub accesses: usize,
    /// distinct objects (= distinct 64 B units) touched in the window
    pub distinct_objs: usize,
    /// distinct pages touched in the window
    pub pages_touched: usize,
    /// the fewest pages that together receive `coverage` of the window's
    /// accesses — the fast-tier capacity a page-granular policy with perfect
    /// knowledge would need to serve that share from fast memory
    pub pages_for_coverage: usize,
    /// OBASE's page utilization (§2.1) over the pages touched: unique bytes
    /// accessed / bytes of the pages touched
    pub utilization: f64,
    /// the same ratio restricted to the `pages_for_coverage` hottest pages:
    /// how much of the fast tier those pages fill with bytes anyone touched
    pub hot_page_utilization: f64,
}

/// Count `trace` (object ids) under `layout` at `objs_per_page` objects per
/// page, with `coverage` the share of accesses the hot pages must absorb.
pub fn count_window(trace: &[u32], layout: &Layout, objs_per_page: u32, coverage: f64) -> WindowCounts {
    count_window_by(trace, layout.mask as usize + 1, |id| layout.slot(id), objs_per_page, coverage)
}

/// [`count_window`] for any id -> slot map over `n` slots (lane 2's learned
/// layouts are tables, not arithmetic).
pub fn count_window_by(
    trace: &[u32],
    n: usize,
    slot_of: impl Fn(u32) -> u32,
    objs_per_page: u32,
    coverage: f64,
) -> WindowCounts {
    let pages = n / objs_per_page as usize;
    let mut seen = vec![0u64; n.div_ceil(64)];
    let mut page_hits = vec![0u32; pages];
    let mut page_objs = vec![0u32; pages];
    let mut distinct = 0usize;
    for &id in trace {
        let slot = slot_of(id) as usize;
        let page = slot / objs_per_page as usize;
        page_hits[page] += 1;
        let (w, b) = (slot / 64, slot % 64);
        if seen[w] >> b & 1 == 0 {
            seen[w] |= 1 << b;
            distinct += 1;
            page_objs[page] += 1;
        }
    }
    let touched = page_hits.iter().filter(|&&h| h > 0).count();

    // hottest pages first; ties broken by page number so the order — and the
    // utilisation of the chosen set — is deterministic
    let mut order: Vec<u32> = (0..pages as u32).filter(|&p| page_hits[p as usize] > 0).collect();
    order.sort_unstable_by(|&a, &b| page_hits[b as usize].cmp(&page_hits[a as usize]).then(a.cmp(&b)));
    let need = (coverage * trace.len() as f64).ceil() as u64;
    let (mut acc, mut k, mut hot_objs) = (0u64, 0usize, 0u64);
    while acc < need && k < order.len() {
        let p = order[k] as usize;
        acc += page_hits[p] as u64;
        hot_objs += page_objs[p] as u64;
        k += 1;
    }

    WindowCounts {
        accesses: trace.len(),
        distinct_objs: distinct,
        pages_touched: touched,
        pages_for_coverage: k,
        utilization: distinct as f64 / (touched as f64 * objs_per_page as f64),
        hot_page_utilization: hot_objs as f64 / (k.max(1) as f64 * objs_per_page as f64),
    }
}

/// Lines and pages the hot set `[0, hot)` occupies under `layout`.
pub fn hot_footprint(layout: &Layout, hot: u32, objs_per_line: u32, objs_per_page: u32) -> (usize, usize) {
    let n = layout.mask as usize + 1;
    let mut lines = vec![0u64; (n / objs_per_line as usize).div_ceil(64)];
    let mut pages = vec![0u64; (n / objs_per_page as usize).div_ceil(64)];
    let (mut nl, mut np) = (0usize, 0usize);
    for id in 0..hot {
        let s = layout.slot(id) as usize;
        let (l, p) = (s / objs_per_line as usize, s / objs_per_page as usize);
        if lines[l / 64] >> (l % 64) & 1 == 0 {
            lines[l / 64] |= 1 << (l % 64);
            nl += 1;
        }
        if pages[p / 64] >> (p % 64) & 1 == 0 {
            pages[p / 64] |= 1 << (p % 64);
            np += 1;
        }
    }
    (nl, np)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_bijections() {
        for l in [Layout::packed(1 << 12), Layout::scattered(1 << 12)] {
            let mut seen = vec![false; 1 << 12];
            for id in 0..(1u32 << 12) {
                let s = l.slot(id);
                assert!(!seen[s as usize], "{} maps two ids to slot {s}", l.name);
                seen[s as usize] = true;
                assert_eq!(l.id_at(s), id);
            }
        }
    }

    #[test]
    fn packed_hot_set_is_dense_scattered_is_not() {
        let n = 1 << 16;
        let hot = 1024; // 64 KiB of objects = 4 pages of 16 KiB
        let (pl, pp) = hot_footprint(&Layout::packed(n), hot, 2, 256);
        let (sl, sp) = hot_footprint(&Layout::scattered(n), hot, 2, 256);
        assert_eq!((pl, pp), (512, 4));
        assert_eq!(sl, 1024, "scattered: every hot object on its own line");
        assert!(sp > 200, "scattered: hot set spread over {sp} of 256 pages");
    }
}
