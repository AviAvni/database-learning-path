//! STUB — a one-pass streaming greedy partitioner (LDG-style,
//! Stanton & Kliot KDD'12).
//!
//! Stream vertices in id order. Place vertex v in the part maximizing
//!
//!     score(i) = |N(v) ∩ P_i| · (1 − |P_i| / C),   C = (1 + slack)·n/k
//!
//! where N(v) ∩ P_i counts already-placed neighbors (one pass: unseen
//! neighbors score 0). A full part (|P_i| ≥ C) scores −∞ — the balance
//! term is a hard capacity, not a suggestion. Break ties toward the
//! lightest part. That's the whole algorithm; it beats random placement
//! by 2× or more wherever the graph has locality.

use crate::graphs::Graph;

/// Returns `assign[v] = part in 0..k` for every vertex.
pub fn greedy_partition(g: &Graph, k: u32, slack: f64) -> Vec<u32> {
    let _ = (g, k, slack);
    todo!("adjacency, then one pass: score each part, place, update counts")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::{community, edge_cut, random_assignment};

    fn test_graph() -> Graph {
        community(8, 500, 8, 2, 7)
    }

    /// The capacity term is a contract: no part may exceed (1+slack)·n/k.
    #[test]
    fn balanced_within_slack() {
        let g = test_graph();
        let assign = greedy_partition(&g, 8, 0.05);
        let mut counts = vec![0usize; 8];
        for &p in &assign {
            counts[p as usize] += 1;
        }
        let cap = (1.05 * g.n as f64 / 8.0).ceil() as usize;
        for (i, &c) in counts.iter().enumerate() {
            assert!(c <= cap, "part {i} holds {c} > cap {cap}");
        }
    }

    /// On a graph with community structure, greedy must cut well below
    /// random's (k−1)/k — the co-location signal is there to be found.
    #[test]
    fn beats_random_on_community_graph() {
        let g = test_graph();
        let rand_cut = edge_cut(&random_assignment(g.n, 8, 99), &g.edges);
        let greedy_cut = edge_cut(&greedy_partition(&g, 8, 0.05), &g.edges);
        assert!(
            greedy_cut < 0.6 * rand_cut,
            "greedy {greedy_cut} vs random {rand_cut}"
        );
    }

    /// Streaming placement must be deterministic: same stream, same
    /// assignment. (Real systems re-derive placement from metadata; a
    /// nondeterministic partitioner can't be re-run.)
    #[test]
    fn deterministic() {
        let g = test_graph();
        assert_eq!(greedy_partition(&g, 8, 0.05), greedy_partition(&g, 8, 0.05));
    }
}
