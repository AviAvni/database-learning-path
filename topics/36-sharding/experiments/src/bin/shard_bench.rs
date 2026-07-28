use std::panic::{catch_unwind, AssertUnwindSafe};

use sharding_experiments::graphs::{community, edge_cut, power_law, random_assignment};
use sharding_experiments::hashring::HashRing;
use sharding_experiments::partitioner::greedy_partition;
use sharding_experiments::placement::{hot_shard_share, modn_movement, splitmix64};

/// Lane 1 (PROVIDED): the problem. Mod-N movement on growth, and the
/// hot shard that hashing provably cannot split.
fn lane1_the_problem() {
    println!("== lane 1: the problem — mod-N movement and the hot shard ==");
    println!("   growing N shards to N+1: fraction of keys that move");
    println!("   N -> N+1     mod-N    ideal 1/(N+1)");
    for from in [4u64, 5, 8, 16] {
        let m = modn_movement((0..1_000_000u64).map(splitmix64), from, from + 1);
        println!(
            "   {from:>2} -> {:>2}     {:>5.1}%       {:>5.1}%",
            from + 1,
            m * 100.0,
            100.0 / (from + 1) as f64
        );
    }
    println!();
    println!("   Zipf(s) traffic, 10k keys on 16 hash shards; ideal share 6.25%");
    for s in [0.8, 1.0, 1.2] {
        let share = hot_shard_share(10_000, s, 16, 500_000, 42);
        println!(
            "   s = {s:.1}: hottest shard carries {:>4.1}% of traffic ({:.1}x ideal)",
            share * 100.0,
            share * 16.0
        );
    }
    println!();
}

/// Lane 2 (needs hashring.rs): consistent hashing pays 1/(N+1) movement
/// instead of 80%, and vnodes buy balance.
fn lane2_hashring() {
    println!("== lane 2: consistent-hash ring — movement and balance ==");
    let keys: Vec<u64> = (0..200_000u64).map(splitmix64).collect();

    let mut ring = HashRing::new(128);
    for n in 0..4 {
        ring.add_node(n);
    }
    let before: Vec<u32> = keys.iter().map(|&k| ring.lookup(k)).collect();
    ring.add_node(4);
    let after: Vec<u32> = keys.iter().map(|&k| ring.lookup(k)).collect();
    let moved = before.iter().zip(&after).filter(|(b, a)| b != a).count();
    println!(
        "   4 -> 5 nodes (128 vnodes): {:.1}% of keys move   (mod-N: 80.0%, ideal: 20.0%)",
        100.0 * moved as f64 / keys.len() as f64
    );

    let node2_share = after.iter().filter(|&&o| o == 2).count();
    ring.remove_node(2);
    let moved_on_remove = keys
        .iter()
        .zip(&after)
        .filter(|&(&k, &o)| ring.lookup(k) != o)
        .count();
    println!(
        "   remove node 2: {:.1}% of keys move — exactly node 2's share ({:.1}%)",
        100.0 * moved_on_remove as f64 / keys.len() as f64,
        100.0 * node2_share as f64 / keys.len() as f64
    );

    println!("   balance on 8 nodes (max shard / mean), by vnodes per node:");
    for vn in [1u32, 8, 64, 512] {
        let mut r = HashRing::new(vn);
        for n in 0..8 {
            r.add_node(n);
        }
        let mut counts = [0u64; 8];
        for &k in &keys {
            counts[r.lookup(k) as usize] += 1;
        }
        let max = *counts.iter().max().unwrap() as f64;
        println!("   {vn:>4} vnodes: max/mean = {:.2}", max / (keys.len() as f64 / 8.0));
    }
    println!();
}

/// Lane 3 (needs partitioner.rs): edge-cut, random vs one-pass greedy.
/// Random placement cuts (k-1)/k of edges (PowerGraph Thm 5.1) — on
/// k=8 that is 87.5% of all edges crossing the network.
fn lane3_partitioner() {
    println!("== lane 3: graph partitioning at k=8 — edge-cut, random vs greedy ==");
    for (name, g) in [
        ("community (8 x 4000, d_in 8 / d_out 2)", community(8, 4_000, 8, 2, 7)),
        ("power-law (50k vertices, m = 8)        ", power_law(50_000, 8, 7)),
    ] {
        let rand_cut = edge_cut(&random_assignment(g.n, 8, 99), &g.edges);
        let assign = greedy_partition(&g, 8, 0.05);
        let cut = edge_cut(&assign, &g.edges);
        let mut counts = vec![0u64; 8];
        for &p in &assign {
            counts[p as usize] += 1;
        }
        let max_part = *counts.iter().max().unwrap() as f64 / (g.n as f64 / 8.0);
        println!(
            "   {name}: random {:>4.1}% -> greedy {:>4.1}% of {} edges cut (max part {:.2}x ideal)",
            rand_cut * 100.0,
            cut * 100.0,
            g.edges.len(),
            max_part
        );
    }
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
    lane1_the_problem();
    stub_lane("lane 2", lane2_hashring);
    stub_lane("lane 3", lane3_partitioner);
}
