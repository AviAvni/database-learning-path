//! Betweenness centrality. PROVIDED: per-source BFS (depths + path
//! counts) and a brute-force all-pairs oracle straight from the
//! definition. STUB: Brandes' dependency accumulation — the O(V·E)
//! trick that made BC computable.
//!
//! Convention: unweighted shortest paths, DIRECTED ordered pairs
//! (s,t), endpoints excluded: bc(v) = Σ_{s≠v≠t} σ_st(v)/σ_st.
//! On a symmetric CSR this double-counts each undirected pair —
//! fine, as long as oracle and Brandes agree.

use crate::graph::Csr;
use std::collections::VecDeque;

/// BFS returning (depth, sigma): depth[v] = hop distance (-1 if
/// unreachable), sigma[v] = number of shortest s→v paths.
pub fn bfs_sigma(g: &Csr, s: u32) -> (Vec<i32>, Vec<f64>) {
    let mut depth = vec![-1i32; g.n];
    let mut sigma = vec![0f64; g.n];
    depth[s as usize] = 0;
    sigma[s as usize] = 1.0;
    let mut q = VecDeque::from([s]);
    while let Some(u) = q.pop_front() {
        for &v in g.neigh(u as usize) {
            let (v, u) = (v as usize, u as usize);
            if depth[v] == -1 {
                depth[v] = depth[u] + 1;
                q.push_back(v as u32);
            }
            if depth[v] == depth[u] + 1 {
                sigma[v] += sigma[u];
            }
        }
    }
    (depth, sigma)
}

/// Brute-force oracle from the definition: v is on an s→t shortest
/// path iff d(s,v)+d(v,t) = d(s,t), contributing σ_sv·σ_vt/σ_st.
/// O(n²) memory, O(n³) time — small graphs only.
pub fn bc_brute(g: &Csr) -> Vec<f64> {
    assert!(g.n <= 512, "oracle is O(n^3)");
    let per_source: Vec<(Vec<i32>, Vec<f64>)> = (0..g.n).map(|s| bfs_sigma(g, s as u32)).collect();
    let mut bc = vec![0f64; g.n];
    for s in 0..g.n {
        let (ds, sig_s) = &per_source[s];
        for t in 0..g.n {
            if t == s || ds[t] < 0 {
                continue;
            }
            let sigma_st = sig_s[t];
            for v in 0..g.n {
                if v == s || v == t || ds[v] < 0 {
                    continue;
                }
                let (dv, sig_v) = &per_source[v];
                if dv[t] >= 0 && ds[v] + dv[t] == ds[t] {
                    bc[v] += sig_s[v] * sig_v[t] / sigma_st;
                }
            }
        }
    }
    bc
}

/// STUB — Brandes '01 (gapbs bc.cc:51 PBFS + back-propagation;
/// LAGraph LAGr_Betweenness.c does the same with a MATRIX frontier
/// of multiple sources at once).
///
/// Per source s:
/// 1. Forward BFS recording depth, sigma, AND the vertices grouped
///    by depth (gapbs keeps a `depth_index` into the BFS queue —
///    reuse `bfs_sigma` then bucket vertices by depth, or write the
///    queue-based version).
/// 2. delta[v] = 0. Walk depths DEEPEST FIRST: for each v at depth d
///    and each neighbor w at depth d+1 (a BFS-DAG successor):
///        delta[v] += sigma[v]/sigma[w] · (1 + delta[w])
///    (gapbs stores a `succ` bitmap during BFS to test the
///    depth[w]==depth[v]+1 condition without recomputing).
/// 3. bc[v] += delta[v] for v ≠ s.
///
/// `sources = None` ⇒ all sources (exact, matches `bc_brute`);
/// `Some(k sources)` ⇒ the GAP-style sampled approximation.
pub fn brandes(_g: &Csr, _sources: Option<&[u32]>) -> Vec<f64> {
    todo!("forward BFS + deepest-first dependency accumulation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{from_edges, gen_rmat};

    #[test]
    fn sigma_counts_paths() {
        // diamond: 0→{1,2}→3 : two shortest 0→3 paths
        let g = from_edges(4, &[(0, 1, 1), (0, 2, 1), (1, 3, 1), (2, 3, 1)], true);
        let (d, s) = bfs_sigma(&g, 0);
        assert_eq!(d[3], 2);
        assert_eq!(s[3], 2.0);
    }

    #[test]
    fn brute_path_graph() {
        // 0-1-2: only the middle vertex carries paths (0→2 and 2→0)
        let g = from_edges(3, &[(0, 1, 1), (1, 2, 1)], true);
        assert_eq!(bc_brute(&g), vec![0.0, 2.0, 0.0]);
    }

    #[test]
    fn brandes_matches_brute() {
        let (n, e) = gen_rmat(7, 6, 9); // n=128
        let g = from_edges(n, &e, true);
        let brute = bc_brute(&g);
        let fast = brandes(&g, None);
        for v in 0..n {
            assert!(
                (brute[v] - fast[v]).abs() < 1e-6 * brute[v].max(1.0),
                "v={v}: brute {} vs brandes {}",
                brute[v],
                fast[v]
            );
        }
    }

    #[test]
    fn brandes_sampled_runs() {
        let (n, e) = gen_rmat(9, 8, 10);
        let g = from_edges(n, &e, true);
        let bc = brandes(&g, Some(&[0, 7, 99]));
        assert_eq!(bc.len(), n);
        assert!(bc.iter().all(|&x| x >= 0.0));
        assert!(bc.iter().any(|&x| x > 0.0));
    }
}
