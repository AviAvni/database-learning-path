//! crdt_bench — four lanes.
//!
//! Lane 1 (PROVIDED, runs today): LWW's lie. Two replicas write to a
//! shared LWW map under varying contention and sync intervals; we
//! count writes silently discarded at merge. This is the number
//! "eventual consistency with LWW" hides from you.
//!
//! Lanes 2-4 need your implementations (they todo!-panic until then):
//!   2. convergence storm — N replicas, random OR-Set ops, random
//!      gossip; measure rounds to convergence + metadata overhead.
//!   3. RGA editing trace — sequential typing + concurrent bursts;
//!      throughput and tombstone bloat.
//!   4. graph dangling storm — concurrent node-removes vs edge-adds;
//!      count hidden edges and prove resurrection works.

use crdt_experiments::clock::ReplicaId;
use crdt_experiments::graph::GraphCrdt;
use crdt_experiments::lww::LwwMap;
use crdt_experiments::orset::OrSet;
use crdt_experiments::rga::Rga;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn main() {
    lane1_lww_lie();
    stub_lane("lane 2: OR-Set convergence storm", lane2_orset_storm);
    stub_lane("lane 3: RGA editing trace", lane3_rga_trace);
    stub_lane("lane 4: graph dangling storm", lane4_graph_storm);
}

fn stub_lane(name: &str, f: fn()) {
    println!("\n=== {name} ===");
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s in src/ to unlock this lane]");
    }
}

/// Lane 1: two replicas, `writes` writes each over `keys` keys, syncing
/// every `sync_every` writes. Count writes lost to LWW at merge.
fn lane1_lww_lie() {
    println!("=== lane 1: LWW's lie (lost concurrent writes) ===");
    println!("{:>8} {:>12} {:>10} {:>10} {:>8}", "keys", "sync_every", "writes", "lost", "lost%");
    // Kept small: state-based sync ships the WHOLE map every interval,
    // so sync_every=1 is O(writes * map_size). That cost is itself a
    // finding — real systems ship deltas (delta-CRDTs) for this reason.
    let writes_per_replica = 20_000;
    for &keys in &[10u64, 1_000, 100_000] {
        for &sync_every in &[1usize, 100, 10_000] {
            let mut rng = ChaCha8Rng::seed_from_u64(keys ^ sync_every as u64);
            let mut a: LwwMap<u64, u64> = LwwMap::new();
            let mut b: LwwMap<u64, u64> = LwwMap::new();
            let mut lost = 0usize;
            let mut ts = 0u64;
            for i in 0..writes_per_replica {
                ts += 1;
                a.set(rng.gen_range(0..keys), ts, ts, 1);
                b.set(rng.gen_range(0..keys), ts, ts, 2);
                if (i + 1) % sync_every == 0 {
                    lost += a.merge(&b);
                    lost += b.merge(&a);
                }
            }
            lost += a.merge(&b);
            lost += b.merge(&a);
            let total = 2 * writes_per_replica;
            println!(
                "{:>8} {:>12} {:>10} {:>10} {:>7.2}%",
                keys,
                sync_every,
                total,
                lost,
                100.0 * lost as f64 / total as f64
            );
        }
    }
    println!("(every 'lost' row entry is a user write no replica remembers)");
}

/// Lane 2: N replicas do random add/remove on an OR-Set, gossiping with
/// a random peer each round. Report rounds until all replicas equal and
/// live-dots vs tombstone counts (the garbage that never leaves).
fn lane2_orset_storm() {
    const N: usize = 8;
    const OPS_PER_ROUND: usize = 50;
    const ROUNDS: usize = 100;
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let mut reps: Vec<OrSet<u64>> = (0..N).map(|i| OrSet::new(i as ReplicaId)).collect();
    let start = Instant::now();
    for _ in 0..ROUNDS {
        for r in reps.iter_mut() {
            for _ in 0..OPS_PER_ROUND {
                let e = rng.gen_range(0..1_000u64);
                if rng.gen_bool(0.7) {
                    r.add(e);
                } else {
                    r.remove(&e);
                }
            }
        }
        // random gossip: each replica merges one random peer
        for i in 0..N {
            let j = rng.gen_range(0..N);
            if i != j {
                let peer = reps[j].clone();
                reps[i].merge(&peer);
            }
        }
    }
    // full mesh to force convergence, then verify
    for _ in 0..2 {
        for i in 0..N {
            for j in 0..N {
                if i != j {
                    let peer = reps[j].clone();
                    reps[i].merge(&peer);
                }
            }
        }
    }
    let first = reps[0].elements();
    assert!(reps.iter().all(|r| r.elements() == first), "storm must converge");
    let live: usize = reps[0].elems.values().map(|d| d.len()).sum();
    let dead = reps[0].tombstones.len();
    println!(
        "converged: {} elements, {} live dots, {} tombstones ({}x garbage), {:?}",
        first.len(),
        live,
        dead,
        dead / live.max(1),
        start.elapsed()
    );
}

/// Lane 3: type 50K chars sequentially, then 3 replicas make concurrent
/// bursts; measure insert throughput and tombstone bloat after deletes.
fn lane3_rga_trace() {
    let start = Instant::now();
    let mut a: Rga<char> = Rga::new(1);
    const N: usize = 50_000;
    let mut ops = Vec::with_capacity(N);
    for i in 0..N {
        ops.push(a.insert(i, 'x'));
    }
    let typed = start.elapsed();
    println!(
        "sequential typing: {} chars in {:?} ({:.0} inserts/s)",
        N,
        typed,
        N as f64 / typed.as_secs_f64()
    );

    let mut b: Rga<char> = Rga::new(2);
    let replay = Instant::now();
    for op in &ops {
        b.apply(op);
    }
    println!("remote replay: {:?}", replay.elapsed());

    // delete half, measure visible vs stored
    for _ in 0..N / 2 {
        a.delete(0);
    }
    println!(
        "after deleting half: visible={} stored={} (tombstone bloat {:.1}x)",
        a.to_vec().len(),
        a.elems.len(),
        a.elems.len() as f64 / a.to_vec().len().max(1) as f64
    );
}

/// Lane 4: seed a graph, then concurrent node-removes on one replica vs
/// edge-adds touching those nodes on another. Count hidden edges post-
/// merge, then re-add nodes and count resurrections.
fn lane4_graph_storm() {
    const NODES: u64 = 1_000;
    const EDGES: usize = 5_000;
    let mut rng = ChaCha8Rng::seed_from_u64(7);
    let mut a = GraphCrdt::new(1);
    for n in 0..NODES {
        a.add_node(n);
    }
    for _ in 0..EDGES {
        a.add_edge(rng.gen_range(0..NODES), rng.gen_range(0..NODES));
    }
    let mut b = a.clone();
    b.replica = 2;
    b.nodes.replica = 2;
    b.edges.replica = 2;

    // a removes 100 random nodes; b concurrently adds 500 edges, some
    // touching the doomed nodes.
    let mut doomed: Vec<u64> = (0..NODES).collect();
    doomed.shuffle(&mut rng);
    doomed.truncate(100);
    for &n in &doomed {
        a.remove_node(n);
    }
    for _ in 0..500 {
        b.add_edge(rng.gen_range(0..NODES), rng.gen_range(0..NODES));
    }

    a.merge(&b);
    b.merge(&a);
    assert_eq!(a.edges(), b.edges());
    let visible = a.edges().len();
    let stored = a.edges.elements().len();
    println!("post-merge: {} visible edges, {} stored ({} dangling, hidden)", visible, stored, stored - visible);

    for &n in &doomed {
        a.add_node(n);
    }
    println!("after re-adding nodes: {} visible (resurrected {})", a.edges().len(), a.edges().len() - visible);
}
