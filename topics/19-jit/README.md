# Topic 19 — JIT & Query Compilation

The other answer to interpretation overhead. Topic 11 killed the
per-tuple interpreter with *batches* (vectorization); this topic
kills it with *compilation* — turn the query into machine code so
there is no interpreter left to amortize. HyPer made it famous,
Umbra made it fast to compile, SQLite has quietly shipped a bytecode
VM since 2000, and SuiteSparse:GraphBLAS JIT-compiles its semiring
kernels — which makes this FalkorDB home turf twice over (M19 JITs
Cypher expressions with cranelift).

## The problem, measured (bench lane 1, provided — runs today)

`cargo run --release --bin jit_bench` — random arithmetic expression trees
evaluated over columns, interpreter (per row) vs vectorized (per column):

```
depth (nodes)     rows     interp M/s   vector M/s   ratio
    2 (7)         1024          89.37       558.34    6.2x
    2 (7)      2097152          86.83       452.10    5.2x
    4 (31)        1024          18.80       182.04    9.7x
    4 (31)     2097152          17.75       129.32    7.3x
    6 (127)       1024           4.05        47.72   11.8x
    6 (127)    2097152           3.71        32.40    8.7x
    8 (511)       1024           0.95        11.84   12.5x
```

**Interpretation cost scales with expression size, and the penalty compounds:
7 nodes to 511 nodes costs the interpreter 94× (89.4 → 0.95 M rows/s) while the
vectorized evaluator loses 47× (558 → 11.8).** The gap between them therefore
*widens* with depth, from 6× to 12×, which is the opposite of what "interpretive
overhead is a constant factor" would predict.

The reason is the thing this topic is about: a tree-walking interpreter pays
dispatch *per node per row*, so its work is `rows × nodes` of branching on tags,
while the vectorized version pays `nodes` dispatches and amortizes each over a
whole column. That leaves the JIT lane with a precise question rather than a
vague one — compile time is a fixed cost paid once, so break-even rows =
`compile_µs / (µs_per_row_interp − µs_per_row_jit)`. Predict where that lands for
depth 8 before you implement it; the answer is why SQLite still ships a bytecode
VM and HyPer does not.

## 1. The spectrum (and where each system sits)

```
 tree walker ──► bytecode VM ──► template/copy-patch ──► IR JIT ──► LLVM -O3
 (eval per      (SQLite VDBE,   (copy-and-patch,        (Umbra     (HyPer,
  AST node)      Postgres        OOPSLA'21)              Tidy       Postgres
                 ExprState)                              Tuples,    jit=on)
                                                         cranelift)
 compile: 0      ~0              ~µs                     ~100µs      ~10-100ms
 run:     1×     ~2-5×           ~10×                    ~10-30×     ~10-60×
```

Every step right buys execution speed with compilation latency.
The entire topic is that trade — and the reason Postgres's LLVM JIT
is *often a regression* (§5): it sits at the far right where compile
cost is milliseconds, gated by a planner cost heuristic that
routinely misfires.

```mermaid
flowchart LR
    Q[query arrives] --> D{expected work?}
    D -->|one row, OLTP| I[interpret / bytecode\ncompile cost 0]
    D -->|millions of rows| J[JIT\namortize compile over rows]
    D -->|unknown| A[adaptive: start interpreting,\ncompile in background, swap in]
    style A fill:#e8f5e9
```

Adaptive execution (ICDE'18) is the escape hatch Umbra ships:
never pay compile latency up front, never miss the JIT win on long
queries.

## 2. SQLite's VDBE — the bytecode VM that refuses to die

[`~/repos/sqlite/src/vdbe.c`](https://github.com/sqlite/sqlite) — one giant dispatch loop
(vdbe.c:1049 `switch( pOp->opcode )`), 190 top-level `case OP_` opcodes
(`grep -c 'case OP_'` says 199 — nine of those are inner-switch cases and
a doc comment), each op a fixed struct (src/vdbe.h:55 `struct VdbeOp`:
opcode + p1..p5 operands). `EXPLAIN SELECT ...` prints the program.

```
 SELECT a+1 FROM t WHERE b < 10;
   addr  opcode        p1  p2  p3
   0     Init          0   8
   1     OpenRead      0   2       ← cursor on table t
   2     Rewind        0   7
   3     Column        0   1   r1  ← b into register 1
   4     Ge            r1  6       ← if b >= 10 skip
   5     Column+Add    …           ← a+1 into result register
   6     ResultRow
   7     Next          0   3       ← loop
```

Why bytecode and not a tree walker? The flattened program is
*resumable* (a coroutine — OP_Yield at vdbe.c:1264 powers
`INSERT ... SELECT` without materializing), *inspectable*, and the
dispatch is one indirect branch per op instead of a virtual call
per AST node. Why not JIT? SQLite's queries touch a handful of rows
— column (a) of the flowchart above, compile cost can never
amortize. Guide: [reading-sqlite-vdbe.md](reading-sqlite-vdbe.md).

## 3. Produce/consume (Neumann VLDB'11) — compile the PIPELINE, not the operators

The paper's insight: iterator-model `next()` calls are the cost, so
don't compile operators that call each other — fuse each pipeline
into ONE tight loop where tuples stay in registers.

```
 σ → Γ → ⋈ plan          generated code (one pipeline):
                          for tuple in scan:          ← produce
 each operator gets         if pred(tuple):           ← σ consume
 produce()/consume();       ht.insert(tuple)          ← Γ consume
 codegen walks the        (pipeline breaker: hash table materializes;
 tree ONCE, emits          next pipeline starts a new loop)
 nested control flow
```

Data flows *upward through registers*, control flow is inverted
(push, not pull) — exactly topic 11's push-vs-pull, but the pushing
is done by generated code with zero interpretation. Guide:
[reading-neumann-vldb11.md](reading-neumann-vldb11.md).

## 4. Umbra's Tidy Tuples & copy-and-patch — attacking compile LATENCY

HyPer used LLVM and ate 10-100 ms compiles. Umbra's answer
(VLDBJ'21): a custom low-level IR designed for *single-pass*
lowering — the query translates to IR to machine code in one linear
sweep, ~100× faster compiles at ~70-80% of LLVM -O3 speed, with
LLVM kept as the top adaptive tier. Copy-and-patch (OOPSLA'21) goes
further: precompile a library of binary "stencils" (one per
operator/type combo, holes for constants), then "compilation" is
memcpy + patching holes — microseconds. Guide:
[reading-umbra-tidy-tuples.md](reading-umbra-tidy-tuples.md).

## 5. Postgres's LLVM JIT — a cautionary tale

[`~/repos/postgres/src/backend/jit/llvm/`](https://github.com/postgres/postgres) — expression + tuple-deform
JIT only (NOT whole-pipeline: the executor stays interpreted;
llvmjit_expr.c:80 `llvm_compile_expr` compiles `ExprState` step
arrays, emitting one basic block per step, llvmjit_expr.c:302-307).
Two LLJIT instances at opt0/opt3 (llvmjit.c:100-101). Gated by
`jit_above_cost` = 100000, with `jit_optimize_above_cost` = 500000
choosing the opt3 instance and `jit_inline_above_cost` = 500000
enabling cross-module inlining (guc_parameters.dat:1458+) — all three
are *planner cost estimate* thresholds, not measured times. And at this
pin the `jit` GUC itself boots to **false** (guc_parameters.dat:1451-1456,
`variable => 'jit_enabled'`), so the cautionary tale ends with the
project agreeing: the failure mode is that the estimate says expensive,
the query is short, and you pay tens of ms of LLVM for a 5 ms query.
Guide: [reading-postgres-jit.md](reading-postgres-jit.md).

## 6. GraphBLAS's JIT — compile the KERNEL, cache it forever

SuiteSparse takes a third road: the JIT unit is not a query but a
*kernel specialization* (semiring × types × sparsity formats).
`Source/jitifyer/GB_jitifyer.c` — encode the problem to a hash
(GB_encodify_mxm.c:58-61), look up an in-memory hash table
(GB_jitifyer.c:2122), fall back to an on-disk cache of compiled
`.so` files, fall back to invoking THE C COMPILER at runtime and
`dlopen`ing the result (GB_jitifyer.c:1576,1937). Compile once per
type-combo ever, not per query — amortization across the process
lifetime, not across rows. FalkorDB inherits this whole machinery.
Guide: [reading-graphblas-jit.md](reading-graphblas-jit.md).

## 7. And DuckDB has NO JIT — on purpose

The counter-argument, worth stating precisely: vectorization already
amortizes interpretation to ~nothing (topic 11's measured ~10-40×),
a JIT adds a compiler dependency + compile latency + a security
surface, and VLDB'18 ("Everything You Always Wanted to Know…")
measured compiled vs vectorized within ~2× of each other on most of
TPC-H — vectorized even *wins* on hash-join-heavy queries (better
memory parallelism from batched probes). JIT's clear wins: complex
*expressions* (compute-heavy scalar code) and data-centric loops
LLVM can keep in registers. Hence M19 JITs *expressions only* —
the eval.rs interpreter is the FalkorDB analogue of ExprState.

## 8. cranelift — the build tool

[`~/repos/cranelift-jit-demo/src/jit.rs`](https://github.com/bytecodealliance/cranelift-jit-demo) is the whole recipe (461
lines): JITBuilder/JITModule (:39-41), FunctionBuilder translates
AST→CLIF IR (:135, FunctionTranslator at :187-192), then
declare→define→finalize→pointer (`compile()` :53-93). Cranelift sits at
Umbra's design point: fast single-pass compiles, decent code, pure Rust.
For what "fast single-pass" is worth against LLVM, the sourced numbers
are Umbra's own (Table 3: 108× the compile speed of LLVM -O3 for code
1.2× slower) rather than a folk figure.
Guide: [reading-cranelift-jit-demo.md](reading-cranelift-jit-demo.md).

## Experiments (`experiments/`)

Three-way expression executor over f64 columns — the PLAN §19 bench:

| file | role |
|---|---|
| src/expr.rs | PROVIDED — `Expr` tree (Col/Const/Add/Mul/Lt/And) + seeded random generator |
| src/interp.rs | PROVIDED — AST-walking `eval(expr, row)` (the strawman) |
| src/vectorized.rs | PROVIDED — column-at-a-time batch eval (topic 11's answer) |
| src/jit.rs | **STUB** — cranelift: compile `Expr` → `fn(*const f64) -> f64` |
| src/bin/jit_bench.rs | PROVIDED — interpreter vs vectorized vs JIT, rows/s + compile µs, depth × rows sweep |

```bash
cd topics/19-jit/experiments
cargo test              # provided tests green; jit tests panic until implemented
cargo run --release --bin jit_bench
```

Predict before you run (notes.md): at which (depth, rows) does JIT
beat vectorized? Where does compile time drown it?

## M19 (capstone)

- [ ] cranelift JIT for Cypher expressions vs eval.rs interpreter
- [ ] fallback path (unsupported expr node → interpreter, never fail)
- [ ] compile-time budget heuristic — measured, not estimated
      (postgres's lesson: gate on *actual rows seen*, adaptive-style,
      not on a planner estimate)

## Reading order

1. reading-neumann-vldb11.md — the model
2. reading-sqlite-vdbe.md — the bytecode floor
3. reading-umbra-tidy-tuples.md — compile-latency war (+ copy-and-patch)
4. reading-postgres-jit.md — how it goes wrong in production
5. reading-graphblas-jit.md — kernel-grain JIT (FalkorDB's inheritance)
6. reading-cranelift-jit-demo.md — then implement the stub
