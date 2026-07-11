//! STUB 3 — Hybrid Logical Clocks (Kulkarni et al., OPODIS '14).
//!
//! CockroachDB's answer to "no TrueTime hardware": a clock that is (a)
//! close to physical time (|l − pt| bounded by clock skew), (b) captures
//! happens-before like a Lamport clock, (c) fits in 64+16 bits. CRDB keeps
//! it in pkg/util/hlc (see reading-spanner-hlc.md).
//!
//! State per node: l (max physical time heard anywhere), c (logical tiebreak
//! counter within one l).
//!
//!   local/send event at physical time pt:
//!     l' = max(l, pt);  c' = (l' == l) ? c + 1 : 0
//!   receive message (m_l, m_c) at physical time pt:
//!     l' = max(l, m_l, pt)
//!     c' = l'==l==m_l -> max(c, m_c)+1 | l'==l -> c+1 | l'==m_l -> m_c+1 | else 0
//!
//! Timestamps order lexicographically: (l, c).

pub type Wall = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp {
    pub l: Wall,
    pub c: u32,
}

pub struct Hlc {
    pub l: Wall,
    pub c: u32,
}

impl Hlc {
    pub fn new() -> Self {
        Self { l: 0, c: 0 }
    }

    /// Local or send event at physical time `pt`.
    pub fn now(&mut self, _pt: Wall) -> Timestamp {
        todo!("stub: HLC local/send rule")
    }

    /// Receive event: merge remote timestamp `m` at physical time `pt`.
    pub fn recv(&mut self, _pt: Wall, _m: Timestamp) -> Timestamp {
        todo!("stub: HLC receive rule")
    }
}

impl Default for Hlc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_even_when_physical_clock_stalls() {
        let mut h = Hlc::new();
        let a = h.now(100);
        let b = h.now(100); // clock frozen
        let c = h.now(99); //  clock went BACKWARD
        assert!(b > a && c > b, "HLC must be strictly monotonic regardless of pt");
        assert_eq!(c.l, 100, "l never goes backward");
    }

    #[test]
    fn physical_time_advances_reset_logical() {
        let mut h = Hlc::new();
        h.now(100);
        h.now(100);
        let t = h.now(200);
        assert_eq!((t.l, t.c), (200, 0), "fresh wall time resets the counter");
    }

    #[test]
    fn happens_before_across_nodes_with_skew() {
        // node A's clock is far ahead of node B's.
        let mut a = Hlc::new();
        let mut b = Hlc::new();
        let m1 = a.now(1_000); // A sends
        let t1 = b.recv(3, m1); // B's clock says 3!
        assert!(t1 > m1, "receive must order after send despite skew");
        let m2 = b.now(4); // B sends onward
        assert!(m2 > t1);
        let t2 = a.recv(1_001, m2); // back at A
        assert!(t2 > m2, "causal chain a->b->a totally ordered");
    }

    #[test]
    fn l_is_bounded_by_max_physical_time_seen() {
        // The paper's key bound: l never exceeds the largest pt in the
        // system, so HLC stays within clock-skew of true time (unlike a
        // Lamport clock, which drifts unboundedly under bursts).
        let mut a = Hlc::new();
        let mut b = Hlc::new();
        let mut max_pt = 0;
        let mut m = a.now(500);
        max_pt = max_pt.max(500);
        for i in 0..1000u64 {
            m = b.recv(10 + i, m);
            max_pt = max_pt.max(10 + i);
            m = b.now(10 + i);
            assert!(m.l <= max_pt, "l {} escaped max physical time {max_pt}", m.l);
        }
        let t = a.recv(501, m);
        assert!(t.l <= max_pt.max(501));
    }

    #[test]
    fn concurrent_events_can_collide_without_node_id_tiebreak() {
        // Two nodes, same pt, no communication: both stamps are (100, 0).
        // HLC alone is NOT a total order over concurrent events — the paper
        // (and CRDB) breaks ties with the node id. The assertion makes that
        // burden explicit.
        let mut a = Hlc::new();
        let mut b = Hlc::new();
        let ta = a.now(100);
        let tb = b.now(100);
        assert_eq!(ta, tb, "collision expected: total order needs a node-id tiebreak");
    }
}
