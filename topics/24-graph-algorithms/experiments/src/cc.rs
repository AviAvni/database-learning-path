//! Connected components: union-find oracle (PROVIDED) vs Afforest
//! (STUB) — the GAP CC winner that gets away with looking at only a
//! FEW edges per vertex.

use crate::graph::Csr;

/// Path-halving union-find over all edges — the boring exact oracle.
pub fn cc_unionfind(g: &Csr) -> Vec<u32> {
    let mut parent: Vec<u32> = (0..g.n as u32).collect();
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            parent[x as usize] = parent[parent[x as usize] as usize];
            x = parent[x as usize];
        }
        x
    }
    for u in 0..g.n {
        for &v in g.neigh(u) {
            let (ru, rv) = (find(&mut parent, u as u32), find(&mut parent, v));
            if ru != rv {
                let (lo, hi) = (ru.min(rv), ru.max(rv));
                parent[hi as usize] = lo;
            }
        }
    }
    (0..g.n as u32).map(|v| find(&mut parent, v)).collect()
}

/// Canonicalize labels so two labelings compare equal iff they induce
/// the same partition: relabel each component by its minimum vertex.
pub fn canonical(labels: &[u32]) -> Vec<u32> {
    let mut min_of = vec![u32::MAX; labels.len()];
    for (v, &l) in labels.iter().enumerate() {
        min_of[l as usize] = min_of[l as usize].min(v as u32);
    }
    labels.iter().map(|&l| min_of[l as usize]).collect()
}

/// STUB — Afforest (Sutton et al.; gapbs cc.cc:95). Union-find
/// asymptotics are fine; the win is SKIPPING most edge inspections:
///
/// 1. Neighbor rounds (cc.cc:106): for r in 0..neighbor_rounds,
///    every vertex Links (union by min, with compress-on-find) only
///    its r-th neighbor. Two rounds already glue the giant component
///    together on skewed graphs.
/// 2. Compress (cc.cc:59) then sample ~1024 vertices' labels and
///    find the most frequent component c (cc.cc:69
///    `SampleFrequentElement`).
/// 3. Final sweep (cc.cc:129): ONLY vertices whose label ≠ c process
///    their REMAINING neighbors (offset neighbor_rounds..deg) — the
///    giant component's vertices are never touched again.
/// 4. Compress; return labels + `edges_inspected` (count every
///    neighbor actually read in steps 1 and 3).
///
/// Contract: same partition as `cc_unionfind` (compare canonical
/// forms); on RMAT, edges_inspected ≪ m.
pub fn afforest(_g: &Csr, _neighbor_rounds: usize) -> (Vec<u32>, usize) {
    todo!("neighbor-round linking + frequent-component sampling + selective sweep")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{from_edges, gen_rmat, gen_uniform};

    #[test]
    fn unionfind_two_islands() {
        let g = from_edges(5, &[(0, 1, 1), (1, 2, 1), (3, 4, 1)], true);
        let c = canonical(&cc_unionfind(&g));
        assert_eq!(c, vec![0, 0, 0, 3, 3]);
    }

    #[test]
    fn afforest_matches_unionfind_rmat() {
        let (n, e) = gen_rmat(11, 8, 21);
        let g = from_edges(n, &e, true);
        let (labels, inspected) = afforest(&g, 2);
        assert_eq!(canonical(&labels), canonical(&cc_unionfind(&g)));
        assert!(
            inspected * 2 < g.m(),
            "afforest looked at {inspected} of {} edges — sampling not working",
            g.m()
        );
    }

    #[test]
    fn afforest_matches_unionfind_uniform() {
        // uniform graphs have no giant-component shortcut at low
        // degree — correctness must not depend on the skew
        let e = gen_uniform(2000, 3000, 22);
        let g = from_edges(2000, &e, true);
        let (labels, _) = afforest(&g, 2);
        assert_eq!(canonical(&labels), canonical(&cc_unionfind(&g)));
    }
}
