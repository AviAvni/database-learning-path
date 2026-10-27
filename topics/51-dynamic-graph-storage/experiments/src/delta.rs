//! CSR plus delta — the FalkorDB shape (topics/20-graphblas/
//! reading-falkordb-delta-matrix.md): a settled CSR `M`, a pending-additions
//! structure `DP`, a pending-deletions structure `DM`, and a flush that folds
//! both into a fresh `M` once `|DP| + |DM|` reaches a threshold.
//!
//! The logical graph is `A = (M ∪ DP) \ DM` with the same three invariants the
//! header of `delta_matrix.h` enumerates: `DP ∩ M = ∅`, `DM ⊆ M`,
//! `DP ∩ DM = ∅`.
//!
//! Modelling difference, stated once: FalkorDB's `DP`/`DM` are hypersparse
//! GraphBLAS matrices probed through library calls; here they are hash maps
//! from a row id to a short sorted `Vec`, which is the cheapest honest model
//! of "only non-empty rows are stored". The flush costs the same class of
//! work as `GrB_wait` (one pass over all of `M`) but the constants differ, so
//! read the *shape* of the numbers, not their absolute values.

use crate::csr::Csr;
use crate::hash::FxMap;
use crate::store::{GraphRead, GraphWrite, Maint};
use std::time::Instant;

/// How a scan skips the tombstones of a row that has any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tombstones {
    /// Test every edge of the row against the next tombstone: one compare
    /// and one data-dependent branch per edge.
    PerEdge,
    /// Binary-search each tombstone's position in the row and scan the plain
    /// runs between them with no per-edge test.
    Split,
}

pub struct CsrDelta {
    m: Csr,
    dp: FxMap<u32, Vec<u32>>,
    dm: FxMap<u32, Vec<u32>>,
    dp_n: usize,
    dm_n: usize,
    threshold: usize,
    maint: Maint,
    tombstones: Tombstones,
}

impl CsrDelta {
    pub fn new(m: Csr, threshold: usize) -> CsrDelta {
        CsrDelta {
            m,
            dp: FxMap::default(),
            dm: FxMap::default(),
            dp_n: 0,
            dm_n: 0,
            threshold,
            maint: Maint::default(),
            tombstones: Tombstones::Split,
        }
    }

    pub fn with_tombstones(mut self, t: Tombstones) -> CsrDelta {
        self.tombstones = t;
        self
    }

    pub fn pending(&self) -> usize {
        self.dp_n + self.dm_n
    }

    pub fn dp_rows(&self) -> usize {
        self.dp.len()
    }

    pub fn settled(&self) -> &Csr {
        &self.m
    }

    fn map_bytes(m: &FxMap<u32, Vec<u32>>) -> usize {
        // hashbrown: (key + value + 1 control byte) per bucket, buckets ≈ 8/7 of capacity.
        m.capacity() * (4 + 24 + 1) * 8 / 7 + m.values().map(|v| 4 * v.capacity()).sum::<usize>()
    }

    /// Fold `DP`/`DM` into a new `M`. Untouched vertex ranges are copied as
    /// one `memcpy` each; only rows named in a delta are merged.
    pub fn flush(&mut self) {
        let t = Instant::now();
        let n = self.m.n();
        let mut affected: Vec<u32> = self.dp.keys().chain(self.dm.keys()).copied().collect();
        affected.sort_unstable();
        affected.dedup();
        let total = self.m.edges.len() + self.dp_n - self.dm_n;
        let mut edges: Vec<u32> = Vec::with_capacity(total);
        let mut off = vec![0u32; n + 1];
        let mut cur = 0usize;
        for &u in &affected {
            let u = u as usize;
            let (s, e) = (self.m.offsets[cur] as usize, self.m.offsets[u] as usize);
            let shift = edges.len() as i64 - s as i64;
            edges.extend_from_slice(&self.m.edges[s..e]);
            for x in cur..u {
                off[x] = (self.m.offsets[x] as i64 + shift) as u32;
            }
            off[u] = edges.len() as u32;
            let row = self.m.row(u as u32);
            let add: &[u32] = self.dp.get(&(u as u32)).map_or(&[], |v| v);
            let del: &[u32] = self.dm.get(&(u as u32)).map_or(&[], |v| v);
            merge_row(row, add, del, &mut edges);
            cur = u + 1;
        }
        let (s, e) = (self.m.offsets[cur] as usize, self.m.offsets[n] as usize);
        let shift = edges.len() as i64 - s as i64;
        edges.extend_from_slice(&self.m.edges[s..e]);
        for x in cur..=n {
            off[x] = (self.m.offsets[x] as i64 + shift) as u32;
        }
        self.m = Csr { offsets: off, edges };
        self.dp.clear();
        self.dm.clear();
        self.dp_n = 0;
        self.dm_n = 0;
        let ns = t.elapsed().as_nanos();
        self.maint.flushes += 1;
        self.maint.flush_ns_total += ns;
        self.maint.flush_ns_max = self.maint.flush_ns_max.max(ns);
    }

    fn maybe_flush(&mut self) {
        if self.pending() >= self.threshold {
            self.flush();
        }
    }
}

/// `row` minus `del` (a sorted subset of `row`) plus `add` (sorted, disjoint
/// from `row`), sorted.
fn merge_row(row: &[u32], add: &[u32], del: &[u32], out: &mut Vec<u32>) {
    let (mut i, mut j, mut d) = (0, 0, 0);
    while i < row.len() || j < add.len() {
        let take_row = j >= add.len() || (i < row.len() && row[i] < add[j]);
        if take_row {
            if d < del.len() && del[d] == row[i] {
                d += 1;
            } else {
                out.push(row[i]);
            }
            i += 1;
        } else {
            out.push(add[j]);
            j += 1;
        }
    }
}

impl GraphRead for CsrDelta {
    fn name(&self) -> String {
        match self.tombstones {
            Tombstones::Split => format!("CSR + delta (flush at {})", self.threshold),
            Tombstones::PerEdge => "CSR + delta, per-edge tombstone test".to_string(),
        }
    }
    fn num_vertices(&self) -> usize {
        self.m.n()
    }
    fn num_edges(&self) -> usize {
        self.m.edges.len() + self.dp_n - self.dm_n
    }
    #[inline]
    fn for_each<F: FnMut(u32)>(&self, u: u32, mut f: F) {
        let row = self.m.row(u);
        // The same guard `delta_mxm.c` uses (`dm_nvals > 0`, `dp_nvals > 0`):
        // an empty delta costs nothing per row.
        match if self.dm.is_empty() { None } else { self.dm.get(&u) } {
            None => {
                for &x in row {
                    f(x);
                }
            }
            Some(t) if self.tombstones == Tombstones::PerEdge => {
                let mut j = 0;
                for &x in row {
                    if j < t.len() && t[j] == x {
                        j += 1;
                    } else {
                        f(x);
                    }
                }
            }
            Some(t) => {
                let mut from = 0;
                for &d in t {
                    let at = from + row[from..].partition_point(|&x| x < d);
                    for &x in &row[from..at] {
                        f(x);
                    }
                    from = at + 1;
                }
                for &x in &row[from.min(row.len())..] {
                    f(x);
                }
            }
        }
        if !self.dp.is_empty() {
            if let Some(p) = self.dp.get(&u) {
                for &x in p {
                    f(x);
                }
            }
        }
    }
    fn contains(&self, u: u32, v: u32) -> bool {
        // The read order of delta_isStored.c: DP, then DM, then M.
        if self.dp.get(&u).map_or(false, |p| p.binary_search(&v).is_ok()) {
            return true;
        }
        if self.dm.get(&u).map_or(false, |t| t.binary_search(&v).is_ok()) {
            return false;
        }
        self.m.contains_edge(u, v)
    }
    fn bytes(&self) -> usize {
        self.m.bytes_used() + Self::map_bytes(&self.dp) + Self::map_bytes(&self.dm)
    }
}

impl GraphWrite for CsrDelta {
    fn insert(&mut self, u: u32, v: u32) -> bool {
        // Re-adding a deleted edge resurrects it out of DM (delta_set_element_bool.c).
        if let Some(t) = self.dm.get_mut(&u) {
            if let Ok(i) = t.binary_search(&v) {
                t.remove(i);
                if t.is_empty() {
                    self.dm.remove(&u);
                }
                self.dm_n -= 1;
                return true;
            }
        }
        if self.m.contains_edge(u, v) {
            return false;
        }
        let p = self.dp.entry(u).or_default();
        match p.binary_search(&v) {
            Ok(_) => false,
            Err(i) => {
                p.insert(i, v);
                self.dp_n += 1;
                self.maybe_flush();
                true
            }
        }
    }
    fn delete(&mut self, u: u32, v: u32) -> bool {
        if let Some(p) = self.dp.get_mut(&u) {
            if let Ok(i) = p.binary_search(&v) {
                p.remove(i);
                if p.is_empty() {
                    self.dp.remove(&u);
                }
                self.dp_n -= 1;
                return true;
            }
        }
        if !self.m.contains_edge(u, v) {
            return false;
        }
        let t = self.dm.entry(u).or_default();
        match t.binary_search(&v) {
            Ok(_) => false,
            Err(i) => {
                t.insert(i, v);
                self.dm_n += 1;
                self.maybe_flush();
                true
            }
        }
    }
    fn maint(&self) -> Maint {
        Maint { pending: self.pending(), ..self.maint }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::{rmat, GraphParams};
    use crate::vecvec::VecVec;

    #[test]
    fn matches_vecvec_through_random_writes_and_flushes() {
        let el = rmat(GraphParams::TINY);
        let wl = crate::gen::workload(&el, GraphParams::TINY.scale, 64, 3000);
        let mut a = CsrDelta::new(Csr::from_edges(el.n, &el.edges), 100);
        let mut b = VecVec::from_edges(&el);
        for i in 0..3000 {
            let (u, v) = wl.inserts[i];
            assert_eq!(a.insert(u, v), b.insert(u, v));
            let (u, v) = wl.deletes[i];
            assert_eq!(a.delete(u, v), b.delete(u, v));
        }
        assert!(a.maint().flushes > 0);
        assert_eq!(a.num_edges(), b.num_edges());
        for &s in &wl.starts {
            assert_eq!(crate::store::two_hop(&a, s), crate::store::two_hop(&b, s));
        }
        a.flush();
        assert_eq!(a.num_edges(), b.num_edges());
        assert_eq!(a.settled().edges.len(), b.num_edges());
    }

    #[test]
    fn both_tombstone_scans_agree() {
        let el = rmat(GraphParams::TINY);
        let wl = crate::gen::workload(&el, GraphParams::TINY.scale, 64, 2000);
        let mut a = CsrDelta::new(Csr::from_edges(el.n, &el.edges), usize::MAX);
        let mut b = CsrDelta::new(Csr::from_edges(el.n, &el.edges), usize::MAX).with_tombstones(Tombstones::PerEdge);
        for i in 0..2000 {
            let (u, v) = wl.deletes[i];
            assert_eq!(a.delete(u, v), b.delete(u, v));
            let (u, v) = wl.inserts[i];
            assert_eq!(a.insert(u, v), b.insert(u, v));
        }
        for u in 0..el.n {
            let (mut x, mut y) = (Vec::new(), Vec::new());
            a.for_each(u, |w| x.push(w));
            b.for_each(u, |w| y.push(w));
            x.sort_unstable();
            y.sort_unstable();
            assert_eq!(x, y, "vertex {u}");
        }
    }

    #[test]
    fn invariants_hold_without_flush() {
        let el = rmat(GraphParams::TINY);
        let mut a = CsrDelta::new(Csr::from_edges(el.n, &el.edges), usize::MAX);
        let (u, v) = el.edges[0];
        assert!(a.delete(u, v)); // M edge -> DM
        assert!(!a.contains(u, v));
        assert!(a.insert(u, v)); // resurrect out of DM
        assert!(a.contains(u, v));
        assert_eq!(a.pending(), 0);
        assert!(!a.insert(u, v));
    }
}
