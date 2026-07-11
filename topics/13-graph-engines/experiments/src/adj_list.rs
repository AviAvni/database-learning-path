//! Provided: the oracle. `Vec<Vec<u32>>` adjacency — the layout M13's
//! first graph core will use. Per-node vectors are contiguous (fine
//! for one expand) but the OUTER structure is pointer-y: every
//! `adj[n1]` in the hop-2 loop is a fresh Vec header load, then a
//! fresh heap buffer — two dependent misses before the data streams.

pub struct AdjList {
    pub adj: Vec<Vec<u32>>,
}

impl AdjList {
    pub fn build(num_nodes: u32, edges: &[(u32, u32)]) -> AdjList {
        let mut adj = vec![Vec::new(); num_nodes as usize];
        for &(u, v) in edges {
            adj[u as usize].push(v);
        }
        AdjList { adj }
    }

    /// Distinct nodes at distance 1 or 2 from `src`, excluding `src`.
    /// `seen`/`stamp`: reusable visited set (see lib.rs).
    pub fn two_hop(&self, src: u32, seen: &mut [u32], stamp: u32) -> u64 {
        let mut count = 0u64;
        seen[src as usize] = stamp;
        for &n1 in &self.adj[src as usize] {
            if seen[n1 as usize] != stamp {
                seen[n1 as usize] = stamp;
                count += 1;
            }
        }
        for &n1 in &self.adj[src as usize] {
            for &n2 in &self.adj[n1 as usize] {
                if seen[n2 as usize] != stamp {
                    seen[n2 as usize] = stamp;
                    count += 1;
                }
            }
        }
        count
    }
}
