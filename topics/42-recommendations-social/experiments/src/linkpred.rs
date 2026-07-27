//! Link prediction: four one-line scores, and why the obvious one loses.
//!
//! Liben-Nowell & Kleinberg surveyed the proximity measures and ran them
//! all on five arXiv co-authorship networks. The headline is not that
//! one measure wins — "there is no single clear winner among the
//! techniques" — but that **network topology alone carries real signal**:
//! the best measures beat a random predictor by 20–55×, on a task where
//! random is correct 0.147%–0.475% of the time.
//!
//! Their Figure 3, as factor improvement over random:
//!
//! ```text
//!   predictor                astro-ph  cond-mat  gr-qc  hep-ph  hep-th
//!   random is correct          0.475%    0.147%  0.341%  0.207%  0.153%
//!   graph distance               9.6      25.3    21.4    12.2    29.2
//!   common neighbors            18.0      41.1    27.2    27.0    47.2
//!   preferential attachment      4.7       6.1     7.6    15.2     7.5   <-- worst
//!   Adamic/Adar                 16.8      54.8    30.1    33.3    50.5
//!   Jaccard                     16.4      42.3    19.9    27.7    41.7
//!   rooted PageRank α=0.15      16.6      41.1    27.2    27.6    42.6
//!   Katz (weighted) β=0.005     13.4      54.8    30.1    24.0    52.2
//! ```
//!
//! Preferential attachment is the interesting row. It scores
//! `|Γ(x)|·|Γ(y)|` — pure degree, no shared structure at all — and it is
//! the *worst* of the neighbourhood family on four of the five networks.
//! It is the popularity baseline of lane 1 wearing a link-prediction
//! costume, and it loses for the same reason: knowing who is famous is
//! not knowing who will connect.
//!
//! Adamic/Adar is the counterweight. It is common neighbours with each
//! shared neighbour discounted by `1/log|Γ(z)|`, so a mutual friend who
//! knows everybody counts for almost nothing and a mutual friend who
//! knows three people counts for a lot. Same idf intuition as topic 39's
//! FRAUDAR column weights and topic 23's inverse document frequency —
//! three fields, one idea.

use crate::graphs::Collab;

/// `|Γ(x) ∩ Γ(y)|` — the most direct implementation of "friends of
/// friends become friends".
///
/// (STUB.)
pub fn common_neighbors(g: &Collab, x: u32, y: u32) -> f64 {
    let _ = (g, x, y);
    todo!(
        "count the shared neighbours of x and y. Iterate the smaller neighbour set and probe the larger - the same asymmetry topic 40's galloping intersect exploits."
    )
}

/// `|Γ(x) ∩ Γ(y)| / |Γ(x) ∪ Γ(y)|` — common neighbours, normalized, so
/// two hubs with 50 mutual friends out of 5,000 do not outrank two
/// specialists with 5 out of 6.
///
/// (STUB.)
pub fn jaccard(g: &Collab, x: u32, y: u32) -> f64 {
    let _ = (g, x, y);
    todo!(
        "intersection over union of the two neighbour sets; return 0.0 when the union is empty."
    )
}

/// `Σ_{z ∈ Γ(x) ∩ Γ(y)} 1 / log|Γ(z)|` — rare shared neighbours count
/// for more. Guard `|Γ(z)| ≤ 1`, where the log is zero or negative.
///
/// (STUB.)
pub fn adamic_adar(g: &Collab, x: u32, y: u32) -> f64 {
    let _ = (g, x, y);
    todo!(
        "sum 1/ln(degree(z)) over the shared neighbours z, skipping any z of degree <= 1 so the log stays positive."
    )
}

/// `|Γ(x)| · |Γ(y)|` — the degree-only measure. Included because it is
/// the one people reach for, and because watching it lose is the point.
///
/// (STUB.)
pub fn preferential_attachment(g: &Collab, x: u32, y: u32) -> f64 {
    let _ = (g, x, y);
    todo!(
        "the product of the two degrees. One line - and note that it never looks at whether x and y have anything in common."
    )
}

/// Score every candidate pair with `f`, ready for `graphs::evaluate`.
pub fn score_all(
    g: &Collab,
    f: impl Fn(&Collab, u32, u32) -> f64,
) -> Vec<((u32, u32), f64)> {
    g.candidates()
        .into_iter()
        .map(|(a, b)| ((a, b), f(g, a, b)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::{collab_instance, evaluate, seeded_rng, CollabConfig};

    fn small() -> Collab {
        let mut rng = seeded_rng(4);
        collab_instance(
            &mut rng,
            &CollabConfig {
                n_nodes: 400,
                train_edges: 2_500,
                test_edges: 600,
                ..CollabConfig::default()
            },
        )
    }

    #[test]
    fn the_measures_agree_on_a_hand_built_case() {
        // x and y share two neighbours: one hub (high degree) and one
        // specialist. Common neighbours cannot tell them apart;
        // Adamic/Adar must weight the specialist far more heavily.
        let mut adj: Vec<std::collections::HashSet<u32>> = vec![Default::default(); 110];
        let mut link = |a: u32, b: u32, adj: &mut Vec<std::collections::HashSet<u32>>| {
            adj[a as usize].insert(b);
            adj[b as usize].insert(a);
        };
        // 0 = x, 1 = y, 2 = specialist (degree 2), 3 = hub (degree 100).
        link(0, 2, &mut adj);
        link(1, 2, &mut adj);
        link(0, 3, &mut adj);
        link(1, 3, &mut adj);
        for v in 10..108u32 {
            link(3, v, &mut adj);
        }
        let g = Collab {
            n_nodes: 110,
            adj,
            community: vec![0; 110],
            new_edges: vec![],
            core: (0..110).collect(),
        };
        assert_eq!(common_neighbors(&g, 0, 1), 2.0);
        let aa = adamic_adar(&g, 0, 1);
        let specialist = 1.0 / (2f64).ln();
        let hub = 1.0 / (100f64).ln();
        assert!((aa - (specialist + hub)).abs() < 1e-9, "adamic/adar = {aa}");
        assert!(specialist > 5.0 * hub, "the discount is not doing any work");
        assert_eq!(preferential_attachment(&g, 0, 1), 4.0);
        assert!((jaccard(&g, 0, 1) - 2.0 / 2.0).abs() < 1e-9);
    }

    #[test]
    fn topology_beats_random_by_an_order_of_magnitude() {
        let g = small();
        for (name, f) in [
            ("common", common_neighbors as fn(&Collab, u32, u32) -> f64),
            ("jaccard", jaccard),
            ("adamic", adamic_adar),
        ] {
            let mut s = score_all(&g, f);
            let r = evaluate(&g, &mut s);
            assert!(
                r.factor_over_random > 5.0,
                "{name} only {}x better than random",
                r.factor_over_random
            );
        }
    }

    #[test]
    fn degree_alone_is_the_weakest_measure() {
        // Preferential attachment is the popularity baseline in
        // disguise: it never checks whether the two nodes have anything
        // in common. It must lose to every measure that does.
        let g = small();
        let mut pa = score_all(&g, preferential_attachment);
        let mut cn = score_all(&g, common_neighbors);
        let mut aa = score_all(&g, adamic_adar);
        let pa = evaluate(&g, &mut pa).factor_over_random;
        let cn = evaluate(&g, &mut cn).factor_over_random;
        let aa = evaluate(&g, &mut aa).factor_over_random;
        assert!(pa < cn, "preferential attachment {pa} vs common neighbors {cn}");
        assert!(pa < aa, "preferential attachment {pa} vs adamic/adar {aa}");
    }

    #[test]
    fn discounting_hubs_helps() {
        // Adamic/Adar is common neighbours plus one idea, and the idea
        // has to earn its place.
        let g = small();
        let mut cn = score_all(&g, common_neighbors);
        let mut aa = score_all(&g, adamic_adar);
        let cn = evaluate(&g, &mut cn).factor_over_random;
        let aa = evaluate(&g, &mut aa).factor_over_random;
        assert!(aa >= cn, "adamic/adar {aa} did not beat common neighbors {cn}");
    }
}
