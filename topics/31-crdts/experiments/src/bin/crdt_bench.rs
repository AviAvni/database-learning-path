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
    // Silence the default panic hook for the duration of the lane: an
    // unimplemented exercise is an expected state, not a crash to report.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = catch_unwind(AssertUnwindSafe(f));
    std::panic::set_hook(prev);
    if r.is_err() {
        println!("[stub — implement the todo!()s in src/ to unlock this lane]");
    }
}

/// A write's identity for the oracle: (replica, per-replica sequence number,
/// how many of the *other* replica's writes the writer had received when it
/// wrote). Write (r, n) is causally before write (r', n', seen') iff r == r'
/// (and n < n') or seen' >= n.
type Wid = (u8, u32, u32);

/// LWW merge that classifies every discarded value. Returns
/// (overwritten, concurrent): `overwritten` is what `LwwMap::merge` counts
/// (any local value replaced), `concurrent` counts only the replaced values
/// whose replacement had NOT causally observed them — writes that were
/// actually lost, as opposed to ordinary sequential updates of a key.
fn merge_classified(me: &mut LwwMap<u64, Wid>, other: &LwwMap<u64, Wid>) -> (usize, usize) {
    let (mut over, mut conc) = (0, 0);
    for (k, reg) in &other.entries {
        match me.entries.get_mut(k) {
            Some(mine) => {
                let old = mine.value;
                if mine.merge(reg) {
                    over += 1;
                    let new = reg.value;
                    let causal = old.0 == new.0 || new.2 >= old.1;
                    if !causal {
                        conc += 1;
                    }
                }
            }
            None => {
                me.entries.insert(*k, reg.clone());
            }
        }
    }
    (over, conc)
}

/// Run two replicas for `writes_per_replica` writes each. Returns
/// (total writes, overwritten, concurrent-lost).
fn run_lww(keys: u64, sync_every: usize, writes_per_replica: usize) -> (usize, usize, usize) {
    let mut rng = ChaCha8Rng::seed_from_u64(keys ^ sync_every as u64);
    let mut a: LwwMap<u64, Wid> = LwwMap::new();
    let mut b: LwwMap<u64, Wid> = LwwMap::new();
    let (mut over, mut conc) = (0usize, 0usize);
    let (mut seq_a, mut seq_b) = (0u32, 0u32);
    let (mut a_knows_b, mut b_knows_a) = (0u32, 0u32);
    let mut ts = 0u64;
    let sync = |a: &mut LwwMap<u64, Wid>, b: &mut LwwMap<u64, Wid>, sa: u32, sb: u32, ka: &mut u32, kb: &mut u32| {
        let (o, c) = merge_classified(a, b);
        *ka = sb;
        let (o2, c2) = merge_classified(b, a);
        *kb = sa;
        (o + o2, c + c2)
    };
    for i in 0..writes_per_replica {
        ts += 1;
        seq_a += 1;
        seq_b += 1;
        a.set(rng.gen_range(0..keys), (1, seq_a, a_knows_b), ts, 1);
        b.set(rng.gen_range(0..keys), (2, seq_b, b_knows_a), ts, 2);
        if (i + 1) % sync_every == 0 {
            let (o, c) = sync(&mut a, &mut b, seq_a, seq_b, &mut a_knows_b, &mut b_knows_a);
            over += o;
            conc += c;
        }
    }
    let (o, c) = sync(&mut a, &mut b, seq_a, seq_b, &mut a_knows_b, &mut b_knows_a);
    over += o;
    conc += c;
    (2 * writes_per_replica, over, conc)
}

/// Lane 1: two replicas, `writes` writes each over `keys` keys, syncing
/// every `sync_every` writes. Two counts per row: every local value the
/// merge replaced ("overwritten": what a naive merge counter reports) and
/// the subset replaced by a write that never saw it ("lost": a write no
/// replica remembers and no later write was based on).
fn lane1_lww_lie() {
    println!("=== lane 1: LWW's lie (lost concurrent writes) ===");
    println!("{:>8} {:>12} {:>10} {:>12} {:>13} {:>8} {:>8}", "keys", "sync_every", "writes", "overwritten%", "lost (concurrent)%", "model%", "lost n");
    // Kept small: state-based sync ships the WHOLE map every interval,
    // so sync_every=1 is O(writes * map_size). That cost is itself a
    // finding — real systems ship deltas (delta-CRDTs) for this reason.
    let writes_per_replica = 20_000;
    for &keys in &[10u64, 1_000, 100_000] {
        for &sync_every in &[1usize, 100, 10_000] {
            let (total, over, conc) = run_lww(keys, sync_every, writes_per_replica);
            println!(
                "{:>8} {:>12} {:>10} {:>11.2}% {:>17.2}% {:>7.2}% {:>8}",
                keys,
                sync_every,
                total,
                100.0 * over as f64 / total as f64,
                100.0 * conc as f64 / total as f64,
                model_lost_pct(keys, sync_every),
                conc
            );
        }
    }
    println!("(overwritten = values replaced at merge, including ordinary sequential updates;");
    println!(" lost = replaced by a write that never saw them. model% = K * pA^2 / (2 s), pA = 1-(1-1/K)^s)");

    println!("\n--- lane 1b: lost% as a function of key-space size and sync interval (5,000 writes/replica) ---");
    let syncs = [1usize, 10, 100, 1_000];
    print!("{:>8}", "keys");
    for s in syncs {
        print!(" {:>16}", format!("sync_every={s}"));
    }
    println!();
    for &keys in &[10u64, 100, 1_000, 10_000] {
        print!("{:>8}", keys);
        for s in syncs {
            let (total, _, conc) = run_lww(keys, s, 5_000);
            print!(" {:>8.2}% ({:>4.2}%)", 100.0 * conc as f64 / total as f64, model_lost_pct(keys, s));
        }
        println!();
    }
    println!("(cell = measured lost% (model%); a window of s writes per replica collides on K*pA^2 keys)");
}

/// Expected concurrent loss: in one sync window each replica writes s
/// times; a key written by both is one lost write (the loser). Number of
/// doubly-written keys = K * pA^2 with pA = 1 - (1 - 1/K)^s; the window has
/// 2s writes.
fn model_lost_pct(keys: u64, s: usize) -> f64 {
    let k = keys as f64;
    let pa = 1.0 - (1.0 - 1.0 / k).powi(s as i32);
    100.0 * k * pa * pa / (2.0 * s as f64)
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
