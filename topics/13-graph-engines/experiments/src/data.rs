//! Provided: seeded preferential-attachment (Barabási–Albert) graph.
//!
//! Each new node attaches to `m` targets sampled from the endpoint
//! list (endpoints appear once per incident edge, so sampling uniform
//! over the list IS degree-proportional). Edges are symmetrized —
//! (u,v) and (v,u) both present — so the power-law tail shows up in
//! adjacency-row lengths, then sorted + deduped.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub struct EdgeList {
    pub num_nodes: u32,
    /// directed, sorted by (src, dst), deduped, no self-loops
    pub edges: Vec<(u32, u32)>,
}

pub fn preferential_attachment(n: u32, m: u32, seed: u64) -> EdgeList {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut edges: Vec<(u32, u32)> = Vec::with_capacity(2 * (n as usize) * (m as usize));
    let mut endpoints: Vec<u32> = Vec::with_capacity(2 * (n as usize) * (m as usize));

    // seed clique over the first m+1 nodes
    let seed_n = m + 1;
    for u in 0..seed_n {
        for v in (u + 1)..seed_n {
            edges.push((u, v));
            edges.push((v, u));
            endpoints.push(u);
            endpoints.push(v);
        }
    }

    for u in seed_n..n {
        for _ in 0..m {
            let v = endpoints[rng.gen_range(0..endpoints.len())];
            if v == u {
                continue;
            }
            edges.push((u, v));
            edges.push((v, u));
            endpoints.push(u);
            endpoints.push(v);
        }
    }

    edges.sort_unstable();
    edges.dedup();
    EdgeList { num_nodes: n, edges }
}

/// Small uniform random graph for tests.
pub fn random_graph(n: u32, num_edges: usize, seed: u64) -> EdgeList {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut edges = Vec::with_capacity(num_edges);
    while edges.len() < num_edges {
        let u = rng.gen_range(0..n);
        let v = rng.gen_range(0..n);
        if u != v {
            edges.push((u, v));
        }
    }
    edges.sort_unstable();
    edges.dedup();
    EdgeList { num_nodes: n, edges }
}
