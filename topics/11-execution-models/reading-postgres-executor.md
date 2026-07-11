# Volcano in production: postgres's executor, warts and wisdom

Tuple-at-a-time execution, still shipping: postgres's executor is the
honest per-tuple baseline your benchmark's `volcano.rs` models. Read it
for the two dispatch costs — a function pointer per plan node per tuple,
an opcode per expression step — and for the one place postgres already
fought back (the computed-goto expression interpreter).

## 1. ExecProcNode: the iterator model in one function pointer

- `src/include/executor/executor.h:322` — `ExecProcNode(node)` is just
  `return node->ExecProcNode(node);` — an indirect call PER TUPLE per
  plan node. A 5-node plan over 100M rows = 500M indirect branches
  before any work happens.
- `src/backend/executor/execProcnode.c:439` — the cute part: nodes are
  initialized with `ExecProcNode = ExecProcNodeFirst` (`:448`), a
  wrapper that does one-time checks (stack depth `:457`,
  instrumentation) then REPLACES the pointer with `ExecProcNodeReal` —
  self-modifying dispatch, so the steady-state path skips the checks.
- Tuples travel as `TupleTableSlot` — an abstraction over
  heap/minimal/virtual tuples; every attribute access may deform
  (unpack) the on-disk tuple. Vectorized engines pay deforming once per
  column per chunk; postgres pays per access.

## 2. execExprInterp.c: the fight against interpretation overhead

Expressions (`a.x + 1 > b.y`) are compiled to a linear array of STEPS,
then interpreted:

- `:14` and `:86–:126` — dispatch is a **computed goto** where the
  compiler supports it (`EEO_SWITCH`/`EEO_CASE`, `:119–:126`): each
  opcode's implementation ends with `goto *dispatch_table[op->opcode]`.
  One indirect branch per step, but each opcode site gets its OWN branch
  predictor entry (vs a single switch's shared one) — the classic
  interpreter trick (same reason redis' RESP parsing stays cheap, and
  the thing JIT removes entirely — topic 19).
- `:146` — `ExecInterpExpr`: the giant opcode loop itself.
- `:300` — peephole: if the step pattern matches common shapes
  (e.g. fetch-inner + fetch-outer + compare), dedicated fast-path
  routines skip the interpreter entirely.
- Flat steps instead of tree-walking: postgres ALREADY did the
  "linearize the expression" half of vectorization — it just still
  applies it one tuple at a time.

```rust
// expressions compile to FLAT STEPS, then interpret — once per tuple
fn interp(steps: &[Step], row: &Row, regs: &mut [Datum]) -> Datum {
    let mut ip = 0;
    loop {
        match steps[ip].op {           // in C: goto *dispatch[op] — each
            FetchAttr(a, r) => regs[r] = row.attr(a),   // opcode SITE gets
            AddI64(x, y, r) => regs[r] = regs[x] + regs[y], // its own branch-
            GtI64(x, y, r)  => regs[r] = (regs[x] > regs[y]).into(), // predictor
            Done(r)         => return regs[r],              // entry
        }
        ip += 1;
    }
}
// vectorization = the SAME flat steps, applied per 2048 rows instead
```

```
 tree-walk interpreter      linear-step interpreter     vectorized kernel
 (recursive, per tuple)     (flat, per tuple)           (flat, per 2048)
        slowest        →        postgres          →        DuckDB
                                    ↘ JIT (topic 19) compiles the steps
```

## 3. Why postgres gets away with it

- OLTP: per-tuple overhead × 3 tuples is nothing.
- The buffer manager / WAL / locking dominate anyway for writes.
- For analytics it does NOT get away with it — that's the market gap
  DuckDB drove a truck through. (JIT via LLVM exists for expressions
  — `jit_above_cost` — but not for the operator loop.)

## Questions for notes.md

1. Count the indirect branches per tuple for
   `SELECT sum(x) FROM t WHERE y > 10`: plan nodes × 1 + expression
   steps. Then per 2048 tuples for the DuckDB equivalent.
2. Computed goto vs switch: WHY does one predictor entry per opcode site
   help? (Think topic 0's branch_misprediction bench.)
3. `ExecProcNodeFirst`'s pointer swap is bit-smuggling's cousin —
   self-modifying dispatch. Where else have you seen
   "first call does setup, then replaces itself"? (Hint: lazy statics,
   memoized FFI resolution.)
4. M11: your eval.rs will interpret property predicates over batches.
   Linear steps or closure tree? What does postgres' `:300` peephole
   suggest about the 3 shapes worth special-casing for Cypher
   (`n.prop = lit`, `n.prop > lit`, label check)?

## Done when

You can explain the two dispatch costs (node-level ExecProcNode,
step-level opcode) and name the mitigation for each (vectorization /
computed goto + JIT).

## References

**Code**
- [postgres](https://github.com/postgres/postgres) —
  `src/backend/executor/`: `execProcnode.c` (the dispatch),
  `execExprInterp.c` (the computed-goto interpreter — read the :14
  header comment first), plus `src/include/executor/executor.h`; ~1 h
