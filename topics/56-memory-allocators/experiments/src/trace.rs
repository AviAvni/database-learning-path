//! Seeded graph-engine allocation traces.
//!
//! A property-graph engine allocates three kinds of small object while
//! loading: a **node** record (fixed 64 bytes: id, label set, adjacency
//! header), **edge** records (fixed 40 bytes: endpoints, type, property
//! pointer) and **property values** (strings and small arrays of widely
//! varying size). The shapes below are this repo's own stand-ins for those
//! three; they are not taken from FalkorDB or any paper. Everything is seeded,
//! so every count reproduces exactly.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub const NODE_SIZE: u32 = 64;
pub const EDGE_SIZE: u32 = 40;

/// (size, weight) of a property value at load time: mostly short strings.
pub const PROP_SIZES: [(u32, u32); 8] =
    [(16, 20), (24, 20), (32, 20), (48, 15), (64, 10), (96, 8), (128, 5), (256, 2)];
/// (size, weight) after a property update grows the value: the churn phase
/// replaces values with these, so freed blocks never fit the new requests.
pub const GROWN_SIZES: [(u32, u32); 6] =
    [(48, 10), (64, 20), (96, 25), (128, 20), (256, 15), (512, 10)];
/// (out-degree, weight): a heavy tail, mean 5.0.
pub const DEGREES: [(u32, u32); 4] = [(2, 50), (4, 30), (8, 15), (32, 5)];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Node = 0,
    Edge = 1,
    Prop = 2,
}

#[derive(Clone, Copy, Debug)]
pub struct Obj {
    pub kind: Kind,
    pub size: u32,
}

pub fn pick(rng: &mut ChaCha8Rng, table: &[(u32, u32)]) -> u32 {
    let total: u32 = table.iter().map(|t| t.1).sum();
    let mut x = rng.gen_range(0..total);
    for &(v, w) in table {
        if x < w {
            return v;
        }
        x -= w;
    }
    unreachable!()
}

/// The bulk-load trace: per node, one node record, its properties (2–4), then
/// its out-edges — the order a loader walking a CSV row would allocate in.
pub fn bulk_load(nodes: usize, seed: u64) -> Vec<Obj> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut v = Vec::with_capacity(nodes * 11);
    for _ in 0..nodes {
        v.push(Obj { kind: Kind::Node, size: NODE_SIZE });
        for _ in 0..rng.gen_range(2..=4) {
            v.push(Obj { kind: Kind::Prop, size: pick(&mut rng, &PROP_SIZES) });
        }
        for _ in 0..pick(&mut rng, &DEGREES) {
            v.push(Obj { kind: Kind::Edge, size: EDGE_SIZE });
        }
    }
    v
}

/// Request sizes of a query's intermediate objects (row buffers, path stubs).
pub const QUERY_SIZES: [(u32, u32); 4] = [(32, 40), (48, 30), (64, 20), (128, 10)];

pub fn query_sizes(n: usize, seed: u64) -> Vec<u32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    (0..n).map(|_| pick(&mut rng, &QUERY_SIZES)).collect()
}

pub fn live_bytes(t: &[Obj]) -> u64 {
    t.iter().map(|o| o.size as u64).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_is_deterministic() {
        let a = bulk_load(1000, 56);
        let b = bulk_load(1000, 56);
        assert_eq!(a.len(), b.len());
        assert_eq!(live_bytes(&a), live_bytes(&b));
        assert!(a.iter().zip(&b).all(|(x, y)| x.size == y.size && x.kind == y.kind));
    }

    #[test]
    fn trace_has_graph_shape() {
        let t = bulk_load(2000, 56);
        let nodes = t.iter().filter(|o| o.kind == Kind::Node).count();
        let edges = t.iter().filter(|o| o.kind == Kind::Edge).count();
        assert_eq!(nodes, 2000);
        let mean_deg = edges as f64 / nodes as f64;
        assert!((4.5..5.5).contains(&mean_deg), "mean degree {mean_deg}");
    }
}
