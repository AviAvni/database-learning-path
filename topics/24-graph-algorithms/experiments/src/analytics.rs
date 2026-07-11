//! PROVIDED baselines: pull PageRank (gapbs pr.cc) and degree-ordered
//! triangle counting (gapbs tc.cc / LAGraph's Sandia L·L∘L in scalar
//! clothing).

use crate::graph::Csr;

pub const DAMP: f64 = 0.85;

/// Pull-based PR on a symmetric graph (in-neigh == out-neigh).
/// Returns (scores, iterations). L1 error convergence like gapbs
/// pr.cc:44-57.
pub fn pagerank(g: &Csr, epsilon: f64, max_iters: usize) -> (Vec<f64>, usize) {
    let n = g.n as f64;
    let base = (1.0 - DAMP) / n;
    let mut scores = vec![1.0 / n; g.n];
    let mut contrib = vec![0f64; g.n];
    for iter in 1..=max_iters {
        for u in 0..g.n {
            let d = g.degree(u);
            contrib[u] = if d > 0 { scores[u] / d as f64 } else { 0.0 };
        }
        let mut error = 0.0;
        for u in 0..g.n {
            let incoming: f64 = g.neigh(u).iter().map(|&v| contrib[v as usize]).sum();
            let new = base + DAMP * incoming;
            error += (new - scores[u]).abs();
            scores[u] = new;
        }
        if error < epsilon {
            return (scores, iter);
        }
    }
    (scores, max_iters)
}

/// Degree-ordered triangle count: orient each edge from lower-rank
/// to higher-rank endpoint (rank = degree order), then two-pointer
/// intersect forward-adjacency lists. Each triangle counted exactly
/// once — gapbs tc.cc:52 OrderedCount + the RelabelByDegree
/// heuristic (:75) rolled into one.
pub fn triangle_count(g: &Csr) -> u64 {
    let mut rank = vec![0u32; g.n];
    let mut order: Vec<u32> = (0..g.n as u32).collect();
    order.sort_unstable_by_key(|&v| (g.degree(v as usize), v));
    for (r, &v) in order.iter().enumerate() {
        rank[v as usize] = r as u32;
    }
    // forward adjacency: neighbors with higher rank, sorted by rank
    let mut fwd: Vec<Vec<u32>> = vec![Vec::new(); g.n];
    for u in 0..g.n {
        for &v in g.neigh(u) {
            if rank[v as usize] > rank[u] {
                fwd[u].push(v);
            }
        }
        fwd[u].sort_unstable_by_key(|&v| rank[v as usize]);
    }
    let mut count = 0u64;
    for u in 0..g.n {
        for &v in &fwd[u] {
            // |fwd[u] ∩ fwd[v]| by rank-ordered two-pointer
            let (a, b) = (&fwd[u], &fwd[v as usize]);
            let (mut i, mut j) = (0, 0);
            while i < a.len() && j < b.len() {
                let (ra, rb) = (rank[a[i] as usize], rank[b[j] as usize]);
                match ra.cmp(&rb) {
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                    std::cmp::Ordering::Equal => {
                        count += 1;
                        i += 1;
                        j += 1;
                    }
                }
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::from_edges;

    #[test]
    fn pr_sums_to_one_ish() {
        let g = from_edges(4, &[(0, 1, 1), (1, 2, 1), (2, 3, 1), (3, 0, 1)], true);
        let (s, iters) = pagerank(&g, 1e-10, 200);
        assert!((s.iter().sum::<f64>() - 1.0).abs() < 1e-6);
        assert!(iters < 200);
        // symmetric ring: all equal
        assert!(s.windows(2).all(|w| (w[0] - w[1]).abs() < 1e-9));
    }

    #[test]
    fn tc_counts() {
        // triangle 0-1-2 plus a pendant 3
        let g = from_edges(4, &[(0, 1, 1), (1, 2, 1), (0, 2, 1), (2, 3, 1)], true);
        assert_eq!(triangle_count(&g), 1);
        // K4 has 4 triangles
        let k4 = from_edges(
            4,
            &[(0, 1, 1), (0, 2, 1), (0, 3, 1), (1, 2, 1), (1, 3, 1), (2, 3, 1)],
            true,
        );
        assert_eq!(triangle_count(&k4), 4);
    }
}
