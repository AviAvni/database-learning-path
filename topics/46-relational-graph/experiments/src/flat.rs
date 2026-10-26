//! LANE 1 (provided) — the relational plan: a path pattern as left-deep binary
//! hash joins over one edge table.
//!
//! `(a)-[]->(b)-[]->(c)-[]->(d)` is, to a relational engine, the self-join
//! `R(a,b) ⋈ R(b,c) ⋈ R(c,d)` — exactly what PostgreSQL's reverted SQL/PGQ
//! rewriter and DuckPGQ's `MATCH` translation emit (one table reference per
//! element pattern, equality quals between them). A pipelined hash join never
//! has to *store* the intermediate, but it has to *produce* it: every tuple of
//! `R(a,b) ⋈ R(b,c)` is built, pushed into the next probe, and only then
//! extended or dropped. `JoinStats` counts those tuples operator by operator.
//!
//! The hash table maps a join key to the list of matching `dst` values (a
//! chaining table, as in Lehner & Neumann §4.4). It is only ever probed, never
//! iterated, so nothing here depends on hash iteration order.

use crate::gen::Edges;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A multiplicative hash for `u32` keys (the FxHash constant). SipHash would
/// make the relational side look slower than it is; real engines use a cheap
/// hash like this one.
#[derive(Default)]
pub struct FxHasher(u64);
impl Hasher for FxHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ b as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0.rotate_left(5) ^ i as u64).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}
pub type FxBuild = BuildHasherDefault<FxHasher>;

/// Build side of `R(x, y)` keyed by `x`: key → every `y`.
pub fn build_by_src(edges: &Edges) -> HashMap<u32, Vec<u32>, FxBuild> {
    let mut ht: HashMap<u32, Vec<u32>, FxBuild> = HashMap::default();
    for &(s, d) in edges {
        ht.entry(s).or_default().push(d);
    }
    ht
}

/// Tuples produced by each join operator of a left-deep pipeline.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct JoinStats {
    /// `out[i]` = tuples emitted by the i-th join (the last one is the result).
    pub out: Vec<u64>,
    /// Hash-table probes issued (one per tuple entering a join).
    pub probes: u64,
}

impl JoinStats {
    /// Every intermediate *and* result tuple the plan produced.
    pub fn tuples(&self) -> u64 {
        self.out.iter().sum()
    }
    /// Atomic values in those tuples: join i emits tuples of arity i + 3.
    pub fn values(&self) -> u64 {
        self.out.iter().enumerate().map(|(i, &t)| t * (i as u64 + 3)).sum()
    }
    pub fn result(&self) -> u64 {
        *self.out.last().unwrap_or(&0)
    }
}

/// `k`-hop path `R(x0,x1) ⋈ R(x1,x2) ⋈ … ⋈ R(xk-1,xk)`, left-deep, pipelined.
/// Scans `R` as the probe side of the first join; `ht` is `R` keyed by `src`.
/// `sink` sees every result tuple's last value (so the work cannot be elided).
pub fn k_hop(edges: &Edges, ht: &HashMap<u32, Vec<u32>, FxBuild>, k: usize, sink: &mut u64) -> JoinStats {
    assert!(k >= 2);
    let mut st = JoinStats { out: vec![0; k - 1], probes: 0 };
    for &(_a, b) in edges {
        extend(ht, b, 0, k - 1, &mut st, sink);
    }
    st
}

fn extend(ht: &HashMap<u32, Vec<u32>, FxBuild>, key: u32, join: usize, joins: usize, st: &mut JoinStats, sink: &mut u64) {
    st.probes += 1;
    let Some(matches) = ht.get(&key) else { return };
    st.out[join] += matches.len() as u64;
    if join + 1 == joins {
        for &v in matches {
            *sink = sink.wrapping_add(v as u64);
        }
    } else {
        for &v in matches {
            extend(ht, v, join + 1, joins, st, sink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::ffx_fig1;

    #[test]
    fn two_hop_on_the_ffx_example_is_13_tuples() {
        let e = ffx_fig1();
        let ht = build_by_src(&e);
        let mut s = 0;
        let st = k_hop(&e, &ht, 2, &mut s);
        assert_eq!(st.out, vec![13]);
        assert_eq!(st.values(), 39, "SIGMOD'26 §2.2.1: 13 flat tuples, 39 values");
    }
}
