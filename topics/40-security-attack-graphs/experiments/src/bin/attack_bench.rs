use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use attack_experiments::ad_graph::{
    ad_instance, attack_path_lengths, attack_path_reachable_users, direct_tier_zero_members,
    memberof_reachable_users, seeded_rng, AdConfig,
};
use attack_experiments::authz::{
    check_pointer, nested_groups, intersect_linear, LeopardIndex,
};
use attack_experiments::chokepoint::{blast_radius, blast_radius_naive, exposure, rank_chokepoints};

/// Lane 1 (PROVIDED): the list view of privilege vs the graph view.
fn lane1_exposure() {
    println!("== lane 1: who is a Domain Admin? list answer vs graph answer ==");
    println!("   (2000 users, 400 groups, 1000 computers; 5 direct tier-zero members,");
    println!("    3 more in one nested group; tiered layout, no policy violations)");
    println!("   1% of users (20 of 2000) are in one over-privileged group.");
    println!("   sessions collected   direct   MemberOf closure   attack-path reachable");
    for sessions in [0usize, 100, 250, 500, 1_000, 2_000] {
        let mut rng = seeded_rng(42);
        let g = ad_instance(
            &mut rng,
            &AdConfig {
                staff_fraction: 0.01,
                ordinary_sessions: sessions,
                ..AdConfig::tiered()
            },
        );
        let direct = direct_tier_zero_members(&g);
        let nested = memberof_reachable_users(&g);
        let paths = attack_path_reachable_users(&g);
        let pct = 100.0 * paths as f64 / g.n_users as f64;
        println!("   {sessions:>18}   {direct:>6}   {nested:>16}   {paths:>10} ({pct:>5.1}%)");
    }
    println!();

    // The other dial: a single misplaced token.
    print!("   Domain Admin tokens left on ordinary workstations: ");
    for v in [0usize, 1, 2, 4] {
        let mut rng = seeded_rng(42);
        let g = ad_instance(
            &mut rng,
            &AdConfig {
                staff_fraction: 0.0,
                da_on_workstation: v,
                ..AdConfig::tiered()
            },
        );
        print!("{v} -> {}   ", attack_path_reachable_users(&g));
    }
    println!("\n   (staff_fraction 0, so the gateway is shut; the tokens alone do this)");
    println!();

    let mut rng = seeded_rng(42);
    let g = ad_instance(&mut rng, &AdConfig::default());
    let (n, mean, max) = attack_path_lengths(&g);
    println!(
        "   default directory: {n} users reach tier zero; shortest path mean\n   {mean:.2} hops, worst {max} hops — the exposure is not remote."
    );
    println!();
    println!("   the MemberOf closure is what the directory console shows you and");
    println!("   what the compliance report counts: 8, forever. Every extra user in");
    println!("   the last column got there through AdminTo + HasSession — local");
    println!("   admin on a machine where a privileged token happens to be sitting.");
    println!("   Nobody granted that. It is an emergent property of the graph,");
    println!("   invisible to any per-object permission review, and it cascades:");
    println!("   20 over-privileged users expose 39 without session data and 1969");
    println!("   with 100 sessions, because each newly exposed user's own sessions");
    println!("   drag in everyone who is local admin on those machines. Note what");
    println!("   that means operationally — your exposure number is a function of");
    println!("   how long you ran the collector, not of how much privilege exists.");
    println!();
}

fn cut_node(g: &mut attack_experiments::ad_graph::AdGraph, d: usize) {
    g.edges.retain(|&(a, b, _)| a != d && b != d);
    g.adj = vec![Vec::new(); g.n_nodes()];
    for &(a, b, _) in &g.edges {
        g.adj[a].push(b);
    }
}

/// Lane 2 (needs chokepoint.rs): pricing every remediation at once.
fn lane2_chokepoints() {
    println!("== lane 2: choke points — dominators price every single-node cut ==");

    for (label, conf) in [
        ("tiered", AdConfig::tiered()),
        ("flat  ", AdConfig::default()),
    ] {
        let mut rng = seeded_rng(42);
        let g = ad_instance(&mut rng, &conf);
        let total = exposure(&g, None);

        let t = Instant::now();
        let fast = blast_radius(&g);
        let dt_fast = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let slow = blast_radius_naive(&g);
        let dt_slow = t.elapsed().as_secs_f64();
        let agree = (0..g.n_nodes()).all(|d| fast[d] == slow[d]);

        println!(
            "   [{label}] {} nodes / {} edges, {total} exposed users",
            g.n_nodes(),
            g.edges.len()
        );
        println!(
            "     dominator tree {:.1} ms  vs  {} reachability re-runs {:.0} ms  ({:.0}x), exact match: {agree}",
            dt_fast * 1e3,
            g.n_nodes(),
            dt_slow * 1e3,
            dt_slow / dt_fast,
        );

        let ranked = rank_chokepoints(&g);
        if ranked.is_empty() {
            println!("     top choke point: NONE — no single node cut removes a single user");
        } else {
            for &(d, b) in ranked.iter().take(3) {
                let what = if g.is_group(d) { "group" } else { "computer" };
                let name = if d == g.helpdesk {
                    " (helpdesk)"
                } else if d == g.staff {
                    " (staff)"
                } else {
                    ""
                };
                println!(
                    "     choke point {what} {d}{name}: {b} users ({:.1}% of exposure)",
                    100.0 * b as f64 / total as f64
                );
            }
        }

        // Greedy: cut the best node, recompute, repeat. When the
        // dominator pass finds nothing, fall back to the honest thing —
        // score every node by re-running reachability.
        let mut work = ad_instance(&mut seeded_rng(42), &conf);
        let mut trace = vec![total.to_string()];
        for _ in 0..5 {
            let ranked = rank_chokepoints(&work);
            let pick = match ranked.first() {
                Some(&(d, _)) => Some(d),
                None => {
                    let blast = blast_radius_naive(&work);
                    (0..work.n_nodes())
                        .filter(|&d| d != work.tier_zero && !work.is_user(d))
                        .max_by_key(|&d| blast[d])
                        .filter(|&d| blast[d] > 0)
                }
            };
            let Some(d) = pick else { break };
            cut_node(&mut work, d);
            trace.push(exposure(&work, None).to_string());
        }
        if trace.len() == 1 {
            // Nothing is worth cutting on its own. Cut the planted set
            // as a *plan* instead and watch it collapse — remediation on
            // a flat directory is a set problem, not a ranking problem.
            let mut work = ad_instance(&mut seeded_rng(42), &conf);
            let mut plan = vec![work.helpdesk];
            plan.extend(work.service_group_ids.clone());
            plan.extend(work.violation_hosts.clone());
            let mut planned = vec![total.to_string()];
            for d in plan {
                cut_node(&mut work, d);
                planned.push(exposure(&work, None).to_string());
            }
            println!("     greedy cuts: {} (nothing to cut)", trace.join(" -> "));
            println!(
                "     planned cut of the whole gateway set (helpdesk + {} service groups + {} hosts): {}",
                conf.service_groups,
                conf.da_on_workstation,
                planned.join(" -> ")
            );
        } else {
            println!("     greedy cuts: {}", trace.join(" -> "));
        }
        println!();
    }

    println!("   same exposure, two different worlds. Tiering is not just hygiene:");
    println!("   it is what makes the graph *have* choke points. In the flat");
    println!("   directory two unmanaged service groups and one Domain Admin token");
    println!("   on a workstation route around every gateway, so the blast radius");
    println!("   of every single-node cut is zero — the dominator pass returning");
    println!("   nothing IS the finding. This is Ammann et al.'s \"cut set\"");
    println!("   question (CCS'02 §2.3), answered exactly in one pass: in the");
    println!("   reverse graph rooted at tier zero, node d dominates u iff every");
    println!("   attack path from u passes through d, so d's dominator subtree IS");
    println!("   its blast radius.");
    println!();
}

/// Lane 3 (needs authz.rs): Zanzibar Check — pointer chasing vs Leopard.
fn lane3_authz() {
    println!("== lane 3: Zanzibar Check — pointer chasing vs a flattened index ==");
    println!("   nesting depth   tuple reads   check µs   index probes   index µs   index entries");
    for depth in [2usize, 4, 8, 16, 32] {
        let mut rng = seeded_rng(9);
        let (store, deep_user) = nested_groups(&mut rng, depth, 8, 25, 5_000);
        let index = LeopardIndex::build(&store);

        let reps = 2_000;
        let t = Instant::now();
        let mut cost = Default::default();
        for _ in 0..reps {
            let (hit, c) = check_pointer(&store, deep_user, 0, true);
            assert!(hit);
            cost = c;
        }
        let ptr_us = t.elapsed().as_secs_f64() * 1e6 / reps as f64;

        let t = Instant::now();
        let mut probes = 0;
        for _ in 0..reps {
            let (hit, p) = index.check(deep_user, 0);
            assert!(hit);
            probes = p;
        }
        let idx_us = t.elapsed().as_secs_f64() * 1e6 / reps as f64;

        println!(
            "   {depth:>13}   {:>11}   {ptr_us:>8.2}   {probes:>12}   {idx_us:>8.2}   {:>13}",
            cost.tuple_reads,
            index.size_entries()
        );
    }
    println!();

    // The denormalization tax, and the lopsided-intersection win.
    let mut rng = seeded_rng(9);
    let (store, deep_user) = nested_groups(&mut rng, 32, 8, 25, 5_000);
    let t = Instant::now();
    let index = LeopardIndex::build(&store);
    let build_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "   depth 32: {} stored tuples -> {} index entries ({:.1}x), built in {build_ms:.1} ms",
        store.tuple_count(),
        index.size_entries(),
        index.size_entries() as f64 / store.tuple_count() as f64
    );
    let a = &index.member2group[deep_user as usize];
    let b = &index.group2group[0];
    let (_, gallop) = attack_experiments::authz::intersect_galloping(a, b);
    let (_, merge) = intersect_linear(a, b);
    println!(
        "   |MEMBER2GROUP(u)| = {}, |GROUP2GROUP(g)| = {}: galloping {gallop} probes vs linear merge {merge} steps",
        a.len(),
        b.len()
    );
    println!();
    println!("   pointer chasing pays for the shape of the graph; the index pays");
    println!("   once, at build time, and then pays rent forever — Zanzibar runs");
    println!("   Leopard as an offline pipeline plus an incremental layer fed by");
    println!("   Watch (~500 index updates/sec median, §4.4).");
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_exposure();
    stub_lane("lane 2", lane2_chokepoints);
    stub_lane("lane 3", lane3_authz);
}
