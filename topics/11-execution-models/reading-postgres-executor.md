# Volcano in production: postgres's executor, warts and wisdom

Tuple-at-a-time execution, still shipping: postgres's executor is the
honest per-tuple baseline your benchmark's `volcano.rs` models. Before
the code, this chapter builds the iterator model and its two dispatch
costs — a function pointer per plan node per tuple, an opcode per
expression step — one concept at a time, ending at the one place
postgres already fought back (the computed-goto expression interpreter).
Then it hands you the file:line anchors.

## The problem in one sentence

Postgres pays ~1 indirect function call per plan node per tuple plus an
interpreted opcode per expression step per tuple — negligible for a
3-row OLTP lookup, but a 5-node plan over 100M rows burns 500M indirect
branches before any useful work happens.

## The concepts, step by step

### Step 1 — the iterator (Volcano) model: `next()` returns one tuple

In the Volcano model (Graefe, 1990), every operator — scan, filter,
aggregate, join — implements the same three-call interface:
`open() / next() / close()`, where `next()` returns exactly ONE tuple
(row). The root operator's `next()` calls its child's `next()`, which
calls *its* child's, down to the scan:

```
 Project.next()
   └─ calls Agg.next()
        └─ calls Filter.next()      per-tuple costs, PER TUPLE:
             └─ calls Scan.next()   - virtual call (indirect branch) x depth
                                    - interpretation of the expression tree
                                    - tuple is gone from registers between calls
```

The elegance is real: operators compose arbitrarily (any tree of
next()-speaking boxes works), execution is demand-driven (a LIMIT stops
pulling and everything upstream stops), and memory stays bounded (one
tuple in flight per operator). The cost is the subject of this guide —
and of this entire topic.

### Step 2 — the price: an indirect call per node per tuple

In postgres, "call the child's next()" is `ExecProcNode(node)`, which is
just `return node->ExecProcNode(node);` — a call through a function
pointer (an **indirect call**: the target address is data, loaded at
runtime, so the CPU must predict where it's going; a misprediction
flushes the pipeline for ~15 cycles). That's one per plan node per
tuple: a 5-node plan over 100M rows = **500M indirect branches** before
any work happens. Worse, between two `next()` calls the tuple's values
leave CPU registers entirely — every operator re-loads what its child
just had in hand. At ~20 ns of such overhead per tuple per operator,
100M rows × 5 operators = minutes spent NOT computing. This is the
number vectorization divides by 2048.

### Step 3 — a production wart worth stealing: self-modifying dispatch

Postgres's node dispatch has a cute optimization: every node is
*initialized* with its function pointer set to `ExecProcNodeFirst`, a
wrapper that performs one-time checks (stack depth, instrumentation
setup) and then REPLACES the node's pointer with the real
`ExecProcNodeReal` — so the steady-state path never pays for the checks
again. Self-modifying dispatch: the first call does setup, then swaps
itself out. You've seen the pattern as lazy statics and memoized FFI
resolution (question 3 below).

### Step 4 — tuple slots: paying for deforming per access

Tuples travel between operators as `TupleTableSlot` — an abstraction
over heap tuples (the on-disk packed format), minimal tuples, and
virtual tuples (just an array of column pointers). The catch: attribute
access may **deform** the tuple — unpack the packed on-disk bytes to
find column k, which requires walking columns 1..k−1 when earlier
columns are variable-length. Postgres pays this per attribute access,
per tuple; a vectorized engine deforms once per column per 2048-row
chunk and then works on flat arrays. Same work, amortized 2048×.

### Step 5 — expressions as flat steps, dispatched by computed goto

Expressions (`a.x + 1 > b.y`) are the second interpretation layer, and
here postgres already fought back. Instead of walking the expression
*tree* per tuple (recursive calls mirroring the syntax), postgres
compiles each expression once, at plan time, into a linear array of
**steps** — opcodes like "fetch attribute 2", "add", "compare" — then
interprets that flat program per tuple:

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

Two refinements in the real thing. First, where the compiler supports
it, dispatch is a **computed goto** (each opcode's implementation ends
with `goto *dispatch_table[op->opcode]` rather than looping back to one
central `switch`): every opcode *site* gets its own branch-predictor
entry, which learns "an AddI64 here is usually followed by GtI64" —
where a single switch's one shared indirect branch predicts far worse.
The classic interpreter trick (same reason redis' RESP parsing stays
cheap; it's exactly what a JIT removes entirely — topic 19). Second, a
peephole: step patterns matching common shapes (fetch-inner +
fetch-outer + compare) get dedicated fast-path routines that skip the
interpreter altogether.

### Step 6 — the ladder, and why postgres gets away with it

Linearizing the expression is *half* of vectorization — postgres just
still applies it one tuple at a time:

```
 tree-walk interpreter      linear-step interpreter     vectorized kernel
 (recursive, per tuple)     (flat, per tuple)           (flat, per 2048)
        slowest        →        postgres          →        DuckDB
                                    ↘ JIT (topic 19) compiles the steps
```

Why it survives: for OLTP, per-tuple overhead × 3 tuples is nothing, and
the buffer manager / WAL / locking dominate writes anyway. For analytics
it does NOT get away with it — that's the market gap DuckDB drove a
truck through. (JIT via LLVM exists for expressions — `jit_above_cost` —
but not for the operator loop.)

## Where each step lives in the code

- **Steps 1–2**: `src/include/executor/executor.h:322` —
  `ExecProcNode(node)` is just `return node->ExecProcNode(node);` — the
  indirect call per tuple per node.
- **Step 3**: `src/backend/executor/execProcnode.c:439` — nodes
  initialized with `ExecProcNode = ExecProcNodeFirst` (`:448`), the
  wrapper doing one-time checks (stack depth `:457`, instrumentation)
  then replacing the pointer with `ExecProcNodeReal`.
- **Step 4**: `TupleTableSlot` — follow it from any node's
  `ExecProcNodeReal`; watch for `slot_getattr` deforming.
- **Step 5**: `src/backend/executor/execExprInterp.c` — read the `:14`
  header comment first. Computed-goto dispatch at `:86–:126`
  (`EEO_SWITCH`/`EEO_CASE`, `:119–:126`); `ExecInterpExpr` `:146` — the
  giant opcode loop itself; the peephole fast paths at `:300`.

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
