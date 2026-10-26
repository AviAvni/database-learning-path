//! Seeded graph generators. Every count printed by the benches reproduces
//! exactly: the only floating-point used is +, * and / (correctly rounded by
//! IEEE 754, so identical on every machine), never `powf`.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// A directed graph: sorted, unique edges, no self-loops.
#[derive(Clone, Debug)]
pub struct Graph {
    pub n: usize,
    pub edges: Vec<(u32, u32)>,
}

/// Compressed sparse rows: `row(u)` is the sorted neighbour list of `u`.
#[derive(Clone, Debug)]
pub struct Csr {
    pub off: Vec<usize>,
    pub col: Vec<u32>,
}

impl Csr {
    pub fn n(&self) -> usize {
        self.off.len() - 1
    }
    pub fn nnz(&self) -> usize {
        self.col.len()
    }
    #[inline]
    pub fn row(&self, u: usize) -> &[u32] {
        &self.col[self.off[u]..self.off[u + 1]]
    }
    #[inline]
    pub fn deg(&self, u: usize) -> usize {
        self.off[u + 1] - self.off[u]
    }
}

impl Graph {
    pub fn from_raw(n: usize, mut edges: Vec<(u32, u32)>) -> Graph {
        edges.retain(|&(u, v)| u != v);
        edges.sort_unstable();
        edges.dedup();
        Graph { n, edges }
    }

    /// Out-adjacency (`transpose = false`) or in-adjacency (`true`).
    pub fn csr(&self, transpose: bool) -> Csr {
        let mut off = vec![0usize; self.n + 1];
        for &(u, v) in &self.edges {
            let s = if transpose { v } else { u };
            off[s as usize + 1] += 1;
        }
        for i in 0..self.n {
            off[i + 1] += off[i];
        }
        let mut col = vec![0u32; self.edges.len()];
        let mut cur = off.clone();
        for &(u, v) in &self.edges {
            let (s, d) = if transpose { (v, u) } else { (u, v) };
            col[cur[s as usize]] = d;
            cur[s as usize] += 1;
        }
        for u in 0..self.n {
            col[off[u]..off[u + 1]].sort_unstable();
        }
        Csr { off, col }
    }

    pub fn out_csr(&self) -> Csr {
        self.csr(false)
    }
    pub fn in_csr(&self) -> Csr {
        self.csr(true)
    }
}

/// Prefix sums of the Zipf(alpha = 1) weights 1/(i+1).
fn zipf_cdf(n: usize) -> Vec<f64> {
    let mut c = Vec::with_capacity(n);
    let mut acc = 0.0f64;
    for i in 0..n {
        acc += 1.0 / (i as f64 + 1.0);
        c.push(acc);
    }
    c
}

fn zipf_sample(cdf: &[f64], rng: &mut ChaCha8Rng) -> usize {
    let u = (rng.gen::<u64>() >> 11) as f64 / (1u64 << 53) as f64;
    let x = u * cdf[cdf.len() - 1];
    cdf.partition_point(|&c| c <= x).min(cdf.len() - 1)
}

/// Low-diameter, heavy-tailed digraph: `m_raw` edges whose source and
/// destination are each drawn from Zipf(1) over `n` vertices, with the
/// destination ranks shuffled so the out-hubs and the in-hubs are different
/// vertices (in-degree and out-degree are skewed independently). Duplicates
/// and self-loops are dropped, so the edge count is a little under `m_raw`.
pub fn power_law(n: usize, m_raw: usize, seed: u64) -> Graph {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let cdf = zipf_cdf(n);
    let mut perm: Vec<u32> = (0..n as u32).collect();
    for i in (1..n).rev() {
        let j = rng.gen_range(0..=i);
        perm.swap(i, j);
    }
    let mut edges = Vec::with_capacity(m_raw);
    for _ in 0..m_raw {
        let u = zipf_sample(&cdf, &mut rng) as u32;
        let v = perm[zipf_sample(&cdf, &mut rng)];
        edges.push((u, v));
    }
    Graph::from_raw(n, edges)
}

/// `w` x `h` grid, edges right and down: a DAG of diameter w + h - 2.
pub fn grid(w: usize, h: usize) -> Graph {
    let mut edges = Vec::new();
    for r in 0..h {
        for c in 0..w {
            let id = (r * w + c) as u32;
            if c + 1 < w {
                edges.push((id, id + 1));
            }
            if r + 1 < h {
                edges.push((id, id + w as u32));
            }
        }
    }
    Graph::from_raw(w * h, edges)
}

/// A single directed path 0 -> 1 -> ... -> n-1.
pub fn chain(n: usize) -> Graph {
    let edges = (0..n as u32 - 1).map(|i| (i, i + 1)).collect();
    Graph::from_raw(n, edges)
}

/// `k` disjoint directed paths of `len` vertices each: a graph with many
/// vertices and a tiny closure, the case a dense n x n matrix pays for.
pub fn chains(k: usize, len: usize) -> Graph {
    let mut edges = Vec::new();
    for c in 0..k {
        let base = (c * len) as u32;
        for i in 0..len as u32 - 1 {
            edges.push((base + i, base + i + 1));
        }
    }
    Graph::from_raw(k * len, edges)
}

/// The datasets every lane runs on, with a stable name.
pub struct Dataset {
    pub name: &'static str,
    pub graph: Graph,
}

/// Transitive-closure datasets: two shapes, the extreme chain, and a sparse
/// many-vertex case.
pub fn tc_datasets() -> Vec<Dataset> {
    vec![
        Dataset { name: "power-law n=1500", graph: power_law(1500, 4500, 55) },
        Dataset { name: "grid 24x24", graph: grid(24, 24) },
        Dataset { name: "chain n=500", graph: chain(500) },
        Dataset { name: "6000 chains x 10", graph: chains(6000, 10) },
    ]
}

/// Same-generation datasets (SG is quadratic in sibling count, so smaller).
pub fn sg_datasets() -> Vec<Dataset> {
    vec![
        Dataset { name: "power-law n=600", graph: power_law(600, 1200, 56) },
        Dataset { name: "grid 24x24", graph: grid(24, 24) },
    ]
}

/// Number of (x, y) pairs with a path of >= 1 edge from x to y, by one BFS
/// per source. Independent of every evaluator in this crate: the oracle the
/// lane-1 sanity lines check the closure size against.
pub fn tc_size_bfs(g: &Graph) -> u64 {
    let out = g.out_csr();
    let mut stamp = vec![u32::MAX; g.n];
    let mut queue: Vec<u32> = Vec::new();
    let mut total = 0u64;
    for s in 0..g.n {
        queue.clear();
        for &v in out.row(s) {
            if stamp[v as usize] != s as u32 {
                stamp[v as usize] = s as u32;
                queue.push(v);
            }
        }
        let mut head = 0;
        while head < queue.len() {
            let u = queue[head] as usize;
            head += 1;
            for &v in out.row(u) {
                if stamp[v as usize] != s as u32 {
                    stamp[v as usize] = s as u32;
                    queue.push(v);
                }
            }
        }
        total += queue.len() as u64;
    }
    total
}
