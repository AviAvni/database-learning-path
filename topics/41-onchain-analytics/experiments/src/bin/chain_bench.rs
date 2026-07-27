use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

use chain_experiments::chain::{
    addresses_of, chain_instance, descendants, seeded_rng, Chain, ChainConfig,
};
use chain_experiments::clustering::{full_clusters, multi_input_clusters, quality};
use chain_experiments::taint::{fifo, flagged_utxos, flagged_value, haircut, poison, Taint};

fn big() -> Chain {
    let mut rng = seeded_rng(42);
    chain_instance(&mut rng, &ChainConfig::default())
}

fn pct(a: usize, b: usize) -> f64 {
    100.0 * a as f64 / b as f64
}

/// Lane 1 (PROVIDED): haircut taint smears the theft over the chain.
fn lane1_haircut_diffusion() {
    println!("== lane 1: haircut tainting — how far does a single theft reach? ==");
    let c = big();
    println!(
        "   {} transactions, {} outputs, {} addresses, {} entities; one stolen coinbase\n",
        c.txs.len(),
        c.n_outputs(),
        c.n_addresses(),
        c.n_entities
    );
    let h = haircut(&c, c.stolen_output);
    let utxos = c.utxo_set();
    let flagged = flagged_utxos(&c, &h);
    let flagged_addrs = addresses_of(&c, flagged.iter().copied());
    let all_addrs = addresses_of(&c, utxos.iter().copied());
    println!("   haircut, at the end of the chain:");
    println!(
        "     tainted UTXOs      {:>6} of {:>6}  ({:>5.1}%)",
        flagged.len(),
        utxos.len(),
        pct(flagged.len(), utxos.len())
    );
    println!(
        "     tainted addresses  {:>6} of {:>6}  ({:>5.1}%)",
        flagged_addrs.len(),
        all_addrs.len(),
        pct(flagged_addrs.len(), all_addrs.len())
    );
    println!(
        "     tainted value      {:>12} of {:>12}  (the theft was {})",
        flagged_value(&c, &h),
        c.total_value(),
        c.stolen_value
    );

    // How thin is the smear? Distribution of taint fraction per UTXO.
    let mut trace = 0usize;
    let mut small = 0usize;
    let mut real = 0usize;
    for &o in &flagged {
        let f = h[o] as f64 / c.outputs[o].value as f64;
        if f < 0.001 {
            trace += 1
        } else if f < 0.05 {
            small += 1
        } else {
            real += 1
        }
    }
    println!(
        "     of those UTXOs: {trace} are <0.1% tainted, {small} are 0.1–5%, {real} are >5%"
    );
    println!();
    println!("   the total is conserved — haircut does not invent money — but it");
    println!("   is spread so thin that \"is this coin tainted?\" stops meaning");
    println!("   anything. Anderson et al. measured the real version: the 2012");
    println!("   Linode theft of 46,653 BTC taints 16,855,619 addresses (93% of");
    println!("   all of them) under haircut, and 245,120 (1.35%) under FIFO;");
    println!("   the 2014 Flexcoin hack taints 10,421,112 vs 15,265. A rule that");
    println!("   taints 93% of everyone is a tax, not a forensic tool.");
    println!();
}

/// Lane 2 (needs taint.rs): poison / haircut / FIFO, side by side.
fn lane2_policies() {
    println!("== lane 2: three taint policies on the same theft ==");
    let c = big();
    let utxos = c.utxo_set();
    let d = descendants(&c, c.stolen_output);
    println!(
        "   the theft was {}; {} outputs descend from it in the graph",
        c.stolen_value,
        d.len()
    );
    println!("   policy    tainted UTXOs   tainted addrs   value flagged   vs stolen");
    let run = |name: &str, t: &Taint| {
        let f = flagged_utxos(&c, t);
        let a = addresses_of(&c, f.iter().copied());
        let v = flagged_value(&c, t);
        println!(
            "   {name:<8}  {:>7} ({:>5.1}%)  {:>8}        {:>12}   {:>7.2}x",
            f.len(),
            pct(f.len(), utxos.len()),
            a.len(),
            v,
            v as f64 / c.stolen_value as f64
        );
    };
    run("poison", &poison(&c, c.stolen_output));
    run("haircut", &haircut(&c, c.stolen_output));
    run("fifo", &fifo(&c, c.stolen_output));
    println!();

    let t = Instant::now();
    let f = fifo(&c, c.stolen_output);
    let dt = t.elapsed().as_secs_f64();
    println!(
        "   FIFO over {} transactions in {:.1} ms ({:.1}M tx/s) — one queue splice",
        c.txs.len(),
        dt * 1e3,
        c.txs.len() as f64 / dt / 1e6
    );
    println!(
        "   concentration: the top-flagged UTXO holds {:.1}% of all flagged value",
        {
            let mut v: Vec<u64> = flagged_utxos(&c, &f).iter().map(|&o| f[o]).collect();
            v.sort_unstable_by(|a, b| b.cmp(a));
            let tot: u64 = v.iter().sum();
            100.0 * v.first().copied().unwrap_or(0) as f64 / tot.max(1) as f64
        }
    );
    println!();
    println!("   poison invents money: it counts each descendant output's FULL");
    println!("   value, so the \"stolen\" total explodes with the fan-out. Haircut");
    println!("   conserves the total but touches every descendant. FIFO conserves");
    println!("   the total AND keeps it in one place, because a satoshi is stolen");
    println!("   or it is not — which is exactly Clayton's Case (1816), and why");
    println!("   the taint can be traced backwards as well as forwards.");
    println!();
}

/// Lane 3 (needs clustering.rs): pseudonyms back into entities.
fn lane3_clustering() {
    println!("== lane 3: address clustering — and how it collapses ==");
    println!("   change reuse   H1 clusters  H1 prec  H1 rec | H1+2 clusters  prec   rec   largest");
    for r in [0.0, 0.01, 0.02, 0.05, 0.10, 0.20] {
        let mut rng = seeded_rng(11);
        let c = chain_instance(
            &mut rng,
            &ChainConfig {
                n_entities: 120,
                n_txs: 8_000,
                change_reuse_rate: r,
                ..ChainConfig::default()
            },
        );
        let h1 = quality(&c, &multi_input_clusters(&c));
        let both = quality(&c, &full_clusters(&c));
        println!(
            "   {r:>12.2}   {:>11}  {:>7.3}  {:>6.3} | {:>13}  {:>5.3} {:>5.3}   {:>5} ({:.0}% of {})",
            h1.n_clusters,
            h1.precision,
            h1.recall,
            both.n_clusters,
            both.precision,
            both.recall,
            both.largest,
            pct(both.largest, c.n_addresses()),
            c.n_addresses()
        );
    }
    println!();
    println!("   Heuristic 1 (co-spend) holds precision 1.000 at every reuse rate:");
    println!("   it keys on a property of the protocol, and you cannot be co-spent");
    println!("   with someone whose key you do not hold. Heuristic 2 (change) buys");
    println!("   real recall — 0.041 to 0.397 at reuse 0 — but it keys on an idiom");
    println!("   of use, and one reused change address in a hundred already costs");
    println!("   a third of its precision. At one in twenty the largest cluster is");
    println!("   16% of the chain; at one in ten it is 71%. Meiklejohn's own");
    println!("   refined run still welded Mt. Gox, Instawallet, BitPay and Silk");
    println!("   Road into one 1.6M-key cluster; BlockSci's 2019 chain has a");
    println!("   supercluster of over 17 million addresses.");
    println!();
}

fn stub_lane(name: &str, f: impl FnOnce() + std::panic::UnwindSafe) {
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        println!("[stub — implement the todo!()s to unlock {name}]\n");
    }
}

fn main() {
    lane1_haircut_diffusion();
    stub_lane("lane 2", lane2_policies);
    stub_lane("lane 3", lane3_clustering);
}
