//! The interface every lane implements, and the measurement harness that
//! drives it.
//!
//! The scan method is generic over the visitor closure (not `dyn`) so every
//! store's inner loop is monomorphised: a virtual call per edge would measure
//! the call, not the layout.

use crate::gen::Workload;
use std::hint::black_box;
use std::time::Instant;

pub trait GraphRead {
    fn name(&self) -> String;
    fn num_vertices(&self) -> usize;
    fn num_edges(&self) -> usize;
    /// Visit every out-neighbour of `u` once, in no particular order.
    fn for_each<F: FnMut(u32)>(&self, u: u32, f: F);
    fn contains(&self, u: u32, v: u32) -> bool;
    /// Bytes accounted from allocation sizes (capacity, not length). It
    /// excludes the allocator's own per-block overhead.
    fn bytes(&self) -> usize;
}

pub trait GraphWrite: GraphRead {
    /// Add edge (u, v). Returns `true` if it was not there before.
    fn insert(&mut self, u: u32, v: u32) -> bool;
    /// Remove edge (u, v). Returns `true` if it was there.
    fn delete(&mut self, u: u32, v: u32) -> bool;
    /// Deferred-work counters, for stores that have deferred work.
    fn maint(&self) -> Maint {
        Maint::default()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Maint {
    pub flushes: usize,
    pub flush_ns_total: u128,
    pub flush_ns_max: u128,
    /// Changes sitting in side structures right now.
    pub pending: usize,
}

/// Two-hop neighbourhood of `v`: every out-neighbour `u` of `v`, then every
/// out-neighbour `w` of each `u`. Returns (edges touched, order-independent
/// checksum of every vertex reached).
#[inline]
pub fn two_hop<G: GraphRead>(g: &G, v: u32) -> (u64, u64) {
    let mut edges = 0u64;
    let mut sum = 0u64;
    g.for_each(v, |u| {
        edges += 1;
        sum = sum.wrapping_add(u as u64);
        g.for_each(u, |w| {
            edges += 1;
            sum = sum.wrapping_add(w as u64);
        });
    });
    (edges, sum)
}

/// Run `starts` as two-hop queries; (edges touched, checksum, ns).
pub fn run_queries<G: GraphRead>(g: &G, starts: &[u32]) -> (u64, u64, u128) {
    let t = Instant::now();
    let mut e = 0u64;
    let mut c = 0u64;
    for &s in starts {
        let (de, dc) = two_hop(g, black_box(s));
        e += de;
        c = c.wrapping_add(dc);
    }
    let ns = t.elapsed().as_nanos();
    (black_box(e), black_box(c), ns)
}

/// Best of five over the same queries (the store is not mutated).
pub fn scan_ns_per_edge<G: GraphRead>(g: &G, starts: &[u32]) -> (f64, u64, u64) {
    let mut best = u128::MAX;
    let mut edges = 0;
    let mut cks = 0;
    for _ in 0..5 {
        let (e, c, ns) = run_queries(g, starts);
        best = best.min(ns);
        edges = e;
        cks = c;
    }
    (best as f64 / edges as f64, edges, cks)
}

/// What one interleaved run measured.
#[derive(Clone, Debug, Default)]
pub struct MixResult {
    pub queries: u64,
    pub query_edges: u64,
    pub query_ns: u128,
    pub checksum: u64,
    pub writes_tried: u64,
    pub writes_applied: u64,
    pub write_ns: u128,
    pub maint: Maint,
    pub final_edges: usize,
}

impl MixResult {
    pub fn ns_per_edge(&self) -> f64 {
        self.query_ns as f64 / self.query_edges as f64
    }
    pub fn ns_per_write(&self) -> f64 {
        self.write_ns as f64 / self.writes_tried.max(1) as f64
    }
}

/// Interleave queries with writes in blocks of `block` operations: in each
/// block, `block − w` two-hop queries run first, then `w` writes, where `w` is
/// `write_pct`% of the block. `del_share` of the writes (in percent of the
/// writes) are deletes, the rest inserts. Query time and write time are
/// accumulated separately — two clock reads per block segment, never per
/// operation, so the timer does not dominate a 100 ns insert.
pub fn run_mix<G: GraphWrite>(
    g: &mut G,
    wl: &Workload,
    blocks: usize,
    block: usize,
    write_pct: usize,
    del_share: usize,
) -> MixResult {
    let mut r = MixResult::default();
    let w = block * write_pct / 100;
    let n_del = w * del_share / 100;
    let n_ins = w - n_del;
    let q = block - w;
    let (mut qi, mut ii, mut di) = (0usize, 0usize, 0usize);
    for _ in 0..blocks {
        let t = Instant::now();
        for _ in 0..q {
            let s = wl.starts[qi % wl.starts.len()];
            qi += 1;
            let (e, c) = two_hop(&*g, black_box(s));
            r.query_edges += e;
            r.checksum = r.checksum.wrapping_add(c);
        }
        r.query_ns += t.elapsed().as_nanos();
        r.queries += q as u64;
        if w > 0 {
            let t = Instant::now();
            for _ in 0..n_ins {
                let (u, v) = wl.inserts[ii % wl.inserts.len()];
                ii += 1;
                r.writes_applied += g.insert(black_box(u), black_box(v)) as u64;
            }
            for _ in 0..n_del {
                let (u, v) = wl.deletes[di % wl.deletes.len()];
                di += 1;
                r.writes_applied += g.delete(black_box(u), black_box(v)) as u64;
            }
            r.write_ns += t.elapsed().as_nanos();
            r.writes_tried += w as u64;
        }
    }
    r.maint = g.maint();
    r.final_edges = g.num_edges();
    r
}
