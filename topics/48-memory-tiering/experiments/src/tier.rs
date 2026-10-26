//! LANE 3 (exercise) — a two-tier page-placement MODEL.
//!
//! This machine has one memory tier. Everything in this module is a
//! simulation priced with *published* numbers, and every figure it prints is
//! labelled a model: it says what a placement policy would do to the access
//! stream lane 1 measured, if the second tier had the latency the papers
//! measured on their hardware. It measures nothing.
//!
//! The simulated machine: `pages` pages of 4 KiB (the unit Linux migrates),
//! `fast_pages` of them in the fast tier (DRAM), the rest in the slow tier
//! (CXL). Each access to a page is a fast hit or a slow access; moving a page
//! is a promotion (slow -> fast) or a demotion (fast -> slow).
//!
//! Provided: three baselines — `first_touch` (Linux's default "local node
//! first, then the CXL node", TPP §6), `promote_on_touch` (promote every slow
//! page the moment it is touched, demoting the least-recently-used fast page
//! to make room — the hysteresis-free behaviour OBASE §5.3.2 attributes to
//! AutoNUMA), and `oracle_static` (the hottest pages of the window placed in
//! fast memory up front: the best any static placement can do) — plus the
//! cost models and Huang et al.'s data-movement model.
//!
//! Exercise: implement [`tpp`], TPP's policy (Maruf et al., ASPLOS'23 §5):
//!
//!  1. **Allocation:** a page's first touch places it in fast memory if a
//!     fast slot is free, else in slow memory. A first touch served from
//!     fast memory counts as a fast hit; the access that triggers a
//!     promotion counts as a slow access (it faulted on the slow page).
//!  2. **Headroom / proactive demotion (§5.2):** after every access, while
//!     free fast slots < `headroom(cfg)`, demote the least-recently-used fast
//!     page. TPP's `demote_scale_factor` default is 2% of the node.
//!  3. **Promotion with hysteresis (§5.3):** an access to a slow page that is
//!     not *active* marks it active and does not move it; an access to a
//!     slow page that is already active promotes it (demoting the LRU fast
//!     page first if no slot is free — promotion ignores the allocation
//!     watermark, §5.3).
//!  4. **Aging:** activity is per epoch of `cfg.fast_pages` accesses — a page
//!     is active if it was marked in the current epoch or the one before.
//!     (The kernel ages its LRU lists under pressure; this is the simplest
//!     stand-in.)
//!  5. Every access to a fast page refreshes it in the LRU ([`Lru::touch`]).
//!
//! The tests below are the specification.

/// Published latency/bandwidth points. Each field cites where it was read.
#[derive(Clone, Copy, Debug)]
pub struct CostModel {
    pub name: &'static str,
    /// fast-tier (local DRAM) load latency, ns
    pub fast_ns: f64,
    /// slow-tier (CXL) load latency, ns
    pub slow_ns: f64,
    /// ns to move one 4 KiB page: 4096 B at the slow tier's measured
    /// bandwidth. A LOWER BOUND — it charges no unmap, TLB shootdown or
    /// page-table update, which no paper read here prices per page.
    pub migrate_ns: f64,
}

/// SIDLE (Zhao et al., VLDB'26) Figure 1(c), Intel MLC on their CXL 1.1
/// platform: fast 111.2 ns / 33,095.3 MB/s; CXL 276.9 ns / 21,724.3 MB/s.
pub const SIDLE_CXL11: CostModel = CostModel {
    name: "SIDLE Fig. 1(c), CXL 1.1 (MLC)",
    fast_ns: 111.2,
    slow_ns: 276.9,
    migrate_ns: 4096.0 / 21_724.3e6 * 1e9,
};

/// TPP (Maruf et al., ASPLOS'23) Figure 2's ranges, at their midpoints:
/// main memory 80–140 ns, CXL memory 170–250 ns. No bandwidth is given in
/// that figure, so migration is priced at SIDLE's CXL bandwidth.
pub const TPP_FIG2_MID: CostModel = CostModel {
    name: "TPP Fig. 2 midpoints",
    fast_ns: 110.0,
    slow_ns: 210.0,
    migrate_ns: 4096.0 / 21_724.3e6 * 1e9,
};

#[derive(Clone, Copy, Debug)]
pub struct TierConfig {
    /// pages in the whole address space
    pub pages: u32,
    /// pages the fast tier holds
    pub fast_pages: u32,
}

/// TPP §5.2: reclaim until 2% of the fast node is free (at least one page).
pub fn headroom(cfg: &TierConfig) -> u32 {
    (cfg.fast_pages / 50).max(1)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TierStats {
    pub accesses: u64,
    pub fast_hits: u64,
    pub promotions: u64,
    pub demotions: u64,
}

impl TierStats {
    pub fn hit_rate(&self) -> f64 {
        self.fast_hits as f64 / self.accesses.max(1) as f64
    }
    /// MODEL ns per access: every access priced at its tier's latency, every
    /// migration at the model's per-page copy cost.
    pub fn model_ns(&self, m: &CostModel) -> f64 {
        let slow = self.accesses - self.fast_hits;
        (self.fast_hits as f64 * m.fast_ns
            + slow as f64 * m.slow_ns
            + (self.promotions + self.demotions) as f64 * m.migrate_ns)
            / self.accesses.max(1) as f64
    }
}

/// An exact LRU over page numbers: an intrusive doubly linked list in two
/// arrays, O(1) per operation. Front = most recent.
pub struct Lru {
    prev: Vec<u32>,
    next: Vec<u32>,
    head: u32,
    tail: u32,
    len: u32,
}

const NIL: u32 = u32::MAX;

impl Lru {
    pub fn new(pages: u32) -> Self {
        Lru { prev: vec![NIL; pages as usize], next: vec![NIL; pages as usize], head: NIL, tail: NIL, len: 0 }
    }
    pub fn len(&self) -> u32 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn push_front(&mut self, p: u32) {
        self.prev[p as usize] = NIL;
        self.next[p as usize] = self.head;
        if self.head != NIL {
            self.prev[self.head as usize] = p;
        } else {
            self.tail = p;
        }
        self.head = p;
        self.len += 1;
    }
    pub fn remove(&mut self, p: u32) {
        let (a, b) = (self.prev[p as usize], self.next[p as usize]);
        if a != NIL { self.next[a as usize] = b } else { self.head = b }
        if b != NIL { self.prev[b as usize] = a } else { self.tail = a }
        self.prev[p as usize] = NIL;
        self.next[p as usize] = NIL;
        self.len -= 1;
    }
    /// Move `p` (already in the list) to the front.
    pub fn touch(&mut self, p: u32) {
        if self.head != p {
            self.remove(p);
            self.push_front(p);
        }
    }
    /// Remove and return the least-recently-used page.
    pub fn pop_back(&mut self) -> Option<u32> {
        (self.tail != NIL).then(|| {
            let t = self.tail;
            self.remove(t);
            t
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    Unallocated,
    Fast,
    Slow,
}

/// Provided baseline: first touch fills fast memory, nothing ever moves.
pub fn first_touch(trace: &[u32], cfg: &TierConfig) -> TierStats {
    let mut at = vec![Where::Unallocated; cfg.pages as usize];
    let mut used = 0u32;
    let mut st = TierStats::default();
    for &p in trace {
        let w = &mut at[p as usize];
        if *w == Where::Unallocated {
            *w = if used < cfg.fast_pages { used += 1; Where::Fast } else { Where::Slow };
        }
        st.accesses += 1;
        st.fast_hits += (*w == Where::Fast) as u64;
    }
    st
}

/// Provided baseline: promote on every slow touch, demote the LRU fast page.
/// No hysteresis — a page touched once moves, and pushes out a page that
/// may be touched next.
pub fn promote_on_touch(trace: &[u32], cfg: &TierConfig) -> TierStats {
    let mut at = vec![Where::Unallocated; cfg.pages as usize];
    let mut lru = Lru::new(cfg.pages);
    let mut st = TierStats::default();
    for &p in trace {
        st.accesses += 1;
        match at[p as usize] {
            Where::Fast => {
                st.fast_hits += 1;
                lru.touch(p);
            }
            Where::Unallocated if lru.len() < cfg.fast_pages => {
                at[p as usize] = Where::Fast;
                lru.push_front(p);
                st.fast_hits += 1;
            }
            Where::Unallocated => at[p as usize] = Where::Slow,
            Where::Slow => {
                if lru.len() >= cfg.fast_pages {
                    let v = lru.pop_back().unwrap();
                    at[v as usize] = Where::Slow;
                    st.demotions += 1;
                }
                at[p as usize] = Where::Fast;
                lru.push_front(p);
                st.promotions += 1;
            }
        }
    }
    st
}

/// Provided baseline: the `fast_pages` most-accessed pages of the window
/// sit in fast memory for the whole window. No policy that only sees the
/// past can beat it on hits without migrating.
pub fn oracle_static(trace: &[u32], cfg: &TierConfig) -> TierStats {
    let mut hits = vec![0u32; cfg.pages as usize];
    for &p in trace {
        hits[p as usize] += 1;
    }
    let mut order: Vec<u32> = (0..cfg.pages).collect();
    order.sort_unstable_by(|&a, &b| hits[b as usize].cmp(&hits[a as usize]).then(a.cmp(&b)));
    let fast: u64 = order[..cfg.fast_pages.min(cfg.pages) as usize].iter().map(|&p| hits[p as usize] as u64).sum();
    TierStats { accesses: trace.len() as u64, fast_hits: fast, promotions: 0, demotions: 0 }
}

/// EXERCISE: TPP's placement policy — see the module docs for the five rules.
#[allow(unused_variables)] // stub: the arguments are the specification
pub fn tpp(trace: &[u32], cfg: &TierConfig) -> TierStats {
    todo!("lane 3: TPP — first-touch allocation, 2% headroom, promote on the second touch within two epochs")
}

/// Huang, Lu & Tan, "Hash Joins Meet CXL" (CIDR'26) §3.2, Eq. 4: the speedup
/// of moving a fraction `x` of the input from CXL to DRAM first, relative to
/// running entirely in CXL, with `k` = (move throughput) / (in-CXL
/// throughput) and `m` the slope of the linear bandwidth model
/// f(x) = m·x + 1.
pub fn hj_gain(k: f64, m: f64, x: f64) -> f64 {
    (k * m * x + k) / (m * x * x + x + k)
}

/// The maximiser of [`hj_gain`]: x* = (√(mk) − 1) / m (from Eq. 5).
pub fn hj_x_star(k: f64, m: f64) -> f64 {
    ((m * k).sqrt() - 1.0) / m
}

/// Eq. 6: the gain at x*, mk / (2√(mk) − 1) when mk ≥ 1, else 1.
pub fn hj_phi(k: f64, m: f64) -> f64 {
    let mk = m * k;
    if mk >= 1.0 { mk / (2.0 * mk.sqrt() - 1.0) } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{trace, Chain, Workload, SEED};
    use crate::layout::Layout;

    const N: u32 = 1 << 16;
    const OBJS_PER_PAGE: u32 = 64; // 4 KiB / 64 B
    const PAGES: u32 = N / OBJS_PER_PAGE;

    fn page_trace(layout: &Layout) -> Vec<u32> {
        let w = Workload::hotspot(N, 0.01);
        trace(&w, &mut Chain::new(SEED), 200_000).iter().map(|&id| layout.slot(id) / OBJS_PER_PAGE).collect()
    }

    fn cfg(frac: f64) -> TierConfig {
        TierConfig { pages: PAGES, fast_pages: (PAGES as f64 * frac) as u32 }
    }

    #[test]
    fn lru_is_exact() {
        let mut l = Lru::new(8);
        for p in [1, 2, 3] {
            l.push_front(p);
        }
        l.touch(1);
        assert_eq!(l.pop_back(), Some(2));
        assert_eq!(l.pop_back(), Some(3));
        assert_eq!(l.pop_back(), Some(1));
        assert_eq!(l.pop_back(), None);
    }

    #[test]
    fn hash_join_model_reproduces_table1_phj() {
        // Table 1 PHJ: k = 2.3534, m = 1.0977 -> x* = 0.5528, g = 1.1703.
        // x* reproduces to 3 decimals; g(x*) comes out 1.1665, not 1.1703.
        let (k, m) = (2.3534, 1.0977);
        assert!((hj_x_star(k, m) - 0.5528).abs() < 0.001);
        assert!((hj_phi(k, m) - hj_gain(k, m, hj_x_star(k, m))).abs() < 1e-9);
        assert!((hj_phi(k, m) - 1.1665).abs() < 0.0005);
    }

    #[test]
    fn tpp_everything_fits() {
        let t = page_trace(&Layout::scattered(N));
        let st = tpp(&t, &TierConfig { pages: PAGES, fast_pages: 2 * PAGES });
        assert_eq!(st.accesses, t.len() as u64);
        assert_eq!(st.fast_hits, st.accesses, "all of memory fits in the fast tier");
        assert_eq!(st.promotions + st.demotions, 0);
    }

    #[test]
    fn tpp_packed_hot_set_is_served_fast() {
        let st = tpp(&page_trace(&Layout::packed(N)), &cfg(0.2));
        assert!(st.hit_rate() >= 0.85, "packed, 20% fast: hit rate {:.3}", st.hit_rate());
    }

    #[test]
    fn tpp_scattered_hot_set_is_not() {
        let c = cfg(0.2);
        let packed = tpp(&page_trace(&Layout::packed(N)), &c);
        let scattered = tpp(&page_trace(&Layout::scattered(N)), &c);
        assert!(
            scattered.hit_rate() <= packed.hit_rate() - 0.25,
            "hotness fragmentation: scattered {:.3} vs packed {:.3}",
            scattered.hit_rate(),
            packed.hit_rate()
        );
    }

    #[test]
    fn tpp_hysteresis_cuts_ping_pong() {
        let t = page_trace(&Layout::scattered(N));
        let c = cfg(0.2);
        let naive = promote_on_touch(&t, &c);
        let st = tpp(&t, &c);
        assert!(
            st.promotions * 2 <= naive.promotions,
            "tpp promoted {} pages, promote-on-touch {}",
            st.promotions,
            naive.promotions
        );
        assert_eq!(tpp(&t, &c), st, "deterministic");
    }
}
