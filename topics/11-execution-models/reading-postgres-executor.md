# Volcano in production: postgres's executor, warts and wisdom

Tuple-at-a-time execution, still shipping: postgres's executor is the
honest per-tuple baseline your benchmark's `volcano.rs` models. Before
the code, this chapter builds the iterator model and its two dispatch
costs — a function pointer per plan node per tuple, an opcode per
expression step per tuple — one concept at a time, ending at the one
place postgres already fought back (the computed-goto expression
interpreter). Then it hands you the file:line anchors.

Every anchor below is postgres master at commit `701f021` — the commit
this repo pins, which `configure.ac:20` calls `20devel` — quoted with the
line numbers the code occupies in that tree.

## The problem in one sentence

Postgres pays one indirect function call per plan node per tuple plus an
interpreted opcode per expression step per tuple — negligible for a
3-row OLTP lookup, and a 5-node plan over 100M rows burns 500M indirect
branches before any useful work happens.

## The concepts, step by step

### Step 1 — the iterator (Volcano) model: `next()` returns one tuple

> **In:** nothing yet — this step fixes the vocabulary every later step
> costs out.
> **Out:** the three-call operator interface, and the one design decision
> (the unit that crosses it) that Steps 2 to 7 price.

An **operator** is one node of a physical plan — a scan, a filter, an
aggregate, a join — that consumes rows and produces rows. In the
**Volcano model** (Graefe, 1990; also called the **iterator model**),
every operator implements the same three calls: `open()` to set up,
`next()` to produce, `close()` to tear down. **`next()` returns exactly
one tuple** — one row — which is what makes this a **tuple-at-a-time**
engine: the unit that crosses the operator boundary is a single row.

Control flows the other way from data. Execution is **pull-based**
(demand-driven): the root asks its child for a tuple, that child asks
*its* child, and the request travels down to the scan, which returns one
row back up the chain. The opposite arrangement, **push-based**, has the
source hand a batch downward into the operators that consume it — that
is what DuckDB does inside a pipeline (reading-duckdb-execution.md
Step 5). Postgres pulls, all the way down:

```
 Project.next()
   └─ calls Agg.next()
        └─ calls Filter.next()      per-tuple costs, PER TUPLE:
             └─ calls Scan.next()   - virtual call (indirect branch) x depth
                                    - interpretation of the expression tree
                                    - tuple is gone from registers between calls
```

The elegance is real: operators compose arbitrarily (any tree of
`next()`-speaking boxes works), execution is demand-driven (a LIMIT stops
pulling and everything upstream stops), and memory stays bounded (one
tuple in flight per operator, so a 100 GB sort-free query needs no 100 GB
buffer). The cost is the subject of this guide — and of this topic.

Why it matters: every later step is a price tag attached to one of those
three properties, and every alternative in this topic keeps the tree and
changes the unit.

### Step 2 — the price: an indirect call per node per tuple

> **In:** the operator tree from Step 1.
> **Out:** two numbers — the dispatch count a plan generates, and this
> repo's own measured cost per row — plus the surprise in the second one
> that Step 7 returns to.

In postgres, "call the child's `next()`" is `ExecProcNode(node)`, a
four-line inline function:

```c
// src/include/executor/executor.h — the whole of ExecProcNode, 314-329
   314  /* ----------------------------------------------------------------
   315   *		ExecProcNode
   316   *
   317   *		Execute the given node to return a(nother) tuple.
   318   * ----------------------------------------------------------------
   319   */
   320  #ifndef FRONTEND
   321  static inline TupleTableSlot *
   322  ExecProcNode(PlanState *node)
   323  {
   324  	if (node->chgParam != NULL) /* something changed? */
   325  		ExecReScan(node);		/* let ReScan handle this */
   326
   327  	return node->ExecProcNode(node);
   328  }
   329  #endif
```

The line that carries the argument is **327**, and note that it is not
the whole function: 324-325 test `chgParam` on every call, because a
correlated subplan's parameter may have changed since the last tuple.
`node->ExecProcNode` is a field, so 327 is an **indirect call** — a call
whose target address is loaded from data at runtime rather than encoded
in the instruction, which means the CPU must *predict* where it is going
and pays a pipeline refill when it predicts wrong. A **branch
misprediction** is exactly that refill: the core has already begun
executing down the wrong path and must throw that work away.

One such call per plan node per tuple. The formula, with its symbols
named:

```
 dispatches = N × D
 where  N = rows pulled through the pipeline
        D = plan depth (operators between the scan and the root)

 t_dispatch = dispatches × c / f
 where  c = cycles per call+return+callee prologue
        f = core clock in cycles per second
```

Worked, on the topic's own shape — 100M rows through a 5-node plan, and
`c = 20` cycles as a stated assumption (a predicted indirect call, the
callee prologue, and the reload of the tuple pointer the caller no longer
had in a register), `f = 4e9`:

```
 dispatches   = 100e6 × 5            = 500,000,000 indirect calls
 cycles       = 500e6 × 20           = 10,000,000,000 cycles
 t_dispatch   = 1.0e10 / 4.0e9       = 2.5 s of dispatch alone
 per row      = 2.5 / 100e6          = 25 ns/row before any work
```

Now the same arithmetic with the call amortised over a **vector** — a
batch of values of one column handed across the operator boundary
instead of a single row (Step 1's unit, replaced). At a vector of 1024:

```
 dispatches   = (100e6 / 1024) × 5   = 488,281 indirect calls
 cycles       = 488,281 × 20         = 9,765,620 cycles
 t_dispatch   = 9.77e6 / 4.0e9       = 0.0024 s
 per-row share of dispatch: 25 ns / 1024  = 0.024 ns
```

A thousandfold on that term. That is the entire arithmetic of this topic,
and the reason the rest of it is about what *remains* after the term goes
away.

Because this repo measures the same shape, check the assumption against
the measurement rather than trusting it. `topics/11-execution-models`'s
provided lane runs 50 M rows through a three-operator Volcano chain
(`experiments/src/volcano.rs`: `Scan`, `FilterOp`, `AggOp`, composed
through `Box<dyn Operator>` at `:118-120`), and
[notes.md](notes.md)'s baseline table records:

```
 selectivity   time      rows/s        ns per scanned row (t / 50e6)
        5%    0.386 s   129.4 M/s      7.72 ns
       50%    0.484 s   103.3 M/s      9.68 ns
       95%    0.669 s    74.7 M/s     13.38 ns
```

9.68 ns per row for a two-`dyn`-call chain says the 20-cycles-per-call
assumption is the right order of magnitude, not a fantasy.

And then the surprise, which is [FINDINGS.md](../../FINDINGS.md) row 11:
**the engine gets slower as more rows pass the filter.** Do the marginal
division the table invites:

```
 extra survivors from 5% to 95%:  (0.95 − 0.05) × 50e6 = 45,000,000 rows
 extra time:                       0.669 − 0.386        = 0.283 s
 marginal cost per surviving row:  0.283 / 45e6         = 6.29 ns
```

Every row that *survives* costs 6.29 ns more than a row that is rejected
— at the assumed 4 GHz, about 25 cycles. That is the second `dyn` call
(`AggOp` pulling through `FilterOp`, `volcano.rs:63-70`) plus the
aggregate's read-modify-write into `sums[k]` (`:96`), neither of which a
rejected row ever reaches: `FilterOp::next` loops internally on a
rejection and returns nothing. **Evaluating the predicate is the cheap
part; passing it is what costs.** Any account of this model that says
"a selective filter does more work" has it backwards.

Why it matters: the per-tuple tax is proportional to rows *delivered*
through the operator boundary, not rows examined — so it is worst on
exactly the analytic queries that deliver the most.

### Step 3 — self-modifying dispatch: the wrapper that swaps itself out

> **In:** the indirect call of Step 2, line 327 of `executor.h`.
> **Out:** what the function pointer at 327 actually points at, and the
> pattern worth stealing.

Postgres's node dispatch has a cute optimization. Every node is
*initialized* with its `ExecProcNode` pointer set to a wrapper, and the
node's real method stashed beside it:

```c
// src/backend/executor/execProcnode.c — ExecSetExecProcNode, 429-440
   429  void
   430  ExecSetExecProcNode(PlanState *node, ExecProcNodeMtd function)
   431  {
   432  	/*
   433  	 * Add a wrapper around the ExecProcNode callback that checks stack depth
   434  	 * during the first execution and maybe adds an instrumentation wrapper.
   435  	 * When the callback is changed after execution has already begun that
   436  	 * means we'll superfluously execute ExecProcNodeFirst, but that seems ok.
   437  	 */
   438  	node->ExecProcNodeReal = function;
   439  	node->ExecProcNode = ExecProcNodeFirst;
   440  }
```

Lines 438-439 are the swap: the real method moves to `ExecProcNodeReal`,
and the pointer the hot path reads gets the wrapper. `ExecInitNode`
(`:141`) routes every node through this on the way out —
`ExecSetExecProcNode(result, result->ExecProcNode)` at `:391`, which is
why "every node starts as `ExecProcNodeFirst`" is true of all node types
without any of them knowing.

The wrapper's whole job is to run once:

```c
// src/backend/executor/execProcnode.c — ExecProcNodeFirst, 447-470
   447  static TupleTableSlot *
   448  ExecProcNodeFirst(PlanState *node)
   449  {
   // ... 450-456: comment — the stack check is not cheap on x86, so do it once ...
   457  	check_stack_depth();
   458
   // ... 459-463: comment — swap in a wrapper only if one is still needed ...
   464  	if (node->instrument)
   465  		node->ExecProcNode = ExecProcNodeInstr;
   466  	else
   467  		node->ExecProcNode = node->ExecProcNodeReal;
   468
   469  	return node->ExecProcNode(node);
   470  }
```

Line **467** is the one to look at: the node's own function-pointer field
is overwritten with the real method, so `executor.h:327` never reaches
this wrapper again. Note that the swap is *conditional* — 464-465 install
`ExecProcNodeInstr` instead when `EXPLAIN ANALYZE` asked for
per-node timing, which is how instrumentation costs nothing when it is
off and is a permanent wrapper when it is on. Line 469 then makes the
call that the first tuple was waiting for.

Self-modifying dispatch: the first call does setup, then replaces itself.
You have seen the pattern as lazy statics and memoized FFI symbol
resolution (question 3 below).

Why it matters: it removes the one-time checks from the hot path without
adding a branch to it — the check is not skipped, the *pointer* changed.
It does nothing about the indirect call itself, which is Step 2's number
and stays.

### Step 4 — tuple slots: deforming, and how often it really happens

> **In:** the tuple that `ExecProcNode` returns at `executor.h:327` — a
> `TupleTableSlot *`, not a row of values.
> **Out:** the cost of getting a column out of it, and the correction to
> a claim this guide used to make.

Tuples travel between operators as a **`TupleTableSlot`** — a container
that can hold a heap tuple (the packed on-disk byte layout), a minimal
tuple, or a **virtual tuple** (no bytes at all, just arrays of `Datum`
values and null flags). To read column *k* out of a packed tuple you must
**deform** it: walk the tuple's bytes computing where each column starts,
because a variable-length or nullable column ahead of *k* means offset
*k* cannot be computed from the schema alone.

The slot caches that work in `tts_values[]`/`tts_isnull[]` and remembers
how far it got in `tts_nvalid`:

```c
// src/include/executor/tuptable.h — slot_getattr, 413-428
   413  /*
   414   * slot_getattr - fetch one attribute of the slot's contents.
   415   */
   416  static inline Datum
   417  slot_getattr(TupleTableSlot *slot, int attnum,
   418  			 bool *isnull)
   419  {
   420  	Assert(attnum > 0);
   421
   422  	if (attnum > slot->tts_nvalid)
   423  		slot_getsomeattrs(slot, attnum);
   424
   425  	*isnull = slot->tts_isnull[attnum - 1];
   426
   427  	return slot->tts_values[attnum - 1];
   428  }
```

Line **422** is the correction this guide owes you. An earlier version of
this chapter said postgres "pays this per attribute access, per tuple".
It does not: the `attnum > tts_nvalid` test means the deform happens only
when the requested column is past the high-water mark, and every access
below it is two array reads (425, 427). `slot_getsomeattrs` (`:375-381`)
guards the same way, and the deform routine says so itself:

```c
// src/backend/executor/execTuples.c — the contract of slot_deform_heap_tuple, 1004-1007
  1004   *		This is essentially an incremental version of heap_deform_tuple:
  1005   *		on each call we extract attributes up to the one needed, without
  1006   *		re-computing information about previously extracted attributes.
  1007   *		slot->tts_nvalid is the number of attributes already extracted.
```

The expression interpreter pushes it further: it hoists the deform into
one dedicated step per slot, emitted ahead of every `Var` reference, so
the interpreter deforms once and then reads flat arrays (Step 5's
`EEOP_INNER_VAR` says this in its own comment at `execExprInterp.c:693-698`).

So the honest claim is: **postgres deforms once per tuple per slot, up to
the highest column the plan touches.** That is still a per-tuple cost, and
it is still what the vectorized engines amortise — DuckDB deforms once
per column per 2048-row chunk — but it is a factor of "columns touched"
smaller than the version this guide used to assert.

Why it matters: the honest number is the one worth beating. Per tuple,
not per access, is what your `vectorized.rs` has to divide by its batch
size.

### Step 5 — expressions as flat steps, dispatched by computed goto

> **In:** one deformed tuple in a slot, from Step 4.
> **Out:** the answer to `f < t` for that tuple, produced by the second
> interpretation layer — the one postgres already optimised.

Expressions (`a.x + 1 > b.y`) are the second interpreter, and here
postgres fought back. Rather than walking the expression *tree* per tuple
(recursion mirroring the syntax, one call per node), postgres compiles
each expression once, at plan time, into a flat array of **steps** — an
**opcode** (a small integer naming an operation) plus its operands, like
"deform up to column 3", "fetch attribute 2", "call `int4gt`", "if false,
bail". Then it interprets that linear program once per tuple.

The shape, in Rust because the C is macro-heavy:

```rust
// ILLUSTRATION — not quoted from postgres. The real loop is
// src/backend/executor/execExprInterp.c:630-2289, whose dispatch macros
// are quoted below (104-131) and one of whose opcode blocks is quoted at
// 689-704. Expressions compile to FLAT STEPS, then interpret per tuple.
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

The real dispatch is two sets of macros chosen by whether the compiler
supports label-as-values:

```c
// src/backend/executor/execExprInterp.c — the two dispatch schemes, 104-131
   104  #if defined(EEO_USE_COMPUTED_GOTO)
   // ... 105-117: the jump-target lookup tables ...
   118
   119  #define EEO_SWITCH()
   120  #define EEO_CASE(name)		CASE_##name:
   121  #define EEO_DISPATCH()		goto *((void *) op->opcode)
   122  #define EEO_OPCODE(opcode)	((intptr_t) dispatch_table[opcode])
   123
   124  #else							/* !EEO_USE_COMPUTED_GOTO */
   125
   126  #define EEO_SWITCH()		starteval: switch ((ExprEvalOp) op->opcode)
   127  #define EEO_CASE(name)		case name:
   128  #define EEO_DISPATCH()		goto starteval
   129  #define EEO_OPCODE(opcode)	(opcode)
   130
   131  #endif							/* EEO_USE_COMPUTED_GOTO */
```

Line **121** against line **128** is the whole difference. **Switch
threading** (128) returns to one shared `switch` after every step, so the
program has exactly *one* indirect branch, and the predictor has one
history slot to describe every opcode transition in every query.
**Computed goto**, also called **direct threading** (121), ends each
opcode's block with a jump straight to the next opcode's block, so there
are as many indirect-branch *sites* as there are opcode implementations,
each with its own predictor entry that can learn "a `SCAN_VAR` here is
followed by a `FUNCEXPR_STRICT_2`". Postgres's own header comment makes
the claim (`:19-28`): a single dispatch location causes "more jumps and
bad branch prediction".

Concretely, the dispatch table in `ExecInterpExpr` has **121** entries
and the function contains **123** `EEO_CASE` blocks (counted at this
commit), spread over lines 470-2289 — a single 1820-line C function. So
switch threading gives that whole opcode set one predictor entry;
computed goto gives it 121. Setup happens once per expression, not per
tuple, at `:440-454`, where each step's `opcode` field is overwritten
with the address of its block.

The cost of getting this wrong is measurable, and this repo measured a
close cousin of it. [FINDINGS.md](../../FINDINGS.md) row 17: branchy
filtering collapses to **0.95 GB/s** at 50% selectivity while a
branchless kernel stays flat at **~10 GB/s**. On the 4-byte elements
that lane uses:

```
 branchy:      0.95e9 B/s / 4 B = 237.5e6 elem/s → 1 / 237.5e6 = 4.21 ns/elem
 branchless:  10.0e9  B/s / 4 B = 2.50e9  elem/s → 1 / 2.50e9  = 0.40 ns/elem
 gap:                                              3.81 ns/elem
 at 50% selectivity, ~1 mispredict per 2 elements → ≤ 7.62 ns per mispredict
 at the assumed 4 GHz                             → ≤ 30 cycles
```

Read that as an *upper* bound, not a measurement of the misprediction
penalty: the branchless lane also autovectorizes, so part of the 3.81 ns
is SIMD rather than prediction. It is still the right order for what one
unpredictable indirect branch per interpreter step costs, and it is why
121 predictor entries beat 1.

Why it matters: this is the one layer where postgres is not naive. The
step list *is* the flat program a vectorized engine runs — postgres just
runs it one tuple at a time.

### Step 6 — the fork: a prepared expression becomes one of two things

> **In:** the flat step list from Step 5, at the moment
> `ExecReadyInterpretedExpr` finishes building it.
> **Out:** two different callables — a hand-written fast path for simple
> shapes, or the full interpreter — and only the second one pays Step 5's
> dispatch at all.

Before installing the interpreter, postgres pattern-matches the step list
and, for a handful of shapes, swaps in a dedicated C function that skips
the interpreter entirely. This is a **peephole optimization**: a
transformation that looks at a short window of instructions and replaces
it with something better, without any understanding of the whole program.

```c
// src/backend/executor/execExprInterp.c — inside ExecReadyInterpretedExpr, 288-308
   288  	/*
   289  	 * Select fast-path evalfuncs for very simple expressions.  "Starting up"
   290  	 * the full interpreter is a measurable overhead for these, and these
   291  	 * patterns occur often enough to be worth optimizing.
   292  	 */
   293  	if (state->steps_len == 5)
   294  	{
   // ... 295-299: read out steps[0..3]'s opcodes ...
   300  		if (step0 == EEOP_INNER_FETCHSOME &&
   301  			step1 == EEOP_HASHDATUM_SET_INITVAL &&
   302  			step2 == EEOP_INNER_VAR &&
   303  			step3 == EEOP_HASHDATUM_NEXT32)
   304  		{
   305  			state->evalfunc_private = (void *) ExecJustHashInnerVarWithIV;
   306  			return;
   307  		}
   308  	}
```

The dispatch key is the *length* of the step list — 5 at 293, 4 at 309,
3 at 337, 2 at 399 — and then the opcode sequence. The simplest branch is
the easiest to read: a three-step program that fetches and returns one
inner column becomes `ExecJustInnerVar`:

```c
// src/backend/executor/execExprInterp.c — the 3-step patterns, 337-347
   337  	else if (state->steps_len == 3)
   338  	{
   339  		ExprEvalOp	step0 = state->steps[0].opcode;
   340  		ExprEvalOp	step1 = state->steps[1].opcode;
   341
   342  		if (step0 == EEOP_INNER_FETCHSOME &&
   343  			step1 == EEOP_INNER_VAR)
   344  		{
   345  			state->evalfunc_private = ExecJustInnerVar;
   346  			return;
   347  		}
```

Line **345** is the fork: `evalfunc_private` now names a hand-written
function, and this expression will never enter `ExecInterpExpr`. Every
shape that falls through all the tests lands on `:456`,
`state->evalfunc_private = ExecInterpExpr;`, and pays Step 5's dispatch
per tuple forever after. Twenty such fast paths are declared at
`:159-178`, and they are all *projections and hash steps* — fetching a
column, assigning a column, hashing a join key. Not one of them evaluates
a predicate.

Which tells you where the peephole's authors found the volume: emitting a
column into an output slot is the most common expression in any plan, and
"start the interpreter" was measurable against it.

Worked, on the query in question 1 — `SELECT sum(x) FROM t WHERE y > 10`.
The `WHERE` clause compiles to five steps, and the reason there are five
and not six is at `execExpr.c:2760-2770`, where a `Const` argument is
written straight into the function's `fcinfo` at *init* time ("Don't
evaluate const arguments every round; especially interesting for
constants in comparisons") and a two-argument strict function gets the
specialised opcode at `:2788-2789`:

```
 EEOP_SCAN_FETCHSOME     deform t up to column y      (interp block :662)
 EEOP_SCAN_VAR           y → the compare's arg 0      (interp block :719)
 EEOP_FUNCEXPR_STRICT_2  int4gt(arg0, 10)             (interp block :996)
 EEOP_QUAL               false → bail to the end      (interp block :1182)
 EEOP_DONE_RETURN        return the boolean           (interp block :632)
```

Five steps, five dispatches, per tuple — none matching a fast-path
pattern, because all 20 of them are projections and hash steps. Add the plan's own
`ExecProcNode` calls (Step 2) and a 3-node plan over 100M rows is
`100e6 × (3 + 5) = 800,000,000` indirect branches for a query whose
useful work is one integer compare and one add per row.

Why it matters: the fork is the whole argument for compilation in
miniature. Postgres pattern-matched 20 shapes by hand; a JIT (topic 19)
pattern-matches every shape by construction.

### Step 7 — the ladder, and why postgres gets away with it

> **In:** both dispatch costs — node-level from Step 2, step-level from
> Steps 5 and 6.
> **Out:** where postgres sits on the interpretation ladder, what it
> already compiles, and the workload boundary where the model stops being
> defensible.

Linearizing the expression is *half* of vectorization — postgres just
still applies it one tuple at a time:

```
 tree-walk interpreter      linear-step interpreter     vectorized kernel
 (recursive, per tuple)     (flat, per tuple)           (flat, per 2048)
        slowest        →        postgres          →        DuckDB
                                    ↘ JIT (topic 19) compiles the steps
```

Why it survives: for OLTP a point query touches three tuples, so Step 2's
25 ns/row and Step 6's five dispatches are noise beside a buffer-pool
lookup, and writes are dominated by WAL and locking anyway. For analytics
it does not get away with it, and that is the market gap DuckDB drove a
truck through.

Postgres does JIT, and the scope is narrower than "queries" and wider
than this guide used to say. `src/backend/jit/README:249-251`:
"Currently expression evaluation and tuple deforming are JITed" — so
Step 4's deform and Steps 5-6's step list both get compiled, and the
operator loop of Step 2 does not. The trigger is a cost threshold, not a
row count: `src/backend/jit/jit.c:40` sets `jit_above_cost = 100000`,
with `jit_expressions` and `jit_tuple_deforming` defaulting to true at
`:37` and `:39`. The README's own future list (`:262-263`) names "later
compiling larger parts of queries" as not-yet-done.

Why it matters: postgres has already conceded Steps 4-6 to compilation
and kept Step 2. Your `vectorized.rs` attacks the opposite half.

## Where each step lives in the code

Read in this order: the dispatch (small), the slot (small), then the
interpreter (large, and worth an hour on its own).

| File | Lines | What | Step |
|---|---|---|---|
| `src/include/executor/executor.h` | 314-329 | `ExecProcNode` — the indirect call is 327; `chgParam` recheck at 324 | 1, 2 |
| `src/backend/executor/execProcnode.c` | 141 | `ExecInitNode` — builds the `PlanState` tree | 1 |
| `src/backend/executor/execProcnode.c` | 391 | `ExecSetExecProcNode(result, result->ExecProcNode)` — every node gets the wrapper here | 3 |
| `src/backend/executor/execProcnode.c` | 429-440 | `ExecSetExecProcNode` — real method to `ExecProcNodeReal` (438), wrapper into the hot field (439) | 3 |
| `src/backend/executor/execProcnode.c` | 447-470 | `ExecProcNodeFirst` — `check_stack_depth()` once (457), pointer swap (464-467), then the deferred call (469) | 3 |
| `src/include/executor/tuptable.h` | 375-381 | `slot_getsomeattrs` — deform only past `tts_nvalid` | 4 |
| `src/include/executor/tuptable.h` | 413-428 | `slot_getattr` — the `attnum > tts_nvalid` guard at 422 | 4 |
| `src/backend/executor/execTuples.c` | 995-1108 | `slot_deform_heap_tuple` — the incremental contract is stated at 1004-1007; `tts_nvalid` advanced at 1106-1108 | 4 |
| `src/backend/executor/execExprInterp.c` | 6-46 | the file header — read it first; it argues switch vs direct threading (19-28) and names the fast paths (35-38) | 5, 6 |
| `src/backend/executor/execExprInterp.c` | 104-131 | `EEO_SWITCH`/`EEO_CASE`/`EEO_DISPATCH` — computed goto at 121, the switch fallback at 128 | 5 |
| `src/backend/executor/execExprInterp.c` | 252-457 | `ExecReadyInterpretedExpr` — the peephole (288-438), the direct-threading rewrite (440-454), the fallback at 456 | 6 |
| `src/backend/executor/execExprInterp.c` | 469-2289 | `ExecInterpExpr` — 1820 lines, 121 dispatch-table entries (484+), 123 opcode blocks; loop entry at 626-631 | 5 |
| `src/backend/executor/execExprInterp.c` | 689-704 | `EEOP_INNER_VAR` — reads `tts_values[attnum]` directly, and says why in 693-698 | 4, 5 |
| `src/backend/executor/execExpr.c` | 2754-2790 | const arguments folded into `fcinfo` at init (2760-2770); `EEOP_FUNCEXPR_STRICT_2` chosen at 2788-2789 | 6 |
| `src/backend/jit/jit.c` | 37-40 | `jit_expressions`, `jit_tuple_deforming`, `jit_above_cost = 100000` | 7 |
| `src/backend/jit/README` | 246-263 | what is JITed and what is not | 7 |

Suggested route: `executor.h:314` → `execProcnode.c:429` and `:447` →
`execExprInterp.c`'s header comment (`:6`) → the macros (`:104`) →
`ExecReadyInterpretedExpr` (`:252`) → then dip into `ExecInterpExpr`
(`:469`) at three or four opcode blocks only. Do not read all 1820 lines.

## Questions for notes.md

1. Count the indirect branches per tuple for
   `SELECT sum(x) FROM t WHERE y > 10`: plan nodes × 1 + expression
   steps. Then per 2048 tuples for the DuckDB equivalent. (Step 6 counts
   the `WHERE` clause's five steps for you and names the interpreter
   block each one lands in — the `sum(x)` transition steps are yours.)
2. Computed goto vs switch: WHY does one predictor entry per opcode site
   help? (Think topic 0's branch_misprediction bench, and the 121
   dispatch-table entries of `ExecInterpExpr`.)
3. `ExecProcNodeFirst`'s pointer swap is bit-smuggling's cousin —
   self-modifying dispatch. Where else have you seen
   "first call does setup, then replaces itself"? (Hint: lazy statics,
   memoized FFI resolution.)
4. M11: your eval.rs will interpret property predicates over batches.
   Linear steps or closure tree? All 20 of postgres's fast paths
   (`:159-178`) are projections and hash steps, not predicates — what
   does that suggest about the 3 Cypher shapes worth special-casing
   (`n.prop = lit`, `n.prop > lit`, label check)?

## Takeaway

Two interpretation layers, one already half-fixed. The node layer costs
an indirect call per operator per tuple and postgres has not touched it;
the expression layer costs an opcode per step per tuple and postgres has
flattened it, direct-threaded it, peepholed 21 shapes out of it, and JITs
it above `jit_above_cost`. Vectorization attacks the first layer, which
is the one still standing — and this repo's row 11 says the bill lands on
rows that *pass* the filter, so the tax is worst on exactly the queries
that return the most.

## Done when

Answer each before unfolding it.

- [ ] You can explain the two dispatch costs (node-level `ExecProcNode`, step-level opcode) and name the mitigation for each.

  <details><summary>Answer</summary>

  The node-level cost is one indirect call per plan node per tuple, at
  `src/include/executor/executor.h:327` — `return node->ExecProcNode(node);`,
  a call through a function-pointer field. A 5-node plan over 100M rows
  issues 500M of them; at a stated 20 cycles each on a 4 GHz core that is
  2.5 s, 25 ns/row, before any work. Its mitigation is *not* in postgres:
  it is the vector. Handing 1024 rows across the boundary instead of one
  turns 500M calls into 488,281 and the per-row share of dispatch from
  25 ns into 0.024 ns.

  The step-level cost is one interpreted opcode per expression step per
  tuple: `WHERE y > 10` is five steps (`execExpr.c:2760-2789` folds the
  constant in at init, so it is five and not six), each ending in
  `EEO_DISPATCH()`. Postgres has three mitigations here, all shipping:
  computed-goto dispatch (`execExprInterp.c:121`) giving each of 121
  opcode sites its own predictor entry instead of one shared indirect
  branch (`:128`); the 20 hand-written fast paths that skip the
  interpreter for simple shapes (`:159-178`, installed at `:288-438`);
  and LLVM JIT of expressions and tuple deforming above
  `jit_above_cost = 100000` (`jit/jit.c:40`, scope stated in
  `jit/README:249-251`).

  </details>

- [ ] You can say why postgres's Volcano executor gets *slower* as a filter passes more rows, and give the marginal cost per surviving row from this repo's own lane.

  <details><summary>Answer</summary>

  Because the per-tuple tax is paid by rows that cross an operator
  boundary, not by rows that are examined. In the provided lane's chain
  (`experiments/src/volcano.rs`), a rejected row costs one `dyn` call
  into `Scan::next` and one predicate compare; `FilterOp::next` (`:63-70`)
  loops internally and never returns. A surviving row costs that plus the
  return through the second `dyn` call and the aggregate's
  read-modify-write of `sums[k]` (`:96`).

  [notes.md](notes.md)'s baseline table, 50 M rows: 0.386 s at 5%
  selectivity, 0.484 s at 50%, 0.669 s at 95% — 129.4, 103.3 and
  74.7 M rows/s. The marginal division is
  `(0.669 − 0.386) / ((0.95 − 0.05) × 50e6) = 0.283 / 45e6 = 6.29 ns` per
  additional surviving row, about 25 cycles at an assumed 4 GHz. That is
  [FINDINGS.md](../../FINDINGS.md) row 11, and it inverts the intuition
  that a permissive filter is the cheap case.

  </details>

- [ ] You can state how often postgres deforms a tuple, and correct the "once per attribute access" version of the claim.

  <details><summary>Answer</summary>

  Once per tuple per slot, up to the highest column the plan references —
  not once per access. `slot_getattr` (`tuptable.h:416-428`) tests
  `attnum > slot->tts_nvalid` at 422 and only then calls
  `slot_getsomeattrs`; every access at or below the high-water mark is
  two array reads, 425 and 427. `slot_deform_heap_tuple` states the
  contract in its own comment (`execTuples.c:1004-1007`): "an incremental
  version of heap_deform_tuple ... without re-computing information about
  previously extracted attributes".

  The expression interpreter tightens it further, hoisting the deform
  into a single `EEOP_*_FETCHSOME` step emitted before any `Var`
  reference (`execExprInterp.c:644-651`), after which `EEOP_INNER_VAR`
  (`:689-704`) reads `innerslot->tts_values[attnum]` with an `Assert`
  and no branch — the comment at 693-698 says exactly why. The honest
  comparison to a vectorized engine is therefore "once per tuple against
  once per column per 2048-row chunk", not "once per access".

  </details>

- [ ] You can explain what direct threading buys over switch threading, in predictor entries, and name the line where postgres chooses.

  <details><summary>Answer</summary>

  `execExprInterp.c:128` is switch threading: `EEO_DISPATCH()` expands to
  `goto starteval`, returning to one shared `switch`, so the entire
  opcode set is dispatched from a single indirect branch with a single
  predictor history. `:121` is direct threading:
  `goto *((void *) op->opcode)`, a jump from the *end of each opcode's own
  block*, so there are as many indirect-branch sites as opcode
  implementations — 121 dispatch-table entries and 123 `EEO_CASE` blocks
  in `ExecInterpExpr` at this commit. Each site's predictor entry can
  learn its own successor distribution: a `SCAN_VAR` block that is always
  followed by `FUNCEXPR_STRICT_2` becomes predictable, where the shared
  branch sees every transition in every query mixed together.

  The choice is made at compile time — `#ifdef HAVE_COMPUTED_GOTO` at
  `:90-92` sets `EEO_USE_COMPUTED_GOTO`, and the rewrite that replaces
  each step's opcode with a label address happens once per expression at
  `:440-454`, not per tuple. The header comment argues it at `:19-28`.
  For the size of the effect, this repo's row 17 bounds one unpredictable
  branch at ≤7.62 ns (≤30 cycles at 4 GHz) from the 0.95 GB/s versus
  ~10 GB/s branchy/branchless gap on 4-byte elements — an upper bound,
  since the branchless lane also autovectorizes.

  </details>

- [ ] You can say what postgres already JITs and what it does not, without saying "expressions".

  <details><summary>Answer</summary>

  It JITs expression evaluation *and* tuple deforming —
  `src/backend/jit/README:249-251` names both, and `jit/jit.c:37-39`
  gives each its own GUC (`jit_expressions`, `jit_tuple_deforming`, both
  true by default). Deforming is the interesting half: the README's
  argument (`:255-257`) is that a JIT knows the number of columns and
  their types, so it can emit a straight-line deform with the branches
  removed — which is Step 4's cost, compiled away.

  What it does not JIT is the operator loop: `ExecProcNode`'s indirect
  call at `executor.h:327` survives compilation, so the per-node,
  per-tuple dispatch of Step 2 is unchanged no matter how expensive the
  query. The README lists "later compiling larger parts of queries" among
  future avenues (`:262-263`). The trigger is a plan-cost threshold, not
  a row count — `jit_above_cost = 100000` at `jit/jit.c:40` — which means
  a cheap plan over a lot of rows can miss it entirely.

  </details>

## References

**Code**
- [postgres](https://github.com/postgres/postgres) — pinned at `701f021`
  (`configure.ac:20` says `20devel`). Read
  `src/backend/executor/execProcnode.c` (the dispatch, ~970 lines) and
  `src/backend/executor/execExprInterp.c` (the interpreter — read the
  `:6-46` header comment first, then the macros and
  `ExecReadyInterpretedExpr`; do not read all 5990 lines), plus
  `src/include/executor/executor.h` and
  `src/include/executor/tuptable.h`; ~1 h.

| File | Lines | What |
|---|---|---|
| `src/include/executor/executor.h` | 327 | the indirect call, once per node per tuple |
| `src/backend/executor/execProcnode.c` | 438-439 | real method aside, wrapper installed |
| `src/backend/executor/execProcnode.c` | 464-467 | the wrapper replacing itself |
| `src/include/executor/tuptable.h` | 422 | the `tts_nvalid` guard that makes deforming incremental |
| `src/backend/executor/execTuples.c` | 1004-1007 | the incremental-deform contract, in postgres's words |
| `src/backend/executor/execExprInterp.c` | 121 / 128 | computed goto against switch |
| `src/backend/executor/execExprInterp.c` | 288-438 | the 20-shape peephole fork |
| `src/backend/executor/execExprInterp.c` | 456 | the fallback: everything else pays the interpreter |
| `src/backend/executor/execExpr.c` | 2760-2770 | constants folded into `fcinfo` at init |
| `src/backend/jit/jit.c` | 40 | `jit_above_cost = 100000` |

**Background**
- Graefe, *Volcano — An Extensible and Parallel Query Evaluation System*
  (TKDE 1994) — the model this executor implements.
- This repo: [FINDINGS.md](../../FINDINGS.md) row 11 (the Volcano ceiling
  and its selectivity curve) and row 17 (the branchy/branchless collapse
  used to bound a misprediction in Step 5).
