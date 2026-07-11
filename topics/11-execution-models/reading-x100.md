# Reading guide — "MonetDB/X100: Hyper-Pipelining Query Execution" (CIDR '05) (~1 h)

The vectorization manifesto. Twenty years old and it reads like the
DuckDB design doc — because it is (Boncz co-authored both; DuckDB came
out of the same CWI group).

## The setup: why were databases 100× slower than hand-written C?

They profile TPC-H Q1 (scan + filter + arithmetic + group-by — no join!)
on MySQL and find ~90% of time in interpretation overhead: per-tuple
function calls, attribute extraction, expression-tree walking. IPC
(instructions per cycle) near 0.7 on hardware capable of 3+. The famous
framing: databases were running BELOW 10% of what a hand-coded loop
achieves ON THE SAME DATA.

```
 hand-written C for Q1:   ~0.6 s      <- the roofline (topic 0!)
 MySQL (Volcano, rows):   ~27 s       <- 45x of pure interpretation tax
 MonetDB (full-column):   ~3.7 s      <- better, but materializes
 X100 (vectors):          ~0.6 s      <- reaches the roofline
```

## The two failure modes X100 threads between

- **Volcano (tuple-at-a-time)**: overhead per tuple — dies of
  interpretation.
- **MonetDB's original model (full-column-at-a-time)**: each operator
  processes ENTIRE columns, materializing full intermediate results —
  no per-tuple overhead, but intermediates spill out of cache to RAM;
  dies of memory bandwidth. (BAT algebra: every op reads and writes
  DRAM-sized arrays.)
- **X100**: vectors of ~1000 values — small enough that operator
  intermediates stay in L1/L2, big enough to amortize interpretation.
  Pipelining THROUGH the cache: "hyper-pipelining".

## Findings to internalize

- The vector-size sweep (their Figure): performance vs vector length is
  U-shaped. 1 = MySQL, ∞ = MonetDB; the sweet spot is where
  (vectors × columns in flight) ≈ cache size. Your exec_bench should
  reproduce this curve's shape — sweep 1 / 64 / 1024 / 64K.
- Primitives: each operation is a compiled loop
  (`map_add_int_vec_int_vec`) selected at plan time — interpretation
  happens per VECTOR, primitives are branch-free and auto-vectorizable.
  Templated combinatorics (types × ops) generate hundreds of them —
  DuckDB inherits this wholesale.
- Selection vectors appear here too: filters produce index lists;
  primitives take an optional sel.
- IPC as the health metric, not just runtime: X100 runs at ~2 IPC where
  MySQL managed 0.7. (Your flamegraph + `instruments`/counters angle.)

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
