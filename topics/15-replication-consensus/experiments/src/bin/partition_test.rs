//! Provided: a 5-node cluster timeline under partition/heal.
//! Requires raft.rs implemented. Prints who leads, what commits when.

use replication_experiments::sim::Sim;

fn snapshot(sim: &Sim, label: &str) {
    println!("--- tick {:4} {label}", sim.ticks);
    for n in &sim.nodes {
        println!(
            "  node {} term {:2} {:9?} commit {:2} log {:?}",
            n.id,
            n.term,
            n.role,
            n.commit_index,
            n.log.iter().map(|&(t, c)| format!("{c}@t{t}")).collect::<Vec<_>>()
        );
    }
}

fn main() {
    let mut sim = Sim::new(5, 2026);

    let leader = sim.run_until_leader(500);
    snapshot(&sim, &format!("leader elected: node {leader}"));

    for cmd in [1, 2, 3] {
        sim.propose(leader, cmd);
        sim.run(5);
    }
    sim.run(10);
    snapshot(&sim, "three entries committed");

    let buddy = (0..5).find(|&i| i != leader).unwrap();
    sim.partition(&[leader, buddy]);
    println!("\n=== PARTITION: {{{leader},{buddy}}} vs the rest ===");
    sim.propose(leader, 99);
    sim.run(60);
    snapshot(&sim, "stale leader proposed 99 (must not commit)");

    let new_leader = sim.current_leader().expect("majority elects");
    println!("\n=== majority side elected node {new_leader} ===");
    sim.propose(new_leader, 4);
    sim.run(20);
    snapshot(&sim, "majority committed 4");

    sim.heal();
    println!("\n=== HEAL ===");
    sim.run(80);
    snapshot(&sim, "after heal: 99 truncated everywhere, logs converged");
}
