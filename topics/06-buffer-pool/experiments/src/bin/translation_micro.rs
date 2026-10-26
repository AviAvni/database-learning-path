//! Translation micro-lane (provided, runs today): what does a hash-table
//! page lookup cost next to a direct pointer, and how much of it can the CPU
//! hide when the page is read from a *predicted* frame in parallel?
//!
//! Three access shapes over the same 1 Mi in-memory "pages" (64 B each, 64 MiB):
//!
//!   direct      frame = frames[pid]                    (swizzled-pointer shape: no translation)
//!   hash        frame = frames[table.lookup(pid)]      (read depends on the lookup)
//!   predictive  spec = frames[pid]  issued next to the lookup; if the lookup
//!               says the page really is in its preferred frame, use `spec`,
//!               else fall back to the looked-up frame (Zinsmeister et al.,
//!               SIGMOD '26, Listing 2). The branch lets the CPU run the read
//!               speculatively while the lookup is still in flight.
//!
//! Two workload shapes:
//!   independent  the next pid comes from a precomputed random array — the
//!                CPU can overlap iterations (throughput-bound).
//!   chained      the next pid is read out of the page just fetched — a B-tree
//!                descent in miniature (latency-bound; nothing to overlap
//!                except translation with the read itself).
//!
//! `miss%` is the fraction of pages NOT in their preferred frame (their
//! frames are permuted among themselves), i.e. the misprediction rate.
//!
//! Deviations from the paper, on purpose: the table is open-addressing with
//! linear probing at 50% load (the paper chains with an inlined first slot),
//! has no latch/version check, and pages are 64 B not 4 KiB. This measures the
//! dependency-chain argument of §3 only, not PrediCache.

use rand::prelude::*;
use std::time::Instant;

const PAGES: usize = 1 << 20;
const TABLE: usize = 1 << 21; // 2x pages -> 50% fill, the paper's sizing
const STEPS: usize = 5_000_000;

#[repr(align(64))]
#[derive(Clone, Copy)]
struct Frame {
    next: u64, // pid of the "child" this page points at
    pid: u64,  // which page lives here (for the sanity check)
}

#[derive(Clone, Copy)]
struct Slot {
    pid: u64, // u64::MAX = empty
    frame: u32,
}

struct Table {
    slots: Vec<Slot>,
}

#[inline(always)]
fn h(pid: u64) -> usize {
    (pid.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (64 - 21)) as usize
}

impl Table {
    fn build(frame_of: &[u32]) -> Self {
        let mut slots = vec![Slot { pid: u64::MAX, frame: 0 }; TABLE];
        for (pid, &f) in frame_of.iter().enumerate() {
            let mut i = h(pid as u64);
            while slots[i].pid != u64::MAX {
                i = (i + 1) & (TABLE - 1);
            }
            slots[i] = Slot { pid: pid as u64, frame: f };
        }
        Table { slots }
    }
    #[inline(always)]
    fn lookup(&self, pid: u64) -> u32 {
        let mut i = h(pid);
        loop {
            let s = unsafe { *self.slots.get_unchecked(i) };
            if s.pid == pid {
                return s.frame;
            }
            i = (i + 1) & (TABLE - 1);
        }
    }
}

struct World {
    frames: Vec<Frame>,
    table: Table,
    start: Vec<u64>,
}

/// `miss_pct` % of pages live in a frame other than frames[pid].
fn build(miss_pct: usize) -> World {
    let mut rng = StdRng::seed_from_u64(42);
    // one random cycle over all pids: next[pid] = following pid in the cycle
    let mut order: Vec<u64> = (0..PAGES as u64).collect();
    order.shuffle(&mut rng);
    let mut next = vec![0u64; PAGES];
    for w in 0..PAGES {
        next[order[w] as usize] = order[(w + 1) % PAGES];
    }
    // frame_of[pid]: identity, except a random miss_pct% permuted among themselves
    let mut frame_of: Vec<u32> = (0..PAGES as u32).collect();
    let mut moved: Vec<usize> = (0..PAGES).collect();
    moved.shuffle(&mut rng);
    moved.truncate(PAGES * miss_pct / 100);
    let mut dst = moved.clone();
    dst.shuffle(&mut rng);
    for (&p, &d) in moved.iter().zip(dst.iter()) {
        frame_of[p] = d as u32;
    }
    // a displaced page whose shuffle landed on itself is not a miss; fine.
    let mut frames = vec![Frame { next: 0, pid: 0 }; PAGES];
    for pid in 0..PAGES {
        frames[frame_of[pid] as usize] = Frame { next: next[pid], pid: pid as u64 };
    }
    let start: Vec<u64> = (0..STEPS).map(|_| rng.gen_range(0..PAGES as u64)).collect();
    World { frames, table: Table::build(&frame_of), start }
}

/// The mispredicted path lives out of line so the compiler keeps a real
/// branch: if it were inlined, both arms are "load frames[x].next" and LLVM may
/// merge them into a select on the lookup result, which would make the read
/// depend on the lookup again and delete the whole effect being measured.
///
/// The guessed read is ALSO issued before the branch (`spec`), not inside the
/// taken arm. Inside the arm, `if f == pred { frames[pred] }` lets the optimizer
/// use the equality to rewrite `pred` as `f` — and then the read depends on the
/// lookup again (measured: no gain, with or without `black_box`).
#[cold]
#[inline(never)]
fn slow_next(w: &World, f: u64) -> u64 {
    w.frames[f as usize].next
}

// ---- independent lane: addresses known up front --------------------------

fn indep_direct(w: &World) -> u64 {
    let mut s = 0u64;
    for &pid in &w.start {
        s = s.wrapping_add(w.frames[pid as usize].next);
    }
    s
}
fn indep_hash(w: &World) -> u64 {
    let mut s = 0u64;
    for &pid in &w.start {
        let f = w.table.lookup(pid);
        s = s.wrapping_add(w.frames[f as usize].next);
    }
    s
}
fn indep_pred(w: &World) -> u64 {
    let mut s = 0u64;
    for &pid in &w.start {
        let f = w.table.lookup(pid) as u64;
        let spec = w.frames[pid as usize].next; // read at the GUESS, address = pid only
        let nx = if f == pid { spec } else { slow_next(w, f) };
        s = s.wrapping_add(nx);
    }
    s
}

// ---- chained lane: next address comes out of the page --------------------

fn chain_direct(w: &World) -> u64 {
    let mut pid = w.start[0];
    for _ in 0..STEPS {
        pid = w.frames[pid as usize].next;
    }
    pid
}
fn chain_hash(w: &World) -> u64 {
    let mut pid = w.start[0];
    for _ in 0..STEPS {
        let f = w.table.lookup(pid);
        pid = w.frames[f as usize].next;
    }
    pid
}
fn chain_pred(w: &World) -> u64 {
    let mut pid = w.start[0];
    for _ in 0..STEPS {
        let f = w.table.lookup(pid) as u64;
        let spec = w.frames[pid as usize].next;
        pid = if f == pid { spec } else { slow_next(w, f) };
    }
    pid
}

fn time(f: impl Fn() -> u64) -> (f64, u64) {
    let t = Instant::now();
    let r = f();
    (t.elapsed().as_nanos() as f64 / STEPS as f64, r)
}

fn main() {
    println!("{PAGES} pages x 64 B = {} MiB frames, table {} MiB, {STEPS} accesses per cell\n",
        PAGES * 64 >> 20, TABLE * 16 >> 20);
    println!("{:>6}  {:<12} {:>10} {:>10} {:>10}", "miss%", "workload", "direct", "hash", "predictive");
    for miss in [0usize, 10, 50] {
        let w = build(miss);
        // sanity: every lookup lands on the frame that holds that pid
        for pid in (0..PAGES as u64).step_by(4099) {
            assert_eq!(w.frames[w.table.lookup(pid) as usize].pid, pid);
        }
        // `direct` reads frames[pid] with no translation, so it is only
        // correct (and only shown) when every page is in its preferred frame.
        let cell = |v: f64| format!("{v:>7.1} ns");
        let (a, ra) = time(|| indep_direct(&w));
        let (b, rb) = time(|| indep_hash(&w));
        let (c, rc) = time(|| indep_pred(&w));
        assert_eq!(rb, rc);
        assert!(miss != 0 || ra == rb);
        let da = if miss == 0 { cell(a) } else { "      -   ".to_string() };
        println!("{:>6}  {:<12} {} {} {}", miss, "independent", da, cell(b), cell(c));
        let (a, ra) = time(|| chain_direct(&w));
        let (b, rb) = time(|| chain_hash(&w));
        let (c, rc) = time(|| chain_pred(&w));
        assert_eq!(rb, rc);
        assert!(miss != 0 || ra == rb);
        let da = if miss == 0 { cell(a) } else { "      -   ".to_string() };
        println!("{:>6}  {:<12} {} {} {}", miss, "chained", da, cell(b), cell(c));
    }
    println!("\nns per page access. direct = no translation; hash = read waits on the lookup;");
    println!("predictive = read issued at the predicted frame beside the lookup.");
}
