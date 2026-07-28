use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use graphrag_experiments::kg::{bfs_rank, mean_rank, mention_rank, random_kg, seeded_rng};
use graphrag_experiments::ppr::{ppr, ppr_rank};
use graphrag_experiments::temporal::TemporalStore;

/// Lane 1 (PROVIDED): the path-finding collapse — mention ranking and
/// BFS distance both go to chance the moment evidence stops co-occurring.
fn lane1_collapse() {
    println!("== lane 1: path-finding collapse — mean rank of the true answer ==");
    println!("   (17 candidates, 8 distractor chains per seed; chance = 9.0, perfect = 1.0)");
    println!("   hops   mention-count   bfs-distance");
    let mut rng = seeded_rng(42);
    for hops in [1usize, 2, 3] {
        let mention = mean_rank(&mut rng, 400, hops, 8, mention_rank);
        let bfs = mean_rank(&mut rng, 400, hops, 8, bfs_rank);
        println!("   {hops}         {mention:>5.2}           {bfs:>5.2}");
    }
    println!();
    println!("   mention ranking = vector RAG's shape: score each passage against");
    println!("   the query independently. At 1 hop the answer is named next to both");
    println!("   seeds and wins; at 2+ hops no passage mentions a query entity and");
    println!("   ranking is chance. BFS covers everything but every candidate sits");
    println!("   at the same depth — coverage without association.");
    println!();
}

/// Lane 2 (needs ppr.rs): PPR restores the ranking, and what it costs.
fn lane2_ppr() {
    println!("== lane 2: personalized PageRank — association as arithmetic ==");
    println!("   hops   mention-count   ppr (damping 0.5, 30 iters)");
    let mut rng = seeded_rng(7);
    for hops in [1usize, 2, 3] {
        let mention = mean_rank(&mut rng, 400, hops, 8, mention_rank);
        let pr = mean_rank(&mut rng, 400, hops, 8, |_, inst| ppr_rank(inst, 0.5, 30));
        println!("   {hops}         {mention:>5.2}           {pr:>5.2}");
    }
    println!();

    // Throughput: one PPR query on a 100k-node graph (HippoRAG runs one
    // of these per query, online).
    let mut rng = seeded_rng(11);
    let kg = random_kg(&mut rng, 100_000, 8);
    let seeds = [0usize, 1, 2];
    let t = Instant::now();
    let runs = 20;
    let mut checksum = 0.0;
    for _ in 0..runs {
        let pi = ppr(&kg, &seeds, 0.5, 30);
        checksum += pi[0];
    }
    let dt = t.elapsed().as_secs_f64() / runs as f64;
    println!(
        "   one PPR query, 100k nodes / ~400k directed edges, 30 iters: {:.1} ms  (checksum {checksum:.3})",
        dt * 1e3
    );
    println!();
}

/// Lane 3 (needs temporal.rs): the bi-temporal store under a stream of
/// job changes — audit trail growth and as-of query cost.
fn lane3_temporal() {
    println!("== lane 3: bi-temporal store — nothing deleted, any moment answerable ==");
    const WORKS_AT: u32 = 0;
    for people in [1_000u32, 10_000] {
        let changes = 10u64; // job changes per person
        let mut store = TemporalStore::new();
        let mut t = 0u64;
        for c in 0..changes {
            for p in 0..people {
                t += 1;
                store.ingest(p, WORKS_AT, 100_000 + c as u32, t, t);
            }
        }
        let current = store.current().len();
        let t0 = Instant::now();
        let runs = 100u64;
        let mut total = 0usize;
        for i in 0..runs {
            total += store.as_of(t / 2 + i, t).len();
        }
        let dt = t0.elapsed().as_secs_f64() / runs as f64 * 1e3;
        println!(
            "   {people:>6} entities x {changes} changes: {} edges kept, {current} current, as-of scan {dt:.2} ms ({} rows)",
            store.edges.len(),
            total / runs as usize
        );
    }
    println!();
    println!("   every superseded edge stays (t_invalid + t_expired set); the");
    println!("   current view is 1/10th of the store, and any past state — event");
    println!("   time OR ingestion time — is one filter away.");
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce()) {
    // Silence the default panic hook for the duration of the lane: an
    // unimplemented exercise is an expected state, not a crash to report.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    if r.is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_collapse();
    stub_lane("lane 2", lane2_ppr);
    stub_lane("lane 3", lane3_temporal);
}
