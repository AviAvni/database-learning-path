//! tier_bench — the tier boundary this machine has, and what placement does
//! at it.
//!
//! cargo run --release --bin tier_bench
//!
//! Lane 1 (provided, measured): a dependent-load latency ladder from 16 KiB
//! to 512 MiB, then one seeded access stream over 1 GiB of 64 B objects with
//! the hot set packed or scattered — same ids, same order, different
//! addresses.
//! Lane 2 (exercise, counted): learn a layout from one window, score it on
//! the next.
//! Lane 3 (exercise, MODEL): two-tier page placement priced with published
//! CXL latencies. Nothing in lane 3 is measured.

use memory_tiering_experiments::{
    chase::{self, Arena},
    gen::{self, Chain, Workload},
    layout::{self, count_window, count_window_by, Layout},
    reorg, stats, tier, LINE_BYTES, N_OBJ, OBJS_PER_16K, OBJS_PER_4K, OBJ_BYTES,
};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

static STUBBED: AtomicBool = AtomicBool::new(false);

fn quiet_stubs() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info.to_string();
        if msg.contains("not yet implemented") {
            STUBBED.store(true, Ordering::Relaxed);
        } else {
            eprintln!("{msg}");
        }
    }));
}

const HOT_FRACS: [f64; 4] = [0.001, 0.005, 0.01, 0.05];
const WARM: usize = 2_000_000;
const STEPS: usize = 4_000_000;
const REPS: usize = 5;
/// the window lanes 2 and 3 count over (same length as a timed run)
const WINDOW: usize = STEPS;

fn mib(bytes: f64) -> f64 {
    bytes / (1024.0 * 1024.0)
}

fn load() -> String {
    stats::loadavg().unwrap_or_else(|| "n/a".into())
}

fn main() {
    quiet_stubs();
    println!("=== tier_bench ===\n");
    println!("load average at start: {}", load());
    println!(
        "objects: {} × {} B = {} MiB; pages counted at 16 KiB ({} objects) and 4 KiB ({}); cache line {} B\n",
        N_OBJ,
        OBJ_BYTES,
        (N_OBJ as usize * OBJ_BYTES) >> 20,
        OBJS_PER_16K,
        OBJS_PER_4K,
        LINE_BYTES
    );

    let dram_ns = lane1a();
    lane1b(dram_ns);
    lane2();
    lane3();

    println!("\nload average at end: {}", load());
    if STUBBED.load(Ordering::Relaxed) {
        println!("\n[stub — implement src/reorg.rs (lane 2) and the tpp() policy in src/tier.rs (lane 3) to unlock the lanes marked STUB]");
    }
}

/// Lane 1a: ns per dependent load as the working set grows. Returns the
/// largest working set's figure (the DRAM rung) for lane 1b's sanity check.
fn lane1a() -> f64 {
    println!("-- lane 1a: dependent-load latency ladder (random cyclic permutation of u32 slots) --");
    println!("   (median of {REPS} runs of 2,000,000 steps each; position carried across runs)\n");
    println!("   working set     ns/load");
    let sizes_kib: [usize; 11] = [16, 64, 256, 1024, 4096, 8192, 16384, 32768, 65536, 262144, 524288];
    let mut first = 0.0;
    let mut last = 0.0;
    for (i, &kib) in sizes_kib.iter().enumerate() {
        let ns = chase::ladder_point(kib * 1024, 2_000_000, REPS, gen::SEED ^ kib as u64);
        let label = if kib >= 1024 { format!("{} MiB", kib / 1024) } else { format!("{kib} KiB") };
        println!("   {label:>11}     {ns:7.2}");
        if i == 0 {
            first = ns;
        }
        last = ns;
    }
    // A dependent load cannot complete in under ~2 cycles (0.5 ns at 4 GHz);
    // a DRAM rung outside 50–400 ns is not a DRAM access.
    let ok1 = first >= 0.5;
    let ok2 = (50.0..=400.0).contains(&last);
    println!(
        "\n   sanity: smallest rung {first:.2} ns ≥ 0.5 ns — loads are dependent{}",
        if ok1 { "" } else { "  ** IMPLAUSIBLE: the chain is not serialising **" }
    );
    println!(
        "   sanity: 512 MiB rung {last:.1} ns within 50–400 ns — a DRAM access{}\n",
        if ok2 { "" } else { "  ** IMPLAUSIBLE **" }
    );
    last
}

/// Lane 1b: the same id stream, hot set packed vs scattered.
fn lane1b(dram_ns: f64) {
    println!("-- lane 1b: hot/cold placement — 1 GiB of 64 B objects, 90% of accesses to the hot set --");
    println!("   (each row: {WARM} warm-up accesses, then median of {REPS} timed runs of {STEPS}; one continuing seeded chain per layout,\n    packed and scattered timed in alternation)\n");

    // the loop's own cost: the same code with every object in one 16 KiB page
    let tiny = Workload::hotspot(OBJS_PER_16K, 0.01);
    let floor_arena = Arena::new(Layout::packed(OBJS_PER_16K));
    let (floor, _) = chase::measure(&floor_arena, &tiny, gen::SEED, WARM, STEPS, REPS);
    drop(floor_arena);
    println!("   loop floor (all objects in one 16 KiB page):      {floor:6.2} ns/access");

    // the other end: the same loop with no hot set at all — every access a
    // uniform draw over the whole GiB. This is the arena's own DRAM rung,
    // measured under the same conditions as the rows below.
    let packed_arena = Arena::new(Layout::packed(N_OBJ));
    let scattered_arena = Arena::new(Layout::scattered(N_OBJ));
    let all_cold = Workload::new(N_OBJ, 1, 0.0);
    let (cold_ns, _) = chase::measure(&packed_arena, &all_cold, gen::SEED, WARM, STEPS, REPS);
    println!("   all cold (every access uniform over the 1 GiB):   {cold_ns:6.2} ns/access\n");

    println!(
        "   hot set              layout      ns/access   hot lines   hot pages   pages for 90%   hot-page util   checksum"
    );
    let mut worst = 0.0f64;
    for &f in &HOT_FRACS {
        let w = Workload::hotspot(N_OBJ, f);
        // counts over the first timed window (deterministic; no memory touched)
        let mut c = Chain::new(gen::SEED);
        gen::trace(&w, &mut c, WARM);
        let window = gen::trace(&w, &mut c, WINDOW);

        // two arenas, timed in alternation: rep r of packed, then rep r of
        // scattered, so a burst of load from elsewhere hits both layouts
        let arenas = [&packed_arena, &scattered_arena];
        let (ns, sums, ratio) = chase::measure_pair(arenas, &w, gen::SEED, WARM, STEPS, REPS);
        for (j, a) in arenas.iter().enumerate() {
            let l = &a.layout;
            worst = worst.max(ns[j]);
            let (lines, pages) = layout::hot_footprint(l, w.hot, (LINE_BYTES / OBJ_BYTES) as u32, OBJS_PER_16K);
            let wc = count_window(&window, l, OBJS_PER_16K, 0.9);
            let head = if j == 0 {
                format!("{:.1}% ({:.1} MiB)", f * 100.0, mib(w.hot as f64 * OBJ_BYTES as f64))
            } else {
                String::new()
            };
            println!(
                "   {head:<19}  {:<10}  {:9.2}   {lines:9}   {pages:9}   {:13}   {:12.1}%   {:016x}",
                l.name,
                ns[j],
                wc.pages_for_coverage,
                wc.hot_page_utilization * 100.0,
                sums[j]
            );
        }
        println!(
            "   {:<19}  scattered/packed: {:.2}x time (median of per-rep ratios); checksums {}\n",
            "",
            ratio,
            if sums[0] == sums[1] { "agree" } else { "DIFFER — the layouts saw different streams" }
        );
    }

    // A hot set can only make the loop cheaper than drawing every access
    // uniformly; a row slower than the all-cold row (with 10% slack for
    // noise) means the machine changed under the run, not the placement.
    let ok = worst <= 1.1 * cold_ns;
    println!(
        "   sanity: slowest row {worst:.1} ns ≤ 1.1 × all-cold {cold_ns:.1} ns — no hot set costs more than no hot set{}",
        if ok { "" } else { "  ** IMPLAUSIBLE: rerun on a quiet machine **" }
    );
    let ok2 = (0.5..=2.0).contains(&(cold_ns / dram_ns));
    println!(
        "   sanity: all-cold {cold_ns:.1} ns vs lane 1a's 512 MiB rung {dram_ns:.1} ns — same tier{}",
        if ok2 { "" } else { "  ** IMPLAUSIBLE: the two DRAM figures disagree by more than 2x **" }
    );
    println!("   pages for 90% = the fewest 16 KiB pages that receive 90% of the window's accesses (exact count, seeded)\n");

    // the same window at 4 KiB pages — the unit Linux tiering migrates
    println!("   at 4 KiB pages (Linux x86, the unit TPP/MEMTIS migrate):");
    println!("   hot set     layout      pages touched   pages for 90%   MiB for 90%   util of touched");
    for &f in &HOT_FRACS {
        let w = Workload::hotspot(N_OBJ, f);
        let mut c = Chain::new(gen::SEED);
        gen::trace(&w, &mut c, WARM);
        let window = gen::trace(&w, &mut c, WINDOW);
        for (j, l) in [Layout::packed(N_OBJ), Layout::scattered(N_OBJ)].iter().enumerate() {
            let wc = count_window(&window, l, OBJS_PER_4K, 0.9);
            let head = if j == 0 { format!("{:.1}%", f * 100.0) } else { String::new() };
            println!(
                "   {head:<10}  {:<10}  {:13}   {:13}   {:11.1}   {:14.1}%",
                l.name,
                wc.pages_touched,
                wc.pages_for_coverage,
                mib(wc.pages_for_coverage as f64 * 4096.0),
                wc.utilization * 100.0
            );
        }
    }
    println!();
}

/// Lane 2: learn a layout from one window, score it on the next.
fn lane2() {
    println!("-- lane 2: address-space engineering — learn the layout from a trace (counted, not timed) --");
    let w = Workload::hotspot(N_OBJ, 0.01);
    let mut c = Chain::new(gen::SEED);
    let learn = gen::trace(&w, &mut c, WINDOW);
    let eval = gen::trace(&w, &mut c, WINDOW);
    // the working set drifts: ids below hot/2 keep their identity, every other
    // id is renamed by a bijection on [hot/2, n) — so half the hot set is now
    // objects scattered through the id space, none of them hot when the
    // layout was learned (OBASE §2.3, "hotness is transient")
    let keep = w.hot / 2;
    let span = (N_OBJ - keep) as u64;
    const P: u64 = 2_654_435_761; // prime, coprime with span — checked below
    assert!(!span.is_multiple_of(P));
    let rename = |id: u32| if id < keep { id } else { keep + ((id - keep) as u64 * P % span) as u32 };
    let eval_drift: Vec<u32> = eval.iter().map(|&id| rename(id)).collect();

    let scattered = Layout::scattered(N_OBJ);
    let packed = Layout::packed(N_OBJ);
    println!("   1% hot set; learn on accesses 1–{WINDOW}, score on the next {WINDOW}; 16 KiB pages\n");
    println!("   layout                        pages for 90%   hot-page util   pages for 90% after drift");
    let s = count_window(&eval, &scattered, OBJS_PER_16K, 0.9);
    let sd = count_window(&eval_drift, &scattered, OBJS_PER_16K, 0.9);
    println!(
        "   scattered (what lane 1 starts with)  {:9}   {:12.1}%   {:9}",
        s.pages_for_coverage,
        s.hot_page_utilization * 100.0,
        sd.pages_for_coverage
    );
    let o = count_window(&eval, &packed, OBJS_PER_16K, 0.9);
    let od = count_window(&eval_drift, &packed, OBJS_PER_16K, 0.9);
    println!(
        "   packed (oracle: knows the hot set)   {:9}   {:12.1}%   {:9}",
        o.pages_for_coverage,
        o.hot_page_utilization * 100.0,
        od.pages_for_coverage
    );
    match catch_unwind(AssertUnwindSafe(|| reorg::learn_layout(&learn, N_OBJ))) {
        Ok(slot_of) => {
            let l = count_window_by(&eval, N_OBJ as usize, |id| slot_of[id as usize], OBJS_PER_16K, 0.9);
            let ld = count_window_by(&eval_drift, N_OBJ as usize, |id| slot_of[id as usize], OBJS_PER_16K, 0.9);
            println!(
                "   learned (reorg::learn_layout)        {:9}   {:12.1}%   {:9}",
                l.pages_for_coverage,
                l.hot_page_utilization * 100.0,
                ld.pages_for_coverage
            );
        }
        Err(_) => println!("   learned (reorg::learn_layout)        [stub — implement src/reorg.rs]"),
    }
    println!("   (drift: in the scoring window half the hot set ({keep} objects) is replaced by objects scattered");
    println!("    through the id space; the layout is not re-learned)\n");
}

/// Lane 3: two-tier page placement — a MODEL.
fn lane3() {
    println!("-- lane 3: two-tier page placement — MODEL, priced with published CXL numbers, not measured --");
    let w = Workload::hotspot(N_OBJ, 0.01);
    let mut c = Chain::new(gen::SEED);
    gen::trace(&w, &mut c, WARM);
    let window = gen::trace(&w, &mut c, WINDOW);
    let pages = N_OBJ / OBJS_PER_4K;
    let cfg = tier::TierConfig { pages, fast_pages: pages / 5 };
    let m = tier::SIDLE_CXL11;
    println!(
        "   1% hot set, {} pages of 4 KiB, fast tier = {} pages (20%: SIDLE §6.1's limit, TPP's 1:4)",
        pages, cfg.fast_pages
    );
    println!(
        "   cost model: {} — fast {:.1} ns, slow {:.1} ns, {:.0} ns per page moved (copy at CXL bandwidth only: a lower bound)",
        m.name, m.fast_ns, m.slow_ns, m.migrate_ns
    );
    println!("   all-fast {:.1} ns/access, all-slow {:.1} ns/access\n", m.fast_ns, m.slow_ns);
    println!("   layout      policy              fast hits   promotions   demotions   MODEL ns/access");
    for l in [Layout::packed(N_OBJ), Layout::scattered(N_OBJ)] {
        let pt: Vec<u32> = window.iter().map(|&id| l.slot(id) / OBJS_PER_4K).collect();
        let rows: [(&str, Option<tier::TierStats>); 4] = [
            ("first touch", Some(tier::first_touch(&pt, &cfg))),
            ("promote on touch", Some(tier::promote_on_touch(&pt, &cfg))),
            ("oracle static", Some(tier::oracle_static(&pt, &cfg))),
            ("tpp (exercise)", catch_unwind(AssertUnwindSafe(|| tier::tpp(&pt, &cfg))).ok()),
        ];
        for (k, (name, st)) in rows.iter().enumerate() {
            let head = if k == 0 { l.name } else { "" };
            match st {
                Some(st) => println!(
                    "   {head:<10}  {name:<18}  {:8.1}%   {:10}   {:9}   {:15.1}",
                    st.hit_rate() * 100.0,
                    st.promotions,
                    st.demotions,
                    st.model_ns(&m)
                ),
                None => println!("   {head:<10}  {name:<18}  [stub — implement tpp() in src/tier.rs]"),
            }
        }
    }

    println!("\n   Huang, Lu & Tan (CIDR'26) §3.2 model, recomputed from Table 1's k and m:");
    println!("   algorithm        k        m     x* (Eq. 5)   g(x*) (Eq. 6)   Table 1 x*   Table 1 g");
    for (name, k, mm, tx, tg) in [("PHJ", 2.3534, 1.0977, 0.5528, 1.1703), ("NPHJ", 3.7879, 0.3641, 0.5104, 1.0214)] {
        println!(
            "   {name:<9}  {k:7.4}  {mm:7.4}   {:10.4}   {:13.4}   {tx:10.4}   {tg:9.4}",
            tier::hj_x_star(k, mm),
            tier::hj_phi(k, mm)
        );
    }
}
