//! Pixie's random walk: four ideas on top of Algorithm 1.
//!
//! Pinterest serves recommendations from a graph of 1 billion boards,
//! 2 billion pins and 17 billion edges held in about 120 GB of RAM on a
//! single machine, at a **99th-percentile latency under 60 ms**, and one
//! server handles ~1,200 requests/s. The reason it can is that the whole
//! algorithm is a random walk whose cost depends on the number of steps
//! and *not on the size of the graph* — the one property that makes
//! "recommend from 3 billion items in real time" a tractable sentence.
//!
//! Four innovations over the basic walk (§3.1):
//!
//! 1. **Biasing** the edge choice by user features (language, topic), so
//!    the same query set gives different results to different users.
//! 2. **Multiple weighted query pins**, with the step budget allocated
//!    *sub-linearly* in query-pin degree.
//! 3. **The multi-hit booster**, which rewards candidates reached from
//!    several query pins rather than many times from one.
//! 4. **Early stopping**, which halves the number of steps.
//!
//! This module asks you to implement 2, 3 and 4. Biasing is exercise 2 —
//! it is the one that needs a user-feature model, and the paper's own
//! measurement of it is the sharpest number in the whole evaluation
//! (English→Slovak target-language content goes from **2.13% under a
//! basic walk to 42.55% under Pixie's**).

use crate::graphs::Bipartite;
#[allow(unused_imports)] // you will need this once the walks are implemented
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use std::collections::HashMap;

/// Per-query-pin visit counters, before the multi-hit boost. Index is
/// the query pin's position in the query set.
pub type PerQueryVisits = Vec<HashMap<u32, u32>>;

/// How many steps each query pin gets.
///
/// Pixie Equation 1–2. The problem: a high-degree query pin needs more
/// steps to say anything (its walk diffuses), but allocating steps
/// *linearly* in degree starves low-degree pins of even a single step.
/// The fix is a scaling factor that grows sub-linearly:
///
/// ```text
///   s_q = |E(q)| · (C − log|E(q)|)      C = max_p log|E(p)|
///   N_q = w_q · s_q / Σ_r s_r           scaled by the pin's weight
/// ```
///
/// (STUB.)
pub fn allocate_steps(g: &Bipartite, query: &[(u32, f64)], total_steps: usize) -> Vec<usize> {
    let _ = (g, query, total_steps);
    todo!(
        "compute C = ln(max item degree in the WHOLE graph) - not just over the query set, or the highest-degree query pin gets s_q = 0. Then s_q = deg_q * (C - ln deg_q) per query pin, and N_q = w_q * s_q / sum(w_r * s_r) * total_steps. Give every query pin at least one step; that guarantee is the whole point of the sub-linear scaling."
    )
}

/// Run one Pixie random walk per query pin and return the per-pin visit
/// counters, unboosted.
///
/// The walk itself is the basic one — item → user → item, restarting at
/// the query pin with probability `alpha` — but each pin gets its own
/// counter, because the multi-hit booster needs to know *which* pin a
/// visit came from.
///
/// (STUB.)
pub fn walk_per_query(
    rng: &mut ChaCha8Rng,
    g: &Bipartite,
    query: &[(u32, f64)],
    steps: &[usize],
    alpha: f64,
) -> PerQueryVisits {
    let _ = (rng, g, query, steps, alpha);
    todo!(
        "for each query pin q with its allocated budget, run the item -> user -> item walk with restart probability alpha, counting visits into that pin's OWN map. Return one map per query pin, in query order."
    )
}

/// **The multi-hit booster** (Pixie Equation 3):
///
/// ```text
///   V[p] = ( Σ_{q ∈ Q} sqrt( V_q[p] ) )²
/// ```
///
/// A pin visited 4 times from one query pin scores 4. A pin visited
/// twice from each of two query pins scores (√2+√2)² = 8. Same total
/// visits, twice the score — because being reachable from several of the
/// user's interests is stronger evidence than being reachable often from
/// one. Note that a single-source pin's score is unchanged, so this is a
/// boost and not a re-weighting.
///
/// (STUB.)
pub fn multi_hit_boost(per_query: &PerQueryVisits) -> HashMap<u32, f64> {
    let _ = per_query;
    todo!(
        "sum sqrt(visits) across the per-query counters for each candidate, then square the sum. A candidate seen from only one query pin must come out with exactly its original visit count."
    )
}

/// Result of a Pixie query.
pub struct PixieResult {
    pub scores: HashMap<u32, f64>,
    /// Steps actually taken, summed over query pins. Early stopping
    /// makes this smaller than the budget.
    pub steps_taken: usize,
}

/// **Early stopping** (Pixie Algorithm 2, lines 10–13).
///
/// Rather than always burning `total_steps`, terminate a walk once at
/// least `n_p` candidate pins have each been visited at least `n_v`
/// times — the cheap proxy for "the top of the ranking has stopped
/// moving". Monitoring this costs one counter, incremented when a pin's
/// visit count crosses `n_v` exactly.
///
/// The paper's measurement: with `n_p = 2000, n_v = 4` the result
/// overlaps the gold-standard long walk by **84%** at **one third** of
/// the runtime; at `n_v = 6` the runtime halves.
///
/// (STUB.)
pub fn pixie_walk(
    rng: &mut ChaCha8Rng,
    g: &Bipartite,
    query: &[(u32, f64)],
    total_steps: usize,
    alpha: f64,
    early_stop: Option<(usize, usize)>,
) -> PixieResult {
    let _ = (rng, g, query, total_steps, alpha, early_stop);
    todo!(
        "allocate_steps, then walk each query pin. With early_stop = Some((n_p, n_v)), each walk keeps its OWN counter of how many pins have reached exactly n_v visits (Algorithm 2's counter is per walk, not per query) and stops that walk once the counter exceeds n_p. Boost with multi_hit_boost and report the steps actually taken."
    )
}

/// Top-k by score, excluding items the user already engaged with.
pub fn topk(scores: &HashMap<u32, f64>, exclude: &[u32], k: usize) -> Vec<u32> {
    let ex: std::collections::HashSet<u32> = exclude.iter().copied().collect();
    let mut v: Vec<(u32, f64)> = scores
        .iter()
        .filter(|(i, _)| !ex.contains(i))
        .map(|(&i, &s)| (i, s))
        .collect();
    v.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    v.into_iter().take(k).map(|(i, _)| i).collect()
}

/// Fraction of `a`'s top-k that also appears in `b`'s.
pub fn overlap(a: &[u32], b: &[u32]) -> f64 {
    if a.is_empty() {
        return 0.0;
    }
    let bs: std::collections::HashSet<u32> = b.iter().copied().collect();
    a.iter().filter(|i| bs.contains(i)).count() as f64 / a.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::{bipartite_instance, seeded_rng, BipartiteConfig};

    fn small() -> Bipartite {
        let mut rng = seeded_rng(5);
        bipartite_instance(
            &mut rng,
            &BipartiteConfig {
                n_users: 600,
                n_items: 1_200,
                n_communities: 10,
                community_purity: 0.9,
                ..BipartiteConfig::default()
            },
        )
    }

    #[test]
    fn multi_hit_beats_single_hit_at_equal_total_visits() {
        // The booster's whole claim, as arithmetic. Pin 1 is seen four
        // times from one query pin; pin 2 twice from each of two.
        let per_query: PerQueryVisits = vec![
            HashMap::from([(1u32, 4u32), (2u32, 2u32)]),
            HashMap::from([(2u32, 2u32)]),
        ];
        let s = multi_hit_boost(&per_query);
        assert!((s[&1] - 4.0).abs() < 1e-9, "single-source score changed: {}", s[&1]);
        assert!((s[&2] - 8.0).abs() < 1e-9, "multi-source score wrong: {}", s[&2]);
        assert!(s[&2] > s[&1]);
    }

    #[test]
    fn every_query_pin_gets_at_least_one_step() {
        // The failure mode Equation 1 exists to prevent: allocate
        // linearly in degree and a low-degree pin gets zero steps, so a
        // whole interest of the user is silently dropped.
        let g = small();
        let mut query: Vec<(u32, f64)> = Vec::new();
        let mut lo = None;
        let mut hi = None;
        for i in 0..g.n_items as u32 {
            let d = g.item_degree(i);
            if d == 1 && lo.is_none() {
                lo = Some(i);
            }
            if d > 30 && hi.is_none() {
                hi = Some(i);
            }
        }
        query.push((lo.expect("no degree-1 item"), 1.0));
        query.push((hi.expect("no high-degree item"), 1.0));
        let steps = allocate_steps(&g, &query, 10_000);
        assert!(steps[0] >= 1, "low-degree pin got {} steps", steps[0]);
        assert!(steps[1] > steps[0], "high-degree pin should get more");
        // Sub-linear: the ratio of steps must be far below the ratio of
        // degrees.
        let deg_ratio = g.item_degree(query[1].0) as f64 / g.item_degree(query[0].0) as f64;
        let step_ratio = steps[1] as f64 / steps[0] as f64;
        assert!(
            step_ratio < deg_ratio,
            "allocation is not sub-linear: steps {step_ratio} vs degrees {deg_ratio}"
        );
    }

    #[test]
    fn early_stopping_keeps_the_top_of_the_ranking() {
        // Pixie's claim: almost the same results in about half the steps.
        let g = small();
        let mut rng = seeded_rng(9);
        let query: Vec<(u32, f64)> = g.user_adj[7][..4.min(g.user_adj[7].len())]
            .iter()
            .map(|&i| (i, 1.0))
            .collect();
        let full = pixie_walk(&mut rng, &g, &query, 200_000, 0.3, None);
        let mut rng = seeded_rng(9);
        let early = pixie_walk(&mut rng, &g, &query, 200_000, 0.3, Some((200, 4)));
        assert!(
            early.steps_taken < full.steps_taken,
            "early stopping took {} of {} steps",
            early.steps_taken,
            full.steps_taken
        );
        let o = overlap(&topk(&early.scores, &[], 100), &topk(&full.scores, &[], 100));
        assert!(o >= 0.7, "early-stopped top-100 overlaps the full walk by only {o}");
    }

    #[test]
    fn the_walk_is_personalized() {
        // Two users from different communities must not get the same
        // list. If they do, you have built a bestseller chart.
        let g = small();
        let mut a = None;
        let mut b = None;
        for u in 0..g.n_users as u32 {
            if g.user_adj[u as usize].len() < 4 {
                continue;
            }
            if a.is_none() {
                a = Some(u);
            } else if g.user_community[u as usize] != g.user_community[a.unwrap() as usize] {
                b = Some(u);
                break;
            }
        }
        let (a, b) = (a.unwrap(), b.unwrap());
        let mut rng = seeded_rng(3);
        let mut list_for = |u: u32, rng: &mut ChaCha8Rng| {
            let q: Vec<(u32, f64)> = g.user_adj[u as usize][..4].iter().map(|&i| (i, 1.0)).collect();
            let r = pixie_walk(rng, &g, &q, 60_000, 0.3, None);
            topk(&r.scores, &g.user_adj[u as usize], 50)
        };
        let la = list_for(a, &mut rng);
        let lb = list_for(b, &mut rng);
        assert!(
            overlap(&la, &lb) < 0.5,
            "two users in different communities share {:.0}% of their top-50",
            100.0 * overlap(&la, &lb)
        );
    }
}
