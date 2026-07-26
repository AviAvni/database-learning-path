//! PROVIDED — a synthetic review graph that makes naive fraud detection's
//! failure measurable: camouflage.
//!
//! FRAUDAR's setting (KDD'16). A bipartite graph of users reviewing
//! objects, with a Zipf-skewed background (a few power users, a few hit
//! products) and an injected fraud block: a group of paid accounts all
//! reviewing the same customers' obscure products. The fraudsters then
//! add *camouflage* — extra reviews of genuinely popular products,
//! exactly what a real fraudster with full knowledge of the detector
//! would buy.
//!
//! Two naive detectors, both of which fail:
//! - degree ranking ("flag the most active accounts") never works —
//!   honest power users out-review any economical fraud account;
//! - obscurity ranking ("flag active accounts that only touch unpopular
//!   products") works perfectly until camouflage, then collapses —
//!   the score is a *row* property, and the fraudster controls his row.
//!
//! fraudar.rs restores detection with a *column*-weighted density metric
//! the fraudster cannot touch: his camouflage lands on popular columns
//! that are downweighted by 1/log(degree), and the fraud block's own
//! columns never change.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::HashSet;

pub fn seeded_rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

/// Zipf(s) sampler over ranks 0..n (rank 0 = most popular).
pub struct Zipf {
    cum: Vec<f64>,
}

impl Zipf {
    pub fn new(n: usize, s: f64) -> Self {
        let mut cum = Vec::with_capacity(n);
        let mut total = 0.0;
        for r in 1..=n {
            total += 1.0 / (r as f64).powf(s);
            cum.push(total);
        }
        Zipf { cum }
    }

    pub fn sample(&self, rng: &mut impl Rng) -> usize {
        let x = rng.gen::<f64>() * self.cum.last().unwrap();
        self.cum.partition_point(|&c| c < x)
    }
}

pub struct ReviewGraph {
    pub n_users: usize,
    pub n_objects: usize,
    /// (user, object), deduplicated.
    pub edges: Vec<(usize, usize)>,
    /// user -> object ids reviewed.
    pub user_adj: Vec<Vec<usize>>,
    /// object -> global in-degree (column degree).
    pub obj_deg: Vec<usize>,
    /// Ground truth: the injected block.
    pub fraud_users: Vec<usize>,
    pub fraud_objects: Vec<usize>,
}

pub struct FraudConfig {
    pub n_users: usize,
    pub n_objects: usize,
    pub background_edges: usize,
    pub block_users: usize,
    pub block_objects: usize,
    pub block_density: f64,
    /// Camouflage edges per fraud edge, per fraud user (biased to
    /// popular objects — the hardest naive case in the paper).
    pub camo_ratio: f64,
}

impl Default for FraudConfig {
    fn default() -> Self {
        FraudConfig {
            n_users: 2_000,
            n_objects: 2_000,
            background_edges: 20_000,
            block_users: 20,
            block_objects: 80,
            block_density: 1.0,
            camo_ratio: 0.0,
        }
    }
}

/// Build a graph: Zipf(0.7) x Zipf(0.8) background, fraud block on
/// fresh node ids (users n_users.., objects n_objects..), then
/// camouflage edges from each fraud user to Zipf(1.5)-sampled (i.e.
/// popularity-biased) honest objects.
pub fn fraud_instance(rng: &mut impl Rng, cfg: &FraudConfig) -> ReviewGraph {
    let n_users = cfg.n_users + cfg.block_users;
    let n_objects = cfg.n_objects + cfg.block_objects;
    let mut seen = HashSet::new();
    let mut edges = Vec::new();

    // Background: Zipf-skewed on both sides (rank = node id); flatter
    // than Zipf(1) so the popular core stays plausibly sparse relative
    // to the fraud block (real review data has this shape — FRAUDAR's
    // Amazon subset has avg degree ~2).
    let zu = Zipf::new(cfg.n_users, 0.7);
    let zo = Zipf::new(cfg.n_objects, 0.8);
    let mut attempts = 0usize;
    while edges.len() < cfg.background_edges && attempts < cfg.background_edges * 10 {
        attempts += 1;
        let u = zu.sample(rng);
        let o = zo.sample(rng);
        if seen.insert((u, o)) {
            edges.push((u, o));
        }
    }

    // Fraud block: dense bipartite core on dedicated nodes.
    let mut fraud_edges_per_user = vec![0usize; cfg.block_users];
    for bu in 0..cfg.block_users {
        for bo in 0..cfg.block_objects {
            if rng.gen::<f64>() < cfg.block_density {
                let (u, o) = (cfg.n_users + bu, cfg.n_objects + bo);
                if seen.insert((u, o)) {
                    edges.push((u, o));
                    fraud_edges_per_user[bu] += 1;
                }
            }
        }
    }

    // Camouflage: popularity-biased edges into the honest columns
    // (steeper Zipf than the background — the paper's "biased
    // camouflage": a fraudster buys reviews of the *hits*).
    let zc = Zipf::new(cfg.n_objects, 1.5);
    for bu in 0..cfg.block_users {
        let u = cfg.n_users + bu;
        let want = (fraud_edges_per_user[bu] as f64 * cfg.camo_ratio).round() as usize;
        let mut placed = 0usize;
        let mut tries = 0usize;
        while placed < want && tries < want * 10 + 10 {
            tries += 1;
            let o = zc.sample(rng);
            if seen.insert((u, o)) {
                edges.push((u, o));
                placed += 1;
            }
        }
    }

    let mut user_adj = vec![Vec::new(); n_users];
    let mut obj_deg = vec![0usize; n_objects];
    for &(u, o) in &edges {
        user_adj[u].push(o);
        obj_deg[o] += 1;
    }

    ReviewGraph {
        n_users,
        n_objects,
        edges,
        user_adj,
        obj_deg,
        fraud_users: (cfg.n_users..n_users).collect(),
        fraud_objects: (cfg.n_objects..n_objects).collect(),
    }
}

/// Precision@|fraud_users| of flagging the highest-degree users.
pub fn degree_rank_precision(g: &ReviewGraph) -> f64 {
    top_k_precision(g, |_, adj| adj.len() as f64)
}

/// "Active account that only reviews obscure products": among users with
/// >= 10 reviews, flag the lowest mean column popularity. A row score —
/// the fraudster controls it, so camouflage kills it.
pub fn obscurity_rank_precision(g: &ReviewGraph) -> f64 {
    top_k_precision(g, |g, adj| {
        if adj.len() < 10 {
            return f64::MIN;
        }
        let mean: f64 =
            adj.iter().map(|&o| g.obj_deg[o] as f64).sum::<f64>() / adj.len() as f64;
        -mean
    })
}

/// Precision of the top-|fraud_users| users under `score` (higher =
/// more suspicious).
pub fn top_k_precision(g: &ReviewGraph, score: impl Fn(&ReviewGraph, &[usize]) -> f64) -> f64 {
    let k = g.fraud_users.len();
    let mut order: Vec<usize> = (0..g.n_users).collect();
    order.sort_by(|&a, &b| {
        score(g, &g.user_adj[b]).total_cmp(&score(g, &g.user_adj[a]))
    });
    let fraud: HashSet<usize> = g.fraud_users.iter().copied().collect();
    order[..k].iter().filter(|u| fraud.contains(u)).count() as f64 / k as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_shape_is_as_advertised() {
        let mut rng = seeded_rng(1);
        let cfg = FraudConfig {
            camo_ratio: 1.0,
            ..FraudConfig::default()
        };
        let g = fraud_instance(&mut rng, &cfg);
        assert_eq!(g.fraud_users.len(), 20);
        assert_eq!(g.fraud_objects.len(), 80);
        // Density 1.0: all 20 * 80 = 1600 fraud edges, plus ~1
        // camouflage edge per fraud edge (modulo dedup misses).
        let fraud_edges = g
            .edges
            .iter()
            .filter(|&&(u, o)| u >= cfg.n_users && o >= cfg.n_objects)
            .count();
        assert_eq!(fraud_edges, 1600);
        let camo_edges = g
            .edges
            .iter()
            .filter(|&&(u, o)| u >= cfg.n_users && o < cfg.n_objects)
            .count();
        assert!(
            (fraud_edges as f64 * 0.8..=fraud_edges as f64 * 1.05)
                .contains(&(camo_edges as f64)),
            "{camo_edges} camouflage for {fraud_edges} fraud edges"
        );
    }

    #[test]
    fn degree_ranking_never_finds_economical_fraud() {
        // Honest power users out-review the fraud accounts. (Camouflage
        // *raises* the fraudsters' degree profile — see the bench table
        // for how the two heuristics fail in opposite regimes.)
        let mut rng = seeded_rng(2);
        let g = fraud_instance(&mut rng, &FraudConfig::default());
        let p = degree_rank_precision(&g);
        assert!(p < 0.3, "degree precision {p}");
    }

    #[test]
    fn obscurity_ranking_dies_to_camouflage() {
        let mut rng = seeded_rng(3);
        let clean = fraud_instance(&mut rng, &FraudConfig::default());
        let p0 = obscurity_rank_precision(&clean);
        assert!(p0 > 0.7, "no camouflage: obscurity precision {p0}");

        let camo = fraud_instance(
            &mut rng,
            &FraudConfig {
                camo_ratio: 2.0,
                ..FraudConfig::default()
            },
        );
        let p2 = obscurity_rank_precision(&camo);
        assert!(p2 < 0.3, "camo 2x: obscurity precision {p2}");
    }
}
