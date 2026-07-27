use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use social_experiments::graphs::{
    basic_random_walk, bipartite_instance, collab_instance, evaluate, hit_rate, personalization,
    popularity_overlap, popularity_topk, seeded_rng, topk_from_visits, Bipartite, BipartiteConfig,
    Collab, CollabConfig,
};
use social_experiments::linkpred::{
    adamic_adar, common_neighbors, jaccard, preferential_attachment, score_all,
};
use social_experiments::pixie::{
    allocate_steps, multi_hit_boost, overlap, pixie_walk, topk, walk_per_query,
};

const K: usize = 50;
const SAMPLE_USERS: usize = 300;

fn bipartite() -> Bipartite {
    bipartite_with(1)
}

fn bipartite_with(interests: usize) -> Bipartite {
    let mut rng = seeded_rng(42);
    bipartite_instance(
        &mut rng,
        &BipartiteConfig {
            interests_per_user: interests,
            ..BipartiteConfig::default()
        },
    )
}

/// Lane 1 (PROVIDED): popularity is a strong baseline, and a naive walk
/// mostly reproduces it.
fn lane1_popularity_trap() {
    println!("== lane 1: the popularity trap ==");
    let g = bipartite();
    println!(
        "   {} users x {} items, {} communities, {} training edges, {} held out",
        g.n_users,
        g.n_items,
        30,
        g.user_adj.iter().map(|v| v.len()).sum::<usize>(),
        g.holdout.len()
    );
    let pop = popularity_topk(&g, K);

    // Baseline 1: give everybody the bestseller list.
    let mut pop_recs: HashMap<u32, Vec<u32>> = HashMap::new();
    for u in 0..SAMPLE_USERS as u32 {
        let own: std::collections::HashSet<u32> = g.user_adj[u as usize].iter().copied().collect();
        pop_recs.insert(
            u,
            popularity_topk(&g, K * 3)
                .into_iter()
                .filter(|i| !own.contains(i))
                .take(K)
                .collect(),
        );
    }

    // Baseline 2: Pixie's Algorithm 1 — an unbiased walk from one of the
    // user's items, no boosting, no weighting.
    let mut rng = seeded_rng(7);
    let mut walk_recs: HashMap<u32, Vec<u32>> = HashMap::new();
    for u in 0..SAMPLE_USERS as u32 {
        let q = g.user_adj[u as usize][0];
        let v = basic_random_walk(&mut rng, &g, q, 20_000, 0.3);
        walk_recs.insert(u, topk_from_visits(&v, &g.user_adj[u as usize], K));
    }

    println!("   recommender          hit-rate@{K}   personalization   overlap w/ bestsellers");
    for (name, recs) in [("popularity", &pop_recs), ("basic walk", &walk_recs)] {
        println!(
            "   {name:<18}   {:>9.3}   {:>15.3}   {:>21.3}",
            hit_rate(&g, recs),
            personalization(recs, 60),
            popularity_overlap(recs, &pop)
        );
    }
    println!();
    println!("   Popularity is not a weak baseline — on a power-law graph it gets");
    println!("   a third of users right, and its personalization score is only");
    println!("   nonzero at all because we filter out items each user already has.");
    println!("   Everybody is being handed the same bestseller list. The basic random");
    println!("   walk personalizes, but a large slice of what it returns is just");
    println!("   the bestseller list again, because an unbiased walk's visit");
    println!("   distribution drifts toward degree. That is exactly Pixie's");
    println!("   complaint: \"low degree nodes with fewer edges contribute less");
    println!("   signal ... smaller boards are more likely to produce highly");
    println!("   relevant recommendations\". Lane 2 fixes it.");
    println!();
}

/// Lane 2 (needs pixie.rs): the four Pixie innovations, measured.
fn lane2_pixie() {
    println!("== lane 2: Pixie — weighted query set, multi-hit boost, early stopping ==");
    for interests in [1usize, 3] {
        lane2_for(interests);
    }
    println!("   Two results, and one of them is negative.");
    println!();
    println!("   EARLY STOPPING WORKS, and by roughly the margin the paper claims:");
    println!("   ~35% of the steps, ~2.2x faster, and the top-50 still overlaps the");
    println!("   full walk by ~0.79 with hit rate unchanged. Pixie reports 84%");
    println!("   overlap at a third of the runtime; this is the same trade.");
    println!();
    println!("   THE MULTI-HIT BOOSTER DOES NOT HELP HERE — not at one interest per");
    println!("   user and not at three. Summing raw visit counts scores slightly");
    println!("   BETTER than Equation 3 in both regimes. That is not a bug in the");
    println!("   implementation (the unit test pins the arithmetic exactly); it is");
    println!("   the generator failing to contain the booster's premise. Equation 3");
    println!("   is a bet that a pin sitting at the intersection of several of your");
    println!("   interests is more engaging than one deep inside a single interest.");
    println!("   That is a claim about people, not about graphs, and this generator");
    println!("   draws its held-out item from the same distribution as the training");
    println!("   items — so reachability from several query pins carries no extra");
    println!("   information about the answer. Exercise 4 asks you to build a graph");
    println!("   where the premise does hold, and to find how strong the effect has");
    println!("   to be before the boost pays for itself. The lesson is the one worth");
    println!("   keeping: a published trick encodes a domain assumption, and you owe");
    println!("   it a measurement on YOUR data before you ship it.");
    println!();
    println!("   Pixie's own numbers, for scale: 1B boards + 2B pins + 17B edges");
    println!("   in ~120 GB on one machine, p99 under 60 ms, ~1,200 requests/s per");
    println!("   server. Cost depends on steps, not on graph size — which is the");
    println!("   only reason \"rank 3 billion items in 60 ms\" is a sentence you can");
    println!("   say. Their early stopping (n_p=2000, n_v=4) keeps 84% of the");
    println!("   gold-standard top-1000 at a third of the runtime.");
    println!();
}

fn lane2_for(interests: usize) {
    let g = bipartite_with(interests);
    let pop = popularity_topk(&g, K);
    let steps_budget = 30_000usize;
    println!("   -- {interests} interest(s) per user --");

    let run = |early: Option<(usize, usize)>| {
        let mut rng = seeded_rng(7);
        let mut recs: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut steps = 0usize;
        let t = Instant::now();
        for u in 0..SAMPLE_USERS as u32 {
            let own = &g.user_adj[u as usize];
            let q: Vec<(u32, f64)> = own.iter().take(8).map(|&i| (i, 1.0)).collect();
            let r = pixie_walk(&mut rng, &g, &q, steps_budget, 0.3, early);
            steps += r.steps_taken;
            recs.insert(u, topk(&r.scores, own, K));
        }
        (recs, steps, t.elapsed().as_secs_f64())
    };

    // Ablation: the same 8 query pins and the same step budget, but
    // scores are plain summed visit counts instead of multi-hit boosted.
    // This isolates what Equation 3 is actually worth.
    let mut rng = seeded_rng(7);
    let mut noboost: HashMap<u32, Vec<u32>> = HashMap::new();
    for u in 0..SAMPLE_USERS as u32 {
        let own = &g.user_adj[u as usize];
        let q: Vec<(u32, f64)> = own.iter().take(8).map(|&i| (i, 1.0)).collect();
        let steps = allocate_steps(&g, &q, steps_budget);
        let per = walk_per_query(&mut rng, &g, &q, &steps, 0.3);
        let mut summed: HashMap<u32, f64> = HashMap::new();
        for m in &per {
            for (&i, &v) in m {
                *summed.entry(i).or_insert(0.0) += v as f64;
            }
        }
        let _ = multi_hit_boost(&per);
        noboost.insert(u, topk(&summed, own, K));
    }

    let (full, full_steps, full_secs) = run(None);
    let (early, early_steps, early_secs) = run(Some((100, 3)));

    println!("   recommender          hit-rate@{K}   personalization   overlap w/ bestsellers");
    for (name, recs) in [
        ("8 pins, no boost", &noboost),
        ("pixie (full)", &full),
        ("pixie (early stop)", &early),
    ] {
        println!(
            "   {name:<18}   {:>9.3}   {:>15.3}   {:>21.3}",
            hit_rate(&g, recs),
            personalization(recs, 60),
            popularity_overlap(recs, &pop)
        );
    }
    println!();
    println!(
        "   full walk:   {full_steps} steps, {:.2} ms/query",
        full_secs * 1e3 / SAMPLE_USERS as f64
    );
    println!(
        "   early stop:  {early_steps} steps ({:.0}% of full), {:.2} ms/query ({:.1}x faster)",
        100.0 * early_steps as f64 / full_steps as f64,
        early_secs * 1e3 / SAMPLE_USERS as f64,
        full_secs / early_secs
    );
    let mean_overlap: f64 = (0..SAMPLE_USERS as u32)
        .map(|u| overlap(&early[&u], &full[&u]))
        .sum::<f64>()
        / SAMPLE_USERS as f64;
    println!("   early-stopped top-{K} overlaps the full walk by {mean_overlap:.3}");
    println!();
}

/// Lane 3 (needs linkpred.rs): the classical proximity measures.
fn lane3_linkpred() {
    println!("== lane 3: link prediction — factor improvement over random ==");
    let mut rng = seeded_rng(4);
    let g: Collab = collab_instance(&mut rng, &CollabConfig::default());
    let cands = g.candidates().len();
    println!(
        "   {} nodes, {} core, {} candidate pairs, {} held-out future edges",
        g.n_nodes,
        g.core.len(),
        cands,
        g.new_edges.len()
    );
    println!(
        "   a random predictor is correct {:.3}% of the time\n",
        100.0 * g.new_edges.len() as f64 / cands as f64
    );
    println!("   predictor                  hits / n     factor over random");
    for (name, f) in [
        (
            "preferential attachment",
            preferential_attachment as fn(&Collab, u32, u32) -> f64,
        ),
        ("common neighbors", common_neighbors),
        ("Jaccard", jaccard),
        ("Adamic/Adar", adamic_adar),
    ] {
        let t = Instant::now();
        let mut s = score_all(&g, f);
        let r = evaluate(&g, &mut s);
        println!(
            "   {name:<24}   {:>4} / {:<5}   {:>10.1}x   ({:.0} ms)",
            r.hits,
            r.n,
            r.factor_over_random,
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    println!();
    println!("   Preferential attachment is the popularity baseline in disguise —");
    println!("   |degree(x)| * |degree(y)|, never once asking whether x and y have");
    println!("   anything in common — and it loses to every measure that does.");
    println!("   Liben-Nowell & Kleinberg measured the same ordering on five arXiv");
    println!("   co-authorship networks: preferential attachment 4.7-15.2x over");
    println!("   random where common neighbors reached 18.0-47.2x and Adamic/Adar");
    println!("   16.8-54.8x. Adamic/Adar is common neighbors with each shared");
    println!("   neighbour discounted by 1/log(degree) — the same idf idea as");
    println!("   topic 23's inverse document frequency and topic 39's FRAUDAR");
    println!("   column weights.");
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_popularity_trap();
    stub_lane("lane 2", lane2_pixie);
    stub_lane("lane 3", lane3_linkpred);
}
