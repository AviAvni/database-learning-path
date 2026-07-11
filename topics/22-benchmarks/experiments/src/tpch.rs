//! TPC-H Q1 and Q6 — the two ends of the choke-point spectrum
//! (Boncz's "TPC-H Analyzed"): Q1 = aggregation/expression pressure
//! with a tiny group domain, Q6 = pure scan+selection. Oracles are
//! deliberately row-at-a-time (topic 11's Volcano lane); the stubs
//! are the columnar versions that must match bit-for-bit… well,
//! within 1e-6 — floats reassociate (topic 17's lesson).

use crate::lineitem::LineItem;
use std::collections::HashMap;

/// Q1 group key: (returnflag, linestatus) — at most 6 live combos.
pub type Q1Key = (u8, u8);

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Q1Agg {
    pub sum_qty: f64,
    pub sum_base_price: f64,
    pub sum_disc_price: f64,
    pub sum_charge: f64,
    pub count: u64,
}

/// Q1 oracle: WHERE shipdate <= 2450 (dbgen's DATE '1998-12-01' - 90
/// analogue), GROUP BY returnflag, linestatus. Row-at-a-time HashMap.
pub fn q1_oracle(t: &LineItem) -> HashMap<Q1Key, Q1Agg> {
    let mut groups: HashMap<Q1Key, Q1Agg> = HashMap::new();
    for i in 0..t.len() {
        if t.shipdate[i] <= 2450 {
            let g = groups.entry((t.returnflag[i], t.linestatus[i])).or_default();
            let disc_price = t.extendedprice[i] * (1.0 - t.discount[i]);
            g.sum_qty += t.quantity[i];
            g.sum_base_price += t.extendedprice[i];
            g.sum_disc_price += disc_price;
            g.sum_charge += disc_price * (1.0 + t.tax[i]);
            g.count += 1;
        }
    }
    groups
}

/// Q6 oracle: SUM(extendedprice*discount) WHERE shipdate in one year
/// AND discount in [0.05, 0.07] AND quantity < 24. Branchy scalar.
pub fn q6_oracle(t: &LineItem) -> f64 {
    let mut rev = 0.0;
    for i in 0..t.len() {
        if t.shipdate[i] >= 730
            && t.shipdate[i] < 1095
            && t.discount[i] >= 0.05
            && t.discount[i] <= 0.07
            && t.quantity[i] < 24.0
        {
            rev += t.extendedprice[i] * t.discount[i];
        }
    }
    rev
}

/// STUB — columnar Q1: replace the HashMap with a FLAT ARRAY indexed
/// by a perfect group code (returnflag is A/N/R, linestatus O/F ⇒
/// code = rf_idx * 2 + ls_idx, 6 slots). Topic 11's flat-group-array
/// trick; this is what makes Q1 an *expression* benchmark instead of
/// a hash-table benchmark. Must match the oracle within 1e-6 rel.
pub fn q1_flat(t: &LineItem) -> HashMap<Q1Key, Q1Agg> {
    let _ = t;
    todo!("Q1 with flat group array (see docs)")
}

/// STUB — branchless columnar Q6: one pass, mask-multiply instead of
/// branches (topic 17's filter shapes — the 50%-selectivity branchy
/// crater is exactly what the oracle suffers). Autovectorization
/// should kick in; check with --emit=asm if curious.
pub fn q6_branchless(t: &LineItem) -> f64 {
    let _ = t;
    todo!("branchless Q6 (see docs)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lineitem::gen_lineitem;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
    }

    #[test]
    fn q1_flat_matches_oracle() {
        let t = gen_lineitem(0.01, 11);
        let (o, f) = (q1_oracle(&t), q1_flat(&t));
        assert_eq!(o.len(), f.len());
        for (k, g) in &o {
            let h = &f[k];
            assert_eq!(g.count, h.count);
            assert!(close(g.sum_charge, h.sum_charge), "{k:?}");
            assert!(close(g.sum_disc_price, h.sum_disc_price), "{k:?}");
        }
    }

    #[test]
    fn q6_branchless_matches_oracle() {
        let t = gen_lineitem(0.01, 12);
        assert!(close(q6_oracle(&t), q6_branchless(&t)));
    }
}
