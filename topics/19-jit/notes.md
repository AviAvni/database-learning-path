# Topic 19 notes — JIT & query compilation

## Baseline (provided interpreter + vectorized, Apple M3 Pro, measured 2026-07-10)

jit_bench, N_COLS=4, best-of-3, Mrows/s. Full binary trees so node
count = 2^(depth+1)-1.

| depth | nodes | interp M/s | vector M/s | vector/interp |
|---|---|---|---|---|
| 2 | 7 | 60-94 | 380-518 | ~6× |
| 4 | 31 | ~19.5 | 150-186 | ~8× |
| 6 | 127 | ~4.1 | 39-54 | ~11× |
| 8 | 511 | ~0.95 | 10-13 | ~12× |
| 10 | 2047 | ~0.24 | 2.6-3.2 | ~12× |

Both lanes scale ~linearly in node count (interp: ~0.95 M/s × 511
nodes ≈ 485 Mnodes/s ≈ 2.1 ns/node — a match dispatch + 2 calls;
vector: ~12 M/s × 511 ≈ 6 Gnode-rows/s ≈ node cost dominated by
materializing temporaries, ~16 B/row/node of memory traffic).
Vectorized flat at ~10-12× from depth 6 up: the topic 11 number,
reproduced. Row count barely matters — no lane has a fixed cost
yet. Compile time will change that: the JIT lane is the only one
with a y-intercept.

## Predictions (fill BEFORE implementing jit.rs)

| question | prediction | actual |
|---|---|---|
| cranelift compile µs for depth 8 (511 nodes)? linear in nodes? | | |
| JIT run-only M/s at depth 8 (vs vector ~12, interp ~0.95)? | | |
| break-even rows vs INTERP at depth 8 (compile/(µs_i−µs_j)) | | |
| break-even rows vs VECTORIZED at depth 8 — does 2M rows clear it? | | |
| depth 2 tiny expr: does JIT ever win e2e at ≤2M rows? | | |
| does JIT beat vectorized per-ROW at all (no temporaries, but scalar vs autovec SIMD)? | | |

Reasoning to check later: JIT straight-line f64 code ≈ 1 FMA-ish op
per node with ILP ⇒ maybe 3-6 Gnode/s single lane ⇒ depth 8 ≈ 6-12
M/s — comparable to vectorized, NOT clearly faster, because
vectorized gets SIMD from autovec and JIT emits scalar. The honest
VLDB'18 conclusion, predicted before measuring.

## Implementation log

- [ ] jit.rs compile() — both tests green
- [ ] jit_bench full three-way table + crossover rows
- [ ] compile-time-vs-nodes linearity measured (hoist ISA/module setup, measure both ways)
- [ ] stretch: 4th lane — bytecode VM for Expr (sqlite-vdbe guide Q5)
- [ ] prediction table reconciled

Surprises / dead ends:

## Questions from the reading guides

### Neumann VLDB'11 (reading-neumann-vldb11.md)

1. Pipelines for the Cypher plan / what M19 compiles vs produce-consume:
2. Why push gives ONE loop, pull needs suspendable state:
3. Cocktail rule applied to our executor:
4. Tuple-in-registers dies at what width:
5. VLDB'18 hash-probe MLP argument:

### SQLite VDBE (reading-sqlite-vdbe.md)

1. EXPLAIN program for a+1 WHERE b<10, ops/row:
2. Register vs stack machine op counts for a*b+c*d:
3. OP_Yield pc-swap — where do locals live:
4. Why OP_Column is huge and OP_Add tiny:
5. Bytecode 4th lane prediction:

### Umbra / copy-and-patch (reading-umbra-tidy-tuples.md)

1. Three Umbra-IR choices enabling single-pass:
2. Why linear-scan regalloc is fine for query code:
3. musttail stencil composition ↔ wgpu pipeline specialization:
4. What state interp/compiled must agree on at swap:
5. Break-even formula with measured numbers; 1M-node WHERE verdict:

### Postgres JIT (reading-postgres-jit.md)

1. EEOP_QUAL in both executors — what LLVM folds:
2. One function per ExprState vs per step:
3. Time-based gate — which half stays an estimate:
4. Deform JIT vs generic for col 19 of 20:
5. Per-query vs per-shape cache for Cypher:

### GraphBLAS JIT (reading-graphblas-jit.md)

1. Generic mxm fn-pointer cost vs JITed FMA:
2. Critical-section scope / benign duplicate compiles:
3. Why unbounded cache is fine (key space size for FalkorDB):
4. PreJIT ↔ copy-and-patch stencils:
5. Cypher expression cache key (shape vs parameter):

### cranelift-jit-demo (reading-cranelift-jit-demo.md)

1. Why define ≠ callable (relocations); which Expr nodes need them:
2. FunctionBuilder SSA — block params vs phis:
3. compile() linearity + constant term:
4. Transmute soundness preconditions:
5. Cypher f64-subset + fallback boundary choice:

## Cross-topic threads

- The whole topic is topic 11's dispatch-amortization argument with
  a third strategy: vectorize amortizes per-batch, JIT eliminates.
  VLDB'18 says they tie ~2×; our bench should reproduce that.
- Vectorized's per-node temporary vector = topic 18's cudf
  over-allocate answer; JIT keeps values in registers = Neumann's
  claim = Tidy Tuples' value tracking.
- GraphBLAS's hash→dlopen ladder = topic 6's buffer-pool
  lookup-then-fault pattern, for code instead of pages.
- postgres jit_above_cost misfiring = topic 10's cardinality-
  estimate fragility, exported into the executor.
- copy-and-patch stencils with musttail = topic 17's fixed
  compare-exchange networks: precommit the schedule, kill dispatch.

## M19 log (capstone)

- [ ] cranelift JIT for Cypher expressions vs eval.rs interpreter
- [ ] fallback: unsupported node → interpreter (GraphBLAS generic-
      kernel contract: never fail, only slower)
- [ ] budget heuristic from MEASURED numbers (rows seen × ns/row vs
      measured compile µs) — postgres's estimate-gate is the
      anti-pattern; cache compiled exprs by shape, params excluded
      (GraphBLAS encodify lesson)

## Done when

- Both jit.rs tests green; three-way table + crossovers in notes;
  prediction table reconciled; reading-guide questions answered;
  M19 verdict written.
