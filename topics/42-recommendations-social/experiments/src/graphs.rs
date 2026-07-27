//! PROVIDED — two synthetic graphs with ground truth, plus the baselines
//! every recommender has to beat.
//!
//! **The bipartite interaction graph** is Pinterest's and Twitter's
//! shape: users on one side, items (pins / tweets) on the other, edges
//! meaning "engaged with". Two properties are planted because they are
//! the two that matter:
//!
//! * **Community structure** — a user's interests concentrate, so there
//!   is something to personalize *to*. Without it every recommender
//!   collapses to popularity and the topic has no content.
//! * **A power-law popularity tail** — a few items collect most of the
//!   edges. This is what a naive random walk latches onto: the visit
//!   distribution of an unbiased walk converges to something
//!   proportional to degree, so it recommends the globally popular items
//!   to everybody. Pixie §3.1: "In classical random walk low degree
//!   nodes with fewer edges contribute less signal. This is undesirable
//!   because smaller boards ... are more likely to produce highly
//!   relevant recommendations."
//!
//! **The collaboration graph** is Liben-Nowell & Kleinberg's: a
//! unipartite network grown with preferential attachment and triadic
//! closure, split into a training period and a test period, with the
//! test-period edges held out. Their evaluation is reproduced exactly —
//! rank every candidate pair, take the top `|E_new|`, and report the
//! **factor improvement over a random predictor**, because raw accuracy
//! on this task is a fraction of a percent and means nothing on its own.

use rand::seq::SliceRandom;
use rand::Rng;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::collections::{HashMap, HashSet};

pub fn seeded_rng(seed: u64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(seed)
}

// ---------------------------------------------------------------- bipartite

#[derive(Clone, Copy, Debug)]
pub struct BipartiteConfig {
    pub n_users: usize,
    pub n_items: usize,
    pub n_communities: usize,
    pub edges_per_user: usize,
    /// Zipf exponent for item popularity. Higher = a heavier head.
    pub zipf_s: f64,
    /// Fraction of a user's edges drawn from their own community. The
    /// rest are drawn from the global popularity distribution — which is
    /// exactly the noise a recommender has to see through.
    pub community_purity: f64,
    /// Held-out edges per user, used as the prediction target.
    pub holdout_per_user: usize,
    /// How many communities a user draws from. Pinterest users have
    /// several unrelated interests at once — recipes *and* hiking *and*
    /// interior design — and Pixie's multi-hit booster exists precisely
    /// because a candidate reachable from two of them is better evidence
    /// than one reachable twice from one. Set this to 1 and the booster
    /// has nothing to work with; lane 2 measures both.
    pub interests_per_user: usize,
}

impl Default for BipartiteConfig {
    fn default() -> Self {
        BipartiteConfig {
            n_users: 3_000,
            n_items: 6_000,
            n_communities: 30,
            edges_per_user: 20,
            zipf_s: 1.1,
            community_purity: 0.75,
            holdout_per_user: 2,
            interests_per_user: 1,
        }
    }
}

pub struct Bipartite {
    pub n_users: usize,
    pub n_items: usize,
    /// user → items engaged with (training edges only).
    pub user_adj: Vec<Vec<u32>>,
    /// item → users (training edges only).
    pub item_adj: Vec<Vec<u32>>,
    pub user_community: Vec<u32>,
    pub item_community: Vec<u32>,
    /// (user, item) edges removed from the graph — the answer key.
    pub holdout: Vec<(u32, u32)>,
}

impl Bipartite {
    pub fn item_degree(&self, i: u32) -> usize {
        self.item_adj[i as usize].len()
    }
    pub fn user_degree(&self, u: u32) -> usize {
        self.user_adj[u as usize].len()
    }
    pub fn holdout_for(&self, u: u32) -> Vec<u32> {
        self.holdout
            .iter()
            .filter(|&&(hu, _)| hu == u)
            .map(|&(_, i)| i)
            .collect()
    }
}

/// Zipf weights over `n` ranks, normalized to a cumulative table.
fn zipf_cdf(n: usize, s: f64) -> Vec<f64> {
    let mut c = Vec::with_capacity(n);
    let mut acc = 0.0;
    for k in 1..=n {
        acc += 1.0 / (k as f64).powf(s);
        c.push(acc);
    }
    let total = acc;
    for v in c.iter_mut() {
        *v /= total;
    }
    c
}

fn sample_cdf(rng: &mut ChaCha8Rng, cdf: &[f64]) -> usize {
    let x: f64 = rng.gen();
    match cdf.binary_search_by(|p| p.partial_cmp(&x).unwrap()) {
        Ok(i) => i,
        Err(i) => i.min(cdf.len() - 1),
    }
}

pub fn bipartite_instance(rng: &mut ChaCha8Rng, cfg: &BipartiteConfig) -> Bipartite {
    let mut user_community = vec![0u32; cfg.n_users];
    let mut item_community = vec![0u32; cfg.n_items];
    // Each user's interests: the first is their "primary" community
    // (reported in `user_community`), the rest are equally real.
    let mut user_interests: Vec<Vec<usize>> = Vec::with_capacity(cfg.n_users);
    for u in 0..cfg.n_users {
        let mut set: HashSet<usize> = HashSet::new();
        while set.len() < cfg.interests_per_user.max(1).min(cfg.n_communities) {
            set.insert(rng.gen_range(0..cfg.n_communities));
        }
        let mut v: Vec<usize> = set.into_iter().collect();
        v.sort_unstable();
        user_community[u] = v[0] as u32;
        user_interests.push(v);
    }
    // Items are partitioned by community, in contiguous blocks so a
    // community's items can be indexed directly.
    let per_comm = cfg.n_items / cfg.n_communities;
    let mut comm_items: Vec<Vec<u32>> = vec![Vec::new(); cfg.n_communities];
    for i in 0..cfg.n_items {
        let c = (i / per_comm).min(cfg.n_communities - 1);
        item_community[i] = c as u32;
        comm_items[c].push(i as u32);
    }

    // Global popularity ranking: a random permutation of items, ranked
    // by a Zipf law. The head is what a naive recommender finds.
    let mut global_rank: Vec<u32> = (0..cfg.n_items as u32).collect();
    global_rank.shuffle(rng);
    let global_cdf = zipf_cdf(cfg.n_items, cfg.zipf_s);
    let local_cdf = zipf_cdf(per_comm.max(1), cfg.zipf_s);

    let mut user_adj: Vec<Vec<u32>> = vec![Vec::new(); cfg.n_users];
    let mut holdout: Vec<(u32, u32)> = Vec::new();

    for u in 0..cfg.n_users {
        let mut chosen: HashSet<u32> = HashSet::new();
        let want = cfg.edges_per_user + cfg.holdout_per_user;
        let mut guard = 0;
        while chosen.len() < want && guard < want * 20 {
            guard += 1;
            let c = user_interests[u][rng.gen_range(0..user_interests[u].len())];
            let item = if rng.gen::<f64>() < cfg.community_purity && !comm_items[c].is_empty() {
                let r = sample_cdf(rng, &local_cdf).min(comm_items[c].len() - 1);
                comm_items[c][r]
            } else {
                global_rank[sample_cdf(rng, &global_cdf)]
            };
            chosen.insert(item);
        }
        let mut items: Vec<u32> = chosen.into_iter().collect();
        items.sort_unstable();
        items.shuffle(rng);
        // The last `holdout_per_user` engagements are the answer key.
        for _ in 0..cfg.holdout_per_user.min(items.len().saturating_sub(1)) {
            let it = items.pop().unwrap();
            holdout.push((u as u32, it));
        }
        items.sort_unstable();
        user_adj[u] = items;
    }

    let mut item_adj: Vec<Vec<u32>> = vec![Vec::new(); cfg.n_items];
    for (u, items) in user_adj.iter().enumerate() {
        for &i in items {
            item_adj[i as usize].push(u as u32);
        }
    }

    Bipartite {
        n_users: cfg.n_users,
        n_items: cfg.n_items,
        user_adj,
        item_adj,
        user_community,
        item_community,
        holdout,
    }
}

/// The baseline nobody can skip: rank items by global degree. It is
/// identical for every user, which is both its weakness and — on a
/// power-law graph — a surprisingly high hit rate.
pub fn popularity_topk(g: &Bipartite, k: usize) -> Vec<u32> {
    let mut items: Vec<u32> = (0..g.n_items as u32).collect();
    items.sort_by_key(|&i| std::cmp::Reverse(g.item_degree(i)));
    items.truncate(k);
    items
}

/// Pixie's Algorithm 1: the *basic* random walk. From a single query
/// item, repeatedly step item → user → item, counting visits. No bias,
/// no multi-hit boost, no early stopping — this is what lane 2 improves
/// on, and lane 1 shows what it does wrong.
pub fn basic_random_walk(
    rng: &mut ChaCha8Rng,
    g: &Bipartite,
    query: u32,
    steps: usize,
    alpha: f64,
) -> HashMap<u32, u32> {
    let mut visits: HashMap<u32, u32> = HashMap::new();
    let mut cur = query;
    for _ in 0..steps {
        let users = &g.item_adj[cur as usize];
        if users.is_empty() {
            cur = query;
            continue;
        }
        let u = users[rng.gen_range(0..users.len())];
        let items = &g.user_adj[u as usize];
        if items.is_empty() {
            cur = query;
            continue;
        }
        cur = items[rng.gen_range(0..items.len())];
        *visits.entry(cur).or_insert(0) += 1;
        // Restart, so the walk stays near the query (Pixie's α).
        if rng.gen::<f64>() < alpha {
            cur = query;
        }
    }
    visits
}

/// Top-k items by visit count, excluding anything the user already
/// engaged with (recommending what someone already has is free hit rate
/// and zero value).
pub fn topk_from_visits(visits: &HashMap<u32, u32>, exclude: &[u32], k: usize) -> Vec<u32> {
    let ex: HashSet<u32> = exclude.iter().copied().collect();
    let mut v: Vec<(u32, u32)> = visits
        .iter()
        .filter(|(i, _)| !ex.contains(i))
        .map(|(&i, &c)| (i, c))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.into_iter().take(k).map(|(i, _)| i).collect()
}

/// Fraction of users for whom at least one held-out item appears in the
/// top-k. The standard "did we get it right" number.
pub fn hit_rate(g: &Bipartite, recs: &HashMap<u32, Vec<u32>>) -> f64 {
    let mut hits = 0usize;
    let mut total = 0usize;
    for (&u, list) in recs {
        let held: HashSet<u32> = g.holdout_for(u).into_iter().collect();
        if held.is_empty() {
            continue;
        }
        total += 1;
        if list.iter().any(|i| held.contains(i)) {
            hits += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64
    }
}

/// Mean overlap between each user's recommendation list and the global
/// popularity top-k. 1.0 means "you built a bestseller list".
pub fn popularity_overlap(recs: &HashMap<u32, Vec<u32>>, pop: &[u32]) -> f64 {
    let p: HashSet<u32> = pop.iter().copied().collect();
    let mut acc = 0.0;
    let mut n = 0.0;
    for list in recs.values() {
        if list.is_empty() {
            continue;
        }
        acc += list.iter().filter(|i| p.contains(i)).count() as f64 / list.len() as f64;
        n += 1.0;
    }
    if n == 0.0 {
        0.0
    } else {
        acc / n
    }
}

/// 1 − mean pairwise Jaccard between recommendation lists. 0.0 means
/// everybody gets the same list; 1.0 means no two users share an item.
pub fn personalization(recs: &HashMap<u32, Vec<u32>>, sample: usize) -> f64 {
    let lists: Vec<&Vec<u32>> = recs.values().take(sample).collect();
    let mut acc = 0.0;
    let mut n = 0.0;
    for i in 0..lists.len() {
        for j in (i + 1)..lists.len() {
            let a: HashSet<u32> = lists[i].iter().copied().collect();
            let b: HashSet<u32> = lists[j].iter().copied().collect();
            let inter = a.intersection(&b).count() as f64;
            let union = a.union(&b).count() as f64;
            if union > 0.0 {
                acc += inter / union;
                n += 1.0;
            }
        }
    }
    if n == 0.0 {
        1.0
    } else {
        1.0 - acc / n
    }
}

// ------------------------------------------------------------- collaboration

#[derive(Clone, Copy, Debug)]
pub struct CollabConfig {
    pub n_nodes: usize,
    /// Edges added during the training period.
    pub train_edges: usize,
    /// Edges added during the test period — the prediction target.
    pub test_edges: usize,
    pub n_communities: usize,
    /// Probability that a new edge closes a triangle rather than being
    /// drawn by preferential attachment. Triadic closure is *why*
    /// common-neighbour measures work at all.
    pub triadic_closure: f64,
    /// Probability that an edge stays inside a community.
    pub community_purity: f64,
    /// Nodes must have at least this training degree to be in `core`,
    /// mirroring the paper's κ_training = 3.
    pub core_degree: usize,
}

impl Default for CollabConfig {
    fn default() -> Self {
        CollabConfig {
            n_nodes: 800,
            train_edges: 4_000,
            test_edges: 1_000,
            n_communities: 12,
            triadic_closure: 0.9,
            community_purity: 0.8,
            core_degree: 3,
        }
    }
}

pub struct Collab {
    pub n_nodes: usize,
    /// The training graph.
    pub adj: Vec<HashSet<u32>>,
    pub community: Vec<u32>,
    /// Test-period edges between core nodes, absent from `adj`.
    pub new_edges: Vec<(u32, u32)>,
    /// Nodes with training degree ≥ `core_degree`.
    pub core: Vec<u32>,
}

impl Collab {
    pub fn degree(&self, v: u32) -> usize {
        self.adj[v as usize].len()
    }
    pub fn neighbors(&self, v: u32) -> &HashSet<u32> {
        &self.adj[v as usize]
    }
    /// Every unordered pair of core nodes not already joined in the
    /// training graph — the candidate set a predictor must rank.
    pub fn candidates(&self) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for i in 0..self.core.len() {
            for j in (i + 1)..self.core.len() {
                let (a, b) = (self.core[i], self.core[j]);
                if !self.adj[a as usize].contains(&b) {
                    out.push((a, b));
                }
            }
        }
        out
    }
}

fn grow(
    rng: &mut ChaCha8Rng,
    adj: &mut Vec<HashSet<u32>>,
    community: &[u32],
    cfg: &CollabConfig,
    count: usize,
    out: &mut Vec<(u32, u32)>,
    record: bool,
) {
    let n = adj.len();
    let mut added = 0usize;
    let mut guard = 0usize;
    while added < count && guard < count * 50 {
        guard += 1;
        let u = rng.gen_range(0..n) as u32;
        let v = if rng.gen::<f64>() < cfg.triadic_closure && !adj[u as usize].is_empty() {
            // Triadic closure: pick a neighbour of a neighbour.
            let ns: Vec<u32> = adj[u as usize].iter().copied().collect();
            let w = ns[rng.gen_range(0..ns.len())];
            let ws: Vec<u32> = adj[w as usize].iter().copied().collect();
            if ws.is_empty() {
                continue;
            }
            ws[rng.gen_range(0..ws.len())]
        } else {
            // Preferential attachment, biased toward the community.
            let mut cand = rng.gen_range(0..n) as u32;
            for _ in 0..3 {
                let alt = rng.gen_range(0..n) as u32;
                if adj[alt as usize].len() > adj[cand as usize].len() {
                    cand = alt;
                }
            }
            if rng.gen::<f64>() < cfg.community_purity {
                let mut tries = 0;
                while community[cand as usize] != community[u as usize] && tries < 20 {
                    cand = rng.gen_range(0..n) as u32;
                    tries += 1;
                }
            }
            cand
        };
        if u == v || adj[u as usize].contains(&v) {
            continue;
        }
        if record {
            out.push((u.min(v), u.max(v)));
        } else {
            adj[u as usize].insert(v);
            adj[v as usize].insert(u);
        }
        added += 1;
    }
}

pub fn collab_instance(rng: &mut ChaCha8Rng, cfg: &CollabConfig) -> Collab {
    let mut community = vec![0u32; cfg.n_nodes];
    for v in 0..cfg.n_nodes {
        community[v] = rng.gen_range(0..cfg.n_communities) as u32;
    }
    let mut adj: Vec<HashSet<u32>> = vec![HashSet::new(); cfg.n_nodes];
    // Seed so preferential attachment has something to prefer.
    for v in 0..cfg.n_nodes {
        let w = rng.gen_range(0..cfg.n_nodes) as u32;
        if w != v as u32 {
            adj[v].insert(w);
            adj[w as usize].insert(v as u32);
        }
    }
    let mut sink = Vec::new();
    grow(rng, &mut adj, &community, cfg, cfg.train_edges, &mut sink, false);

    let core: Vec<u32> = (0..cfg.n_nodes as u32)
        .filter(|&v| adj[v as usize].len() >= cfg.core_degree)
        .collect();
    let core_set: HashSet<u32> = core.iter().copied().collect();

    let mut new_edges = Vec::new();
    grow(
        rng,
        &mut adj,
        &community,
        cfg,
        cfg.test_edges,
        &mut new_edges,
        true,
    );
    new_edges.retain(|&(a, b)| {
        core_set.contains(&a) && core_set.contains(&b) && !adj[a as usize].contains(&b)
    });
    new_edges.sort_unstable();
    new_edges.dedup();

    Collab {
        n_nodes: cfg.n_nodes,
        adj,
        community,
        new_edges,
        core,
    }
}

/// Liben-Nowell & Kleinberg's evaluation, exactly: rank the candidate
/// pairs by score, take the top `n = |E_new|`, count how many are real,
/// and divide by what a random predictor would have got.
///
/// Reporting the raw accuracy would be pointless — in the paper it runs
/// between 0.147% and 0.475%.
pub struct PredictorScore {
    pub hits: usize,
    pub n: usize,
    pub random_accuracy: f64,
    pub factor_over_random: f64,
}

pub fn evaluate(g: &Collab, scored: &mut Vec<((u32, u32), f64)>) -> PredictorScore {
    let truth: HashSet<(u32, u32)> = g.new_edges.iter().copied().collect();
    let n = truth.len();
    let candidates = g.candidates().len();
    let random_accuracy = n as f64 / candidates as f64;
    // Deterministic tie-breaking, so two predictors that assign the same
    // score to everything are compared fairly.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let hits = scored
        .iter()
        .take(n)
        .filter(|(p, _)| truth.contains(p))
        .count();
    let accuracy = hits as f64 / n.max(1) as f64;
    PredictorScore {
        hits,
        n,
        random_accuracy,
        factor_over_random: accuracy / random_accuracy.max(f64::MIN_POSITIVE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bipartite_graph_has_a_popularity_tail_and_communities() {
        let mut rng = seeded_rng(1);
        let g = bipartite_instance(&mut rng, &BipartiteConfig::default());
        let mut degs: Vec<usize> = (0..g.n_items as u32).map(|i| g.item_degree(i)).collect();
        degs.sort_unstable_by(|a, b| b.cmp(a));
        let total: usize = degs.iter().sum();
        let head: usize = degs.iter().take(g.n_items / 100).sum();
        assert!(
            head as f64 / total as f64 > 0.05,
            "top 1% of items hold only {:.1}% of edges",
            100.0 * head as f64 / total as f64
        );
        // And a user's engagements really do concentrate in their community.
        let u = 0u32;
        let c = g.user_community[u as usize];
        let same = g.user_adj[u as usize]
            .iter()
            .filter(|&&i| g.item_community[i as usize] == c)
            .count();
        assert!(same * 2 > g.user_adj[u as usize].len());
    }

    #[test]
    fn the_collaboration_graph_holds_out_real_future_edges() {
        let mut rng = seeded_rng(2);
        let g = collab_instance(&mut rng, &CollabConfig::default());
        assert!(g.new_edges.len() > 100, "{} test edges", g.new_edges.len());
        for &(a, b) in &g.new_edges {
            assert!(!g.adj[a as usize].contains(&b), "test edge leaked into train");
        }
        // The task has to be hard: a random guess must be near-hopeless.
        let cands = g.candidates().len();
        let r = g.new_edges.len() as f64 / cands as f64;
        assert!(r < 0.02, "random accuracy {r} — task is too easy");
    }
}
