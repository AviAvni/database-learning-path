# X100: the vectorization manifesto

MonetDB/X100 (CIDR '05) is where vectorized execution was born — from a
profiler, not a whiteboard. Twenty years old and it reads like the DuckDB
design doc — because it is (Boncz co-authored both; DuckDB came out of
the same CWI group). Before the paper, this chapter builds its argument
step by step: the profile that started it, the two failure modes it
threads between, the vector, the primitive, and the health metric — then
routes you through the sections.

## The problem in one sentence

In 2005 a database evaluating TPC-H Q1 — a plain scan + filter +
arithmetic + group-by, no join — ran **45× slower than a hand-written C
loop over the same data**, and ~90% of that time was interpretation
overhead, not computation.

## The concepts, step by step

### Step 1 — the profile: databases ran below 10% of the hardware

The paper opens with measurement, not design. Profiling TPC-H Q1 on
MySQL shows ~90% of time in interpretation overhead: per-tuple function
calls, attribute extraction, expression-tree walking — the machinery of
deciding what to do, not doing it. The health metric they use is **IPC**
(instructions per cycle — how many instructions the core actually
retires per clock; a superscalar core of that era could sustain 3+):
MySQL ran at ~0.7 IPC, because dependent loads and unpredictable
indirect branches stall the pipeline. The famous framing: databases were
running BELOW 10% of what a hand-coded loop achieves ON THE SAME DATA.

```
 hand-written C for Q1:   ~0.6 s      <- the roofline (topic 0!)
 MySQL (Volcano, rows):   ~27 s       <- 45x of pure interpretation tax
 MonetDB (full-column):   ~3.7 s      <- better, but materializes
 X100 (vectors):          ~0.6 s      <- reaches the roofline
```

The hand-written loop is a **roofline** — the hardware's actual capacity
for this query. Everything above it is engine tax.

### Step 2 — failure mode one: tuple-at-a-time (Volcano)

The Volcano model — every operator exposing `next()`, each call
returning ONE tuple — pays its overhead per tuple: an indirect function
call per operator, expression-tree interpretation per row, tuple values
leaving CPU registers between operators (see
reading-postgres-executor.md for this model in production). Overhead ×
N_rows, with overhead ~20–100 ns against ~1 ns of useful work. That is
MySQL's 27 s: it dies of interpretation.

### Step 3 — failure mode two: full-column-at-a-time (old MonetDB)

MonetDB — the authors' own previous system — had already fixed
interpretation by going to the opposite extreme: each operator processes
an ENTIRE column at once (the BAT algebra), so per-tuple overhead is
zero. The new problem: every operator **materializes** its full
intermediate result — writes a complete result column to memory for the
next operator to read back. For Q1 over 6M rows with ~10 intermediates,
that's hundreds of MB streamed to and from DRAM per query; every op
reads and writes DRAM-sized arrays. IPC is fine; **memory bandwidth**
becomes the wall (question 2 has you compute the seconds of pure memory
traffic). It dies of bandwidth.

### Step 4 — the vector: small enough for cache, big enough to amortize

X100 threads between the two failure modes: operators still compose via
`next()`, but each call returns a **vector** — ~1000 values of one
column in a plain array. Two constraints pin the size from opposite
sides:

- big enough that per-call interpretation divides into insignificance —
  ~100 ns of dispatch over 1000 values is 0.1 ns/value;
- small enough that all the vectors in flight between the pipeline's
  operators — intermediates included — stay resident in L1/L2, never
  round-tripping through DRAM. Pipelining THROUGH the cache:
  "hyper-pipelining".

Their vector-size sweep makes the trade visible: performance vs vector
length is U-shaped. Length 1 = MySQL (interpretation tax), length ∞ =
old MonetDB (bandwidth wall); the sweet spot is where (vectors × columns
in flight) ≈ cache size. Vector size is a CACHE parameter, not a tuning
constant — which is why DuckDB's 2048 and X100's ~1000 are the same
decision on different hardware. Your exec_bench should reproduce this
curve's shape — sweep 1 / 64 / 1024 / 64K.

### Step 5 — primitives: interpretation happens per vector, work per value

Inside each `next()`, the work is done by **primitives**: precompiled,
type-specialized loops — `map_add_int_vec_int_vec` — selected once at
plan time. The interpreter's job shrinks to choosing which primitive to
call per vector; the primitive itself is branch-free and
auto-vectorizable (the compiler emits SIMD for it):

```rust
// a primitive: picked once at plan time, then runs branch-free per vector
fn map_add_i64_vec(a: &[i64], b: &[i64], out: &mut [i64],
                   sel: Option<&[u32]>) -> usize {
    match sel {
        None => { for i in 0..a.len() { out[i] = a[i] + b[i]; } a.len() }
        Some(s) => {
            for (o, &i) in s.iter().enumerate() {
                out[o] = a[i as usize] + b[i as usize];
            }
            s.len()
        }
    }
}   // interpretation: ONE dispatch per ~1000 values, not per value;
    // ~1000 × 8 B per operand keeps the intermediates in L1
```

Note the `sel` parameter: **selection vectors** appear here first —
filters produce index lists over untouched data, and every primitive
takes an optional sel (DuckDB inherits this wholesale). The cost of the
primitive scheme is combinatorics: types × operations generate hundreds
of monomorphized loops — the C++ template / Rust generics trick, paid in
compile time and binary size (question 3).

### Step 6 — the discipline: IPC as the health metric

The paper's lasting methodological lesson: measure IPC, not just
runtime. X100 runs at ~2 IPC where MySQL managed 0.7 — same hardware,
same data, ~3× more of the silicon actually working. Runtime tells you
*that* you're slow; IPC (plus cache-miss and branch-miss counters) tells
you *which wall* you're against — interpretation (low IPC, high
branches), bandwidth (low IPC, high misses), or genuinely compute-bound
(high IPC: you're done optimizing dispatch). This is your flamegraph +
`instruments`/counters angle for the experiments.

## How to read the paper (with the concepts in hand)

~1 h. The TPC-H Q1 profile and the vector-size sweep figure are the two
things to internalize.

- **§1–2 (the problem + how CPUs work)** — Steps 1–2. The 2005 CPU
  tutorial is dated in constants, current in structure; skim if topic 0
  is fresh.
- **§3 (microbenchmark: TPC-H Q1) — read carefully.** Step 1's table
  measured: MySQL's per-operation profile (the famous "90% overhead"
  breakdown), old MonetDB's bandwidth wall (Step 3).
- **§4 (the X100 architecture)** — Steps 4–5: vectors, primitives, the
  in-cache pipeline. Watch for the selection-vector plumbing.
- **§5 (evaluation)** — the vector-size sweep: find the U-curve, label
  both ends with the failure modes, note where ~1000 sits relative to
  their cache sizes.
- **§6** — skim; the DSM/NSM storage discussion feeds topic 12.

## Questions for notes.md

1. Reproduce the arithmetic: 8-col chunk of 8-byte values — what vector
   length keeps 3 operators' intermediates inside your M-series L1
   (128 KB data)? Does DuckDB's 2048 fit?
2. Full-column MonetDB dies of bandwidth. Compute: Q1 over 6M rows,
   ~10 intermediate columns materialized — GB moved vs your Mac's
   ~100 GB/s. Seconds of pure memory traffic?
3. Primitives are monomorphized per type combination — the C++ template
   trick. What's the Rust equivalent, and what does it do to compile
   time / binary size? (You'll hit this writing kernels.rs.)
4. X100 pre-dates SIMD-everywhere: which of its wins does the compiler
   now deliver FREE via autovectorization of the primitive loops, and
   what still needs explicit `std::simd`? (Answer after writing
   kernels.rs — compare autovec asm vs your manual version.)

## Done when

You can draw the U-curve from memory with the two failure modes labeled,
and explain why vector size is a CACHE parameter, not a tuning constant.

## References

**Papers**
- Boncz, Zukowski, Nes — "MonetDB/X100: Hyper-Pipelining Query
  Execution" (CIDR 2005) — ~1 h; the TPC-H Q1 profile and the
  vector-size sweep figure are the two things to internalize
