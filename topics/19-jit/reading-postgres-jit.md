# Reading guide — Postgres LLVM JIT ([`~/repos/postgres/src/backend/jit/llvm/`](https://github.com/postgres/postgres))

The production cautionary tale. Postgres 11+ ships an LLVM JIT for
*expressions and tuple deforming only* — the executor loop stays
interpreted — and it is famous mostly for the advice "set jit=off".
Read it to learn exactly where the spectrum bites.

## Anchor map

| anchor | what it is |
|---|---|
| llvmjit.c:156 | provider hook: `cb->compile_expr = llvm_compile_expr` |
| llvmjit.c:85-101 | session state: two LLJITs — `llvm_opt0_orc` / `llvm_opt3_orc` |
| llvmjit.c:363 | `llvm_get_function` — lookup + (lazy) emission |
| llvmjit.c:716-781 | module → ThreadSafeModule → LLJIT dylib + resource tracker |
| llvmjit_expr.c:80 | `llvm_compile_expr(ExprState*)` — the entry point |
| llvmjit_expr.c:302-307 | one LLVM basic block per ExprState step (`opblocks`) |
| llvmjit_expr.c:326+ | the giant `case EEOP_*` switch — mirror of the interpreter |
| llvmjit_expr.c:354+ | EEOP_*_FETCHSOME → JIT tuple deforming (llvmjit_deform.c) |
| planner.c:699-700 | the gate: `top_plan->total_cost > jit_above_cost` |

## 1. What is actually compiled

```
 NOT compiled: executor nodes (SeqScan, HashJoin...) — still the
               interpreted node->ExecProcNode indirection
 compiled:     ExprState step arrays (WHERE clauses, projections,
               aggregates' transition expressions)
               + tuple DEFORMING (attribute extraction — schema-
               specialized: known offsets, nullability)
```

ExprState is postgres's bytecode: a flat array of steps
(EEOP_FUNCEXPR, EEOP_QUAL, ...) run by a threaded-dispatch
interpreter (execExprInterp.c — computed goto). The JIT translates
each step to a basic block (opblocks, llvmjit_expr.c:302-307) and
lets LLVM fold the dispatch away. Structurally the SAME translation
our stub does for `Expr` → CLIF — postgres just starts from
bytecode instead of an AST.

## 2. The cost model failure (the actual lesson)

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
llvmjit.c:100-101) — but tier choice is still estimate-driven.

## 3. Tuple deforming — the underrated half

llvmjit_deform.c generates a schema-specialized decoder: attribute
offsets constant-folded, null-bitmap checks skipped for NOT NULL
columns, alignment known. This routinely beats the expression JIT
in profit because deforming is per-ROW-per-ATTRIBUTE and pure
branchy pointer math — the same reason topic 12's PAX/columnar
layouts win, arrived at from the compiler side.

## 4. Lifecycle plumbing worth stealing

llvmjit.c:716+ — modules are compiled into a dylib with a
resource tracker per compilation; llvmjit.c:288-299 shows teardown
(remove tracker, clear dead symbol-pool entries). Memory for JITed
code is owned per-query-context: when the query dies, the code
dies. M19 note: cranelift's `JITModule` has the same
`free_memory` obligation — our stub keeps the module alive inside
`CompiledExpr` so the fn pointer can't dangle.

## 5. What transfers to M19

- Compile the *expression*, keep the executor: exactly M19's scope.
- Gate on MEASURED cost (rows already processed × measured ns/row
  vs measured compile µs), not an estimate.
- Deforming lesson: FalkorDB's property access (attribute fetch
  from the property store) is the deform-analogue — likely more
  profit than arithmetic JIT.

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
