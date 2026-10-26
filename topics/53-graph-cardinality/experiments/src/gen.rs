//! A seeded labelled graph with Zipfian degrees (PROVIDED).
//!
//! Four node labels, five edge types, each type running between a fixed pair
//! of labels:
//!
//! ```text
//!   r0: L0 → L1    r1: L1 → L2    r2: L2 → L3    r3: L3 → L0    r4: L2 → L0
//! ```
//!
//! so every walk of consecutive types is a label-consistent path, and
//! r0, r1, r4 close a triangle L0 → L1 → L2 → L0. Nodes carry exactly one
//! label and are numbered label by label, so label `l` owns the id range
//! `label_off[l]..label_off[l+1]`.
//!
//! Each edge type draws its sources and destinations from a Zipf law over a
//! popularity ranking of the nodes of the right label (rank r has weight
//! `(r+1)^-s`; sources use `s_src`, destinations `s_dst`). The ranking is the
//! one knob that matters for estimation:
//!
//! * [`Popularity::Shared`] — one ranking per label, used by every edge type
//!   and both roles, so the node that is the most popular destination of r0
//!   is also the most popular source of r1: hubs are hubs everywhere. This is
//!   the correlation between a node's in-degree and out-degree that the
//!   independence assumption cannot see.
//! * [`Popularity::Independent`] — a separate ranking per (type, role), so a
//!   node's in-degree and out-degree are unrelated. The negative control:
//!   here the independence assumption is true by construction.
//!
//! Edges are drawn with replacement and then sorted and de-duplicated, so
//! the graph is simple per type (at most one r_t edge between two nodes) and
//! `edges[t]` is the count after de-duplication. Nothing here iterates a hash
//! container, so every count reproduces from the seed.

use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub const NLABELS: usize = 4;
pub const NTYPES: usize = 5;
/// `(source label, destination label)` of each edge type.
pub const TYPES: [(usize, usize); NTYPES] = [(0, 1), (1, 2), (2, 3), (3, 0), (2, 0)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Popularity {
    Shared,
    Independent,
}

#[derive(Clone, Debug)]
pub struct Params {
    pub label_sizes: [usize; NLABELS],
    /// Edges drawn per type before de-duplication.
    pub edge_draws: [usize; NTYPES],
    pub s_src: f64,
    pub s_dst: f64,
    pub pop: Popularity,
    pub seed: u64,
}

impl Params {
    /// The lane-1 graph: 27,000 nodes, 280,000 edge draws.
    pub fn lane1(seed: u64, pop: Popularity) -> Params {
        Params {
            label_sizes: [3000, 6000, 12000, 6000],
            edge_draws: [40_000, 60_000, 90_000, 60_000, 30_000],
            s_src: 0.6,
            s_dst: 1.0,
            pop,
            seed,
        }
    }
    /// A graph small enough for tests: 2,700 nodes, ~28,000 edge draws.
    pub fn small(seed: u64, pop: Popularity) -> Params {
        Params {
            label_sizes: [300, 600, 1200, 600],
            edge_draws: [4000, 6000, 9000, 6000, 3000],
            s_src: 0.6,
            s_dst: 1.0,
            pop,
            seed,
        }
    }
}

/// Compressed adjacency: `nbrs(v)` is sorted ascending.
#[derive(Clone, Debug, Default)]
pub struct Csr {
    pub off: Vec<u32>,
    pub adj: Vec<u32>,
}

impl Csr {
    fn build(n: usize, edges: &[(u32, u32)]) -> Csr {
        // `edges` must be sorted by (from, to).
        let mut off = vec![0u32; n + 1];
        for &(a, _) in edges {
            off[a as usize + 1] += 1;
        }
        for i in 0..n {
            off[i + 1] += off[i];
        }
        Csr { off, adj: edges.iter().map(|&(_, b)| b).collect() }
    }
    #[inline]
    pub fn nbrs(&self, v: usize) -> &[u32] {
        &self.adj[self.off[v] as usize..self.off[v + 1] as usize]
    }
    #[inline]
    pub fn deg(&self, v: usize) -> usize {
        (self.off[v + 1] - self.off[v]) as usize
    }
}

#[derive(Clone, Debug)]
pub struct Graph {
    pub n: usize,
    pub label_sizes: [usize; NLABELS],
    pub label_off: [usize; NLABELS + 1],
    /// `out[t]` maps a source to its destinations; `inn[t]` the reverse.
    pub out: Vec<Csr>,
    pub inn: Vec<Csr>,
    /// Edge count per type (after de-duplication).
    pub edges: [usize; NTYPES],
}

impl Graph {
    /// Build from explicit, per-type edge lists (used by tests). Lists are
    /// sorted and de-duplicated here.
    pub fn from_edges(label_sizes: [usize; NLABELS], mut lists: Vec<Vec<(u32, u32)>>) -> Graph {
        assert_eq!(lists.len(), NTYPES);
        let mut label_off = [0usize; NLABELS + 1];
        for l in 0..NLABELS {
            label_off[l + 1] = label_off[l] + label_sizes[l];
        }
        let n = label_off[NLABELS];
        let mut out = Vec::new();
        let mut inn = Vec::new();
        let mut edges = [0usize; NTYPES];
        for t in 0..NTYPES {
            let e = &mut lists[t];
            e.sort_unstable();
            e.dedup();
            edges[t] = e.len();
            out.push(Csr::build(n, e));
            let mut rev: Vec<(u32, u32)> = e.iter().map(|&(a, b)| (b, a)).collect();
            rev.sort_unstable();
            inn.push(Csr::build(n, &rev));
        }
        Graph { n, label_sizes, label_off, out, inn, edges }
    }

    pub fn label_range(&self, l: usize) -> std::ops::Range<usize> {
        self.label_off[l]..self.label_off[l + 1]
    }
}

fn zipf_cdf(n: usize, s: f64) -> Vec<f64> {
    let mut c = Vec::with_capacity(n);
    let mut acc = 0.0;
    for r in 0..n {
        acc += ((r + 1) as f64).powf(-s);
        c.push(acc);
    }
    for x in c.iter_mut() {
        *x /= acc;
    }
    c
}

fn sample(cdf: &[f64], rng: &mut ChaCha8Rng) -> usize {
    let u: f64 = rng.gen();
    cdf.partition_point(|&c| c <= u).min(cdf.len() - 1)
}

fn ranking(range: std::ops::Range<usize>, rng: &mut ChaCha8Rng) -> Vec<u32> {
    let mut v: Vec<u32> = range.map(|x| x as u32).collect();
    v.shuffle(rng);
    v
}

pub fn generate(p: &Params) -> Graph {
    let mut rng = ChaCha8Rng::seed_from_u64(p.seed);
    let mut label_off = [0usize; NLABELS + 1];
    for l in 0..NLABELS {
        label_off[l + 1] = label_off[l] + p.label_sizes[l];
    }
    let range = |l: usize| label_off[l]..label_off[l + 1];

    // Popularity rankings: rank -> node id.
    let shared: Vec<Vec<u32>> = if p.pop == Popularity::Shared {
        (0..NLABELS).map(|l| ranking(range(l), &mut rng)).collect()
    } else {
        Vec::new()
    };
    let mut src_rank: Vec<Vec<u32>> = Vec::new();
    let mut dst_rank: Vec<Vec<u32>> = Vec::new();
    for t in 0..NTYPES {
        let (ls, ld) = TYPES[t];
        match p.pop {
            Popularity::Shared => {
                src_rank.push(shared[ls].clone());
                dst_rank.push(shared[ld].clone());
            }
            Popularity::Independent => {
                src_rank.push(ranking(range(ls), &mut rng));
                dst_rank.push(ranking(range(ld), &mut rng));
            }
        }
    }

    let mut lists = Vec::new();
    for t in 0..NTYPES {
        let (ls, ld) = TYPES[t];
        let cs = zipf_cdf(p.label_sizes[ls], p.s_src);
        let cd = zipf_cdf(p.label_sizes[ld], p.s_dst);
        let mut e = Vec::with_capacity(p.edge_draws[t]);
        for _ in 0..p.edge_draws[t] {
            let a = src_rank[t][sample(&cs, &mut rng)];
            let b = dst_rank[t][sample(&cd, &mut rng)];
            e.push((a, b));
        }
        lists.push(e);
    }
    Graph::from_edges(p.label_sizes, lists)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_graph_and_csr_is_consistent() {
        let a = generate(&Params::small(7, Popularity::Shared));
        let b = generate(&Params::small(7, Popularity::Shared));
        assert_eq!(a.edges, b.edges);
        assert_eq!(a.out[1].adj, b.out[1].adj);
        for t in 0..NTYPES {
            let (ls, ld) = TYPES[t];
            let mut seen = 0;
            for v in 0..a.n {
                let nb = a.out[t].nbrs(v);
                assert!(nb.windows(2).all(|w| w[0] < w[1]), "sorted, de-duplicated");
                if !nb.is_empty() {
                    assert!(a.label_range(ls).contains(&v));
                }
                for &w in nb {
                    assert!(a.label_range(ld).contains(&(w as usize)));
                }
                seen += nb.len();
            }
            assert_eq!(seen, a.edges[t]);
            assert_eq!(a.inn[t].adj.len(), a.edges[t]);
        }
    }

    #[test]
    fn popularity_modes_differ_in_the_in_out_degree_correlation() {
        // Pearson correlation between in-degree via r0 and out-degree via r1 on L1.
        fn corr(g: &Graph) -> f64 {
            let r = g.label_range(1);
            let n = r.len() as f64;
            let xs: Vec<f64> = r.clone().map(|v| g.inn[0].deg(v) as f64).collect();
            let ys: Vec<f64> = r.map(|v| g.out[1].deg(v) as f64).collect();
            let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
            let cov: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
            let vx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
            let vy: f64 = ys.iter().map(|y| (y - my).powi(2)).sum();
            cov / (vx.sqrt() * vy.sqrt())
        }
        let s = corr(&generate(&Params::small(3, Popularity::Shared)));
        let i = corr(&generate(&Params::small(3, Popularity::Independent)));
        assert!(s > 0.5, "shared {s}");
        assert!(i.abs() < 0.2, "independent {i}");
    }
}
