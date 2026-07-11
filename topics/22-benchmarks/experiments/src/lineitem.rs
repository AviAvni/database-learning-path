//! dbgen-lite: a columnar lineitem-ish table, seeded. SF 1 ≈ 6M rows
//! (real dbgen's ratio). Only the columns Q1 and Q6 touch — the point
//! is choke-point analysis, not schema fidelity.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub struct LineItem {
    pub quantity: Vec<f64>,       // 1..=50
    pub extendedprice: Vec<f64>,  // ~ 900..=105000
    pub discount: Vec<f64>,       // 0.00..=0.10
    pub tax: Vec<f64>,            // 0.00..=0.08
    pub returnflag: Vec<u8>,      // 'A' | 'N' | 'R'
    pub linestatus: Vec<u8>,      // 'O' | 'F'
    pub shipdate: Vec<u32>,       // days since 1992-01-01, 0..=2526
}

impl LineItem {
    pub fn len(&self) -> usize {
        self.quantity.len()
    }
    pub fn is_empty(&self) -> bool {
        self.quantity.is_empty()
    }
}

pub fn gen_lineitem(sf: f64, seed: u64) -> LineItem {
    let n = (sf * 6_000_000.0) as usize;
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut t = LineItem {
        quantity: Vec::with_capacity(n),
        extendedprice: Vec::with_capacity(n),
        discount: Vec::with_capacity(n),
        tax: Vec::with_capacity(n),
        returnflag: Vec::with_capacity(n),
        linestatus: Vec::with_capacity(n),
        shipdate: Vec::with_capacity(n),
    };
    for _ in 0..n {
        t.quantity.push(rng.gen_range(1..=50) as f64);
        t.extendedprice.push(rng.gen_range(900.0..=105_000.0));
        t.discount.push(rng.gen_range(0..=10) as f64 / 100.0);
        t.tax.push(rng.gen_range(0..=8) as f64 / 100.0);
        t.returnflag.push(*[b'A', b'N', b'R'].get(rng.gen_range(0..3)).unwrap());
        t.linestatus.push(if rng.gen_bool(0.5) { b'O' } else { b'F' });
        t.shipdate.push(rng.gen_range(0..=2526));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_determinism() {
        let t = gen_lineitem(0.001, 7);
        assert_eq!(t.len(), 6000);
        let u = gen_lineitem(0.001, 7);
        assert_eq!(t.shipdate, u.shipdate);
    }
}
