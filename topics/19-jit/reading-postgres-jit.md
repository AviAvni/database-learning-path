# Postgres's LLVM JIT: why everyone sets jit=off

The production cautionary tale. Postgres 11+ ships an LLVM JIT for
*expressions and tuple deforming only* — the executor loop stays
interpreted — and it is famous mostly for the advice "set jit=off".
This chapter builds the machinery step by step — what Postgres
interprets, what the JIT actually compiles, the underrated deforming
half, and the cost-model gate whose four failure modes are the
lesson — then maps every step to the ~3 files under
`src/backend/jit/llvm/`.

## The problem in one sentence

Postgres decides whether to spend 10–100 ms of LLVM compilation
using a *planner cost estimate* made before a single row is read —
so when the estimate says "expensive" and the query takes 5 ms, you
pay 50 ms of compile for nothing, and enough users hit that to make
"try jit=off" standard ops advice.

## The concepts, step by step

### Step 1 — ExprState: Postgres already has bytecode

Before any JIT enters the picture, Postgres does not tree-walk
expressions per row. At plan time a WHERE clause or projection is
flattened into an **ExprState**: a contiguous array of small *steps*
(opcodes like EEOP_FUNCEXPR "call this function", EEOP_QUAL "test
and jump out if false"), executed by a threaded-dispatch interpreter
(execExprInterp.c — computed goto, one indirect branch per step).
This is the same design as SQLite's VDBE at expression grain:
flatten once, dispatch per step per row. So the JIT's opponent is
already a decent bytecode interpreter, not a strawman — the win on
offer is only the per-step dispatch plus what a compiler can see
across steps.

### Step 2 — what the JIT compiles: one basic block per step

The JIT's scope is deliberately narrow:

```
 NOT compiled: executor nodes (SeqScan, HashJoin...) — still the
               interpreted node->ExecProcNode indirection
 compiled:     ExprState step arrays (WHERE clauses, projections,
               aggregates' transition expressions)
               + tuple DEFORMING (attribute extraction — schema-
               specialized: known offsets, nullability)
```

`llvm_compile_expr` (llvmjit_expr.c:80) translates each step of one
ExprState into one **basic block** (a straight-line chunk of code
with one entry and one exit — LLVM's unit of control flow), wires
the blocks together in step order, and lets LLVM fold the dispatch
away — the indirect branch the interpreter pays per step becomes a
fallthrough:

```rust
// llvm_compile_expr's shape: one basic block per interpreter step —
// the dispatch the interpreter pays per step becomes a fallthrough
let opblocks: Vec<Block> = state.steps.iter().map(|_| new_block()).collect();
for (i, step) in state.steps.iter().enumerate() {
    position_at(opblocks[i]);
    match step.opcode {
        EEOP_QUAL          => emit_cmp_and_branch(step, opblocks[step.jumpdone]),
        EEOP_FUNCEXPR      => emit_direct_call(step.fn_addr, step.args),
        EEOP_SCAN_FETCHSOME => emit_deform(tupledesc, step.last_attr),
        // ... the giant switch mirrors execExprInterp.c case by case
    }
    emit_branch(opblocks[i + 1]);   // then LLVM folds blocks together
}
```

Structurally the SAME translation our stub does for `Expr` → CLIF —
postgres just starts from bytecode instead of an AST. It is NOT
Neumann's whole-pipeline compilation: operators still call each
other through interpreted indirection; only the leaves got fast.

### Step 3 — tuple deforming: the underrated half

**Deforming** is extracting attribute values from Postgres's
on-disk row format — variable-length fields, a null bitmap, and
alignment padding mean that reaching column 19 requires walking
columns 1–18, testing the null bitmap at each. The generic decoder
(`slot_deform_heap_tuple`) re-discovers the schema per row.
llvmjit_deform.c instead generates a decoder *specialized to the
schema*: attribute offsets constant-folded, null-bitmap checks
skipped for NOT NULL columns, alignment known. This routinely beats
the expression JIT in profit because deforming is
per-ROW-per-ATTRIBUTE and pure branchy pointer math — the same
reason topic 12's PAX/columnar layouts win, arrived at from the
compiler side.

### Step 4 — the gate: a cost estimate decides, and misfires four ways

Compilation triggers when the planner's estimated total cost — an
abstract unitless number built from row-count guesses (topic 10) —
crosses a GUC threshold:

```
 planner.c:699:  use JIT iff estimated total_cost > jit_above_cost
                                    (default 100000)

 failure 1: estimate high, reality short → pay ~10-100ms LLVM
            for a fast query   (the classic complaint)
 failure 2: cost is in COST UNITS not ms — jit_above_cost has no
            unit relationship with compile time on this machine
 failure 3: decision is per-QUERY, all-or-nothing, made BEFORE
            any row is seen — no adaptivity (contrast Umbra)
 failure 4: opt3 is gated by ANOTHER estimate (jit_optimize_above_
            cost) — two thresholds to mistune
```

There's a partial mitigation: two LLJIT tiers (opt0/opt3,
llvmjit.c:100-101 — LLJIT is LLVM's JIT engine; opt0 compiles fast
and slow, opt3 slow and fast) — but tier choice is still
estimate-driven. This is the actual lesson of the chapter: the
compile-or-not decision is a bet, and Postgres places it with the
least reliable number in the system.

### Step 5 — lifecycle plumbing worth stealing

JIT-compiled code is memory that something must own. llvmjit.c:716+
compiles modules into a dylib with a resource tracker per
compilation; llvmjit.c:288-299 shows teardown (remove tracker,
clear dead symbol-pool entries). Ownership is per-query-context:
when the query dies, the code dies — no dangling function pointers.
M19 note: cranelift's `JITModule` has the same `free_memory`
obligation — our stub keeps the module alive inside `CompiledExpr`
so the fn pointer can't dangle.

### Step 6 — what transfers to M19

- Compile the *expression*, keep the executor: exactly M19's scope.
- Gate on MEASURED cost (rows already processed × measured ns/row
  vs measured compile µs), not an estimate — Umbra's lesson applied
  to Postgres's failure.
- Deforming lesson: FalkorDB's property access (attribute fetch
  from the property store) is the deform-analogue — likely more
  profit than arithmetic JIT.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| llvmjit.c:156 | provider hook: `cb->compile_expr = llvm_compile_expr` | 2 |
| llvmjit_expr.c:80 | `llvm_compile_expr(ExprState*)` — the entry point | 2 |
| llvmjit_expr.c:302-307 | one LLVM basic block per ExprState step (`opblocks`) | 2 |
| llvmjit_expr.c:326+ | the giant `case EEOP_*` switch — mirror of the interpreter | 1–2 |
| llvmjit_expr.c:354+ | EEOP_*_FETCHSOME → JIT tuple deforming (llvmjit_deform.c) | 3 |
| planner.c:699-700 | the gate: `top_plan->total_cost > jit_above_cost` | 4 |
| llvmjit.c:85-101 | session state: two LLJITs — `llvm_opt0_orc` / `llvm_opt3_orc` | 4 |
| llvmjit.c:363 | `llvm_get_function` — lookup + (lazy) emission | 5 |
| llvmjit.c:716-781 | module → ThreadSafeModule → LLJIT dylib + resource tracker | 5 |

Pair llvmjit_expr.c with `src/backend/executor/execExprInterp.c`
side by side — every `case EEOP_*` in the JIT mirrors a case in the
interpreter, and seeing what each block replaces is Step 1 and
Step 2 in one diff. Then read planner.c:699 for the gate and
llvmjit.c for the lifecycle.

## Questions for notes.md

1. Trace one EEOP through both executors: find EEOP_QUAL in
   execExprInterp.c and in llvmjit_expr.c. What does LLVM get to
   do that the interpreter can't (cross-step constant prop, dead
   null-check elimination)?
2. Why does the JIT emit ONE function per ExprState with a block
   per step, rather than one function per step (call overhead +
   register state across steps — the copy-and-patch contrast)?
3. jit_above_cost is in planner cost units. Propose the fix
   postgres upstream keeps debating: what would a *time-based*
   gate need to know (compile-time model per step count + rows
   estimate — and which half is still an estimate)?
4. Deform JIT: for a 20-column table where the query touches
   column 19, what does the generated decoder skip vs the generic
   `slot_deform_heap_tuple`, and which topic 12 layout makes the
   whole problem vanish?
5. For M19: postgres compiles per-query with no cache. GraphBLAS
   caches per type-combo forever (reading-graphblas-jit.md).
   Which is right for Cypher expressions, and what's the cache
   key (expression shape with constants as parameters — count how
   many distinct shapes a workload of 1000 queries has)?

## References

**Code**
- [postgres](https://github.com/postgres/postgres) —
  `src/backend/jit/llvm/` — llvmjit.c (lifecycle), llvmjit_expr.c
  (the EEOP switch), llvmjit_deform.c (the underrated half); pair
  with `src/backend/executor/execExprInterp.c` to see what each
  EEOP block replaces, and `planner.c:699` for the gate
