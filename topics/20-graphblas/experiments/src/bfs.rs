//! BFS three ways — push (SpMSpV), pull (masked SpMV over AT with
//! early exit), direction-optimizing (Beamer SC'12 / the LAGraph
//! template's switch, reading-beamer-sc12.md + reading-lagraph.md).

use crate::csr::Csr;

/// Oracle: textbook queue BFS. Levels; -1 = unreachable.
pub fn bfs_scalar(a: &Csr, src: u32) -> Vec<i64> {
    let mut level = vec![-1i64; a.n];
    level[src as usize] = 0;
    let mut q = vec![src];
    let mut l = 0;
    while !q.is_empty() {
        let mut next = Vec::new();
        for &u in &q {
            for &v in a.row(u as usize).0 {
                if level[v as usize] < 0 {
                    level[v as usize] = l + 1;
                    next.push(v);
                }
            }
        }
        q = next;
        l += 1;
    }
    level
}

/// Per-level trace for the direction-optimizing stub — gb_bench
/// prints these so the switch decision is visible.
#[derive(Debug, Clone)]
pub struct LevelTrace {
    pub level: usize,
    pub frontier: usize,
    pub used_pull: bool,
    pub edges_checked: usize,
}

/// STUB — push step: next = (frontier × A) minus visited.
///   frontier: sparse Vec<u32>; visited: dense bitmap (Vec<bool> ok).
///   edges_checked += out_degree of every frontier vertex.
///   This is SpMSpV over (ANY, PAIR): claim once, order irrelevant.
pub fn bfs_push(a: &Csr, src: u32) -> (Vec<i64>, Vec<LevelTrace>) {
    let _ = (a, src);
    todo!("push BFS: frontier-driven scatter (see module docs)")
}

/// STUB — pull step over AT (A transposed): every UNVISITED vertex
///   scans its in-edges (AT row), stops at FIRST frontier hit —
///   the ANY-monoid early exit. Needs frontier as a dense bitmap.
///   edges_checked += probes actually made (count the early exit!).
pub fn bfs_pull(at: &Csr, src: u32) -> (Vec<i64>, Vec<LevelTrace>) {
    let _ = (at, src);
    todo!("pull BFS: unvisited-driven gather with early exit")
}

/// STUB — direction-optimizing: start push; switch to pull when
///   frontier is growing AND (|frontier| > n/beta1 OR push-work
///   estimate (Σ out-degrees of frontier) > edges_unexplored/alpha);
///   switch back when |frontier| < n/beta2. LAGraph ships
///   alpha=8, beta1=8, beta2=512 (template :184-187).
///   Maintain edges_unexplored incrementally (:196,:261-277).
pub fn bfs_diropt(
    a: &Csr,
    at: &Csr,
    src: u32,
    alpha: f64,
    beta1: f64,
    beta2: f64,
) -> (Vec<i64>, Vec<LevelTrace>) {
    let _ = (a, at, src, alpha, beta1, beta2);
    todo!("direction-optimizing BFS (see module docs)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csr::{path, rmat, uniform};

    #[test]
    fn scalar_on_path() {
        let a = path(5);
        assert_eq!(bfs_scalar(&a, 0), vec![0, 1, 2, 3, 4]);
        assert_eq!(bfs_scalar(&a, 3), vec![-1, -1, -1, 0, 1]);
    }

    fn check_matches_oracle(f: impl Fn(&Csr, &Csr, u32) -> Vec<i64>) {
        for (g, src) in [
            (rmat(9, 8, 2), 0u32),
            (uniform(500, 2000, 3), 7),
            (path(64), 0),
        ] {
            let at = g.transpose();
            assert_eq!(f(&g, &at, src), bfs_scalar(&g, src));
        }
    }

    #[test]
    fn push_matches_oracle() {
        check_matches_oracle(|g, _at, s| bfs_push(g, s).0);
    }

    #[test]
    fn pull_matches_oracle() {
        check_matches_oracle(|g, at, s| bfs_pull(at, s).0);
    }

    #[test]
    fn diropt_matches_oracle_and_never_pulls_on_path() {
        check_matches_oracle(|g, at, s| bfs_diropt(g, at, s, 8.0, 8.0, 512.0).0);
        let p = path(512);
        let pt = p.transpose();
        let (_, trace) = bfs_diropt(&p, &pt, 0, 8.0, 8.0, 512.0);
        assert!(trace.iter().all(|t| !t.used_pull), "path graph must stay push");
    }
}
