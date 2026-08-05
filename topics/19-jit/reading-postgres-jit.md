# Postgres's LLVM JIT: why everyone sets jit=off

The production cautionary tale. Postgres 11+ ships an LLVM JIT for
*expressions and tuple deforming only* — the executor loop stays
interpreted — and it is famous mostly for the advice "set jit=off".
This chapter builds the machinery step by step — what Postgres
interprets, what the JIT actually compiles, the underrated deforming
half, and the cost-model gate whose four failure modes are the
lesson — then maps every step to the ~3 files under
`src/backend/jit/llvm/`.

**Version.** Anchors are against postgres at the pin in
`resources/codebases.md`, **`701f021`** — which is **PostgreSQL
20devel**, not a released branch. That matters for one headline
number: see Step 4. Retrieve any anchor with
`python3 tools/pinned-source.py show postgres src/backend/jit/jit.c -r 32:42`.
Note also that GUC definitions have moved out of `guc_tables.c` into
the generated `src/backend/utils/misc/guc_parameters.dat`, so older
walkthroughs that send you to `guc_tables.c` for the JIT defaults
now find nothing.

## The problem in one sentence

Postgres decides whether to spend tens of milliseconds of LLVM
compilation using a *planner cost estimate* made before a single row
is read — so when the estimate says "expensive" and the query takes
5 ms, you pay the compile for nothing, and enough users hit that to
make "try jit=off" standard ops advice.

## The concepts, step by step

### Step 1 — ExprState: Postgres already has bytecode

> **In:** a parsed WHERE clause or projection.
> **Out:** an `ExprState` — a flat array of `ExprEvalStep`s — plus
> the threaded interpreter that runs it. This is the *baseline* the
> JIT must beat, and every later step is defined against it.

Before any JIT enters the picture, Postgres does not tree-walk
expressions per row. At plan time a WHERE clause or projection is
flattened into an **ExprState**: a contiguous array of small *steps*
(opcodes like `EEOP_FUNCEXPR` "call this function", `EEOP_QUAL`
"test and jump out if false"), executed by a threaded-dispatch
interpreter. This is the same design as SQLite's VDBE at expression
grain: flatten once, dispatch per step per row.

But it is a *better* interpreter than SQLite's, in exactly the way
`reading-sqlite-vdbe.md` says SQLite is not:

```c
// postgres/src/backend/executor/execExprInterp.c — threaded dispatch, 113-137
 113	/* to make dispatch_table accessible outside ExecInterpExpr() */
 114	static const void **dispatch_table = NULL;
// ... 115-118: reverse_dispatch_table, for mapping a label back to an opcode ...
 119	#define EEO_SWITCH()
 120	#define EEO_CASE(name)		CASE_##name:
 121	#define EEO_DISPATCH()		goto *((void *) op->opcode)
 122	#define EEO_OPCODE(opcode)	((intptr_t) dispatch_table[opcode])
 123
 124	#else							/* !EEO_USE_COMPUTED_GOTO */
 125
 126	#define EEO_SWITCH()		starteval: switch ((ExprEvalOp) op->opcode)
 127	#define EEO_CASE(name)		case name:
 128	#define EEO_DISPATCH()		goto starteval
 129	#define EEO_OPCODE(opcode)	(opcode)
 130
 131	#endif							/* EEO_USE_COMPUTED_GOTO */
 132
 133	#define EEO_NEXT() \
 134		do { \
 135			op++; \
 136			EEO_DISPATCH(); \
 137		} while (0)
```

Line 121 is the load-bearing one. **Threaded dispatch** (also called
computed goto): instead of one shared indirect branch at the top of
a loop, *every* step ends with its own `goto *op->opcode` — so
`op->opcode` is not an enum at all, it is a pre-resolved *label
address*, taken from `dispatch_table` at line 114. The branch
predictor then gets one indirect-branch site per opcode kind, each
with its own history, instead of one 121-target site that it cannot
possibly predict. Lines 126–129 are the portable fallback when the
compiler has no `&&label` extension.

So the JIT's opponent is a genuinely good bytecode interpreter, not
a strawman. The win on offer is only the per-step dispatch plus what
a compiler can see *across* steps.

How big is the step set? Count it two ways and get the same answer,
which is the tidiest structural fact in this chapter:

```
 grep -c 'EEO_CASE('  execExprInterp.c   → 123
   minus the 2 #define lines at :120, :127 → 121 real step kinds

 the dispatch_table initialiser (execExprInterp.c:484-612)
   → 121 `&&CASE_…` entries, and the file asserts it:
     execExprInterp.c:608  StaticAssertDecl(lengthof(dispatch_table)
                                            == EEOP_LAST + 1, …)

 grep -c 'case EEOP_'  llvmjit_expr.c    → 121
```

**121 interpreter step kinds, 121 JIT cases.** The JIT is a
one-to-one mirror of the interpreter, maintained by hand. That is
the maintenance bill for this feature, stated as a number.

### Step 2 — what the JIT compiles: one basic block per step

> **In:** Step 1's `ExprState` step array. **Out:** one LLVM
> function containing one basic block per step, with the dispatch
> replaced by fallthrough. Nothing above the expression is touched.

The JIT's scope is deliberately narrow, and this is the single most
misreported fact about it:

```
 NOT compiled: executor nodes (SeqScan, HashJoin...) — still the
               interpreted node->ExecProcNode indirection
 compiled:     ExprState step arrays (WHERE clauses, projections,
               aggregates' transition expressions)
               + tuple DEFORMING (attribute extraction — schema-
               specialized: known offsets, nullability)
```

Postgres compiles **expressions and tuple deforming**, not query
plans. `planner.c:717-720` says it in the source: the only two
things the flag word can request are `PGJIT_EXPR` and
`PGJIT_DEFORM`. If someone tells you Postgres "compiles queries the
way HyPer does", they have not read `llvmjit_expr.c`.

`llvm_compile_expr` (`llvmjit_expr.c:80`) translates each step of
one ExprState into one **basic block** (a straight-line chunk of
code with one entry and one exit — LLVM's unit of control flow),
wires the blocks together in step order, and lets LLVM fold the
dispatch away:

```c
// postgres/src/backend/jit/llvm/llvmjit_expr.c — the block-per-step loop, 301-324
 301		/* allocate blocks for each op upfront, so we can do jumps easily */
 302		opblocks = palloc_array(LLVMBasicBlockRef, state->steps_len);
 303		for (int opno = 0; opno < state->steps_len; opno++)
 304			opblocks[opno] = l_bb_append_v(eval_fn, "b.op.%d.start", opno);
 305
 306		/* jump from entry to first block */
 307		LLVMBuildBr(b, opblocks[0]);
 308
 309		for (int opno = 0; opno < state->steps_len; opno++)
 310		{
// ... 311-315: local LLVMValueRef declarations ...
 316			LLVMPositionBuilderAtEnd(b, opblocks[opno]);
 317
 318			op = &state->steps[opno];
 319			opcode = ExecEvalStepOp(state, op);
 320
 321			v_resvaluep = l_ptr_const(op->resvalue, l_ptr(TypeDatum));
 322			v_resnullp = l_ptr_const(op->resnull, l_ptr(TypeStorageBool));
 323
 324			switch (opcode)
```

Line 302 is why the whole thing works: **every block is created
before any is filled in**, so a step that jumps forward (`EEOP_QUAL`
skipping to the end on a false qual) already has its target
available and needs no patch-up pass. Same instinct as SQLite's
fixed-width `p2` jump operand (`reading-sqlite-vdbe.md`, Step 3),
solved with an array of block handles instead.

Lines 321–322 are the quiet performance story. `op->resvalue` is a
pointer that is *known at compile time*, so it is emitted as an LLVM
constant — the interpreter loads it from the step struct every row;
the JIT bakes it in. Multiply that by 121 opcode kinds and you have
most of the win that is not dispatch.

**Anchor correction.** The giant `switch (opcode)` is at
**`llvmjit_expr.c:324`** (first case `EEOP_DONE_RETURN` at `:326`),
not "326+"; and the FETCHSOME group is at **`:344-348`**, not
"354+".

```rust
// ILLUSTRATION — not quoted from postgres. The C at
// llvmjit_expr.c:301-324 (above) written as Rust, to make the shape
// obvious; read the real thing, this elides the LLVM builder plumbing.
let opblocks: Vec<Block> = state.steps.iter().map(|_| new_block()).collect();
for (i, step) in state.steps.iter().enumerate() {
    position_at(opblocks[i]);
    match step.opcode {
        EEOP_QUAL           => emit_cmp_and_branch(step, opblocks[step.jumpdone]),
        EEOP_FUNCEXPR       => emit_direct_call(step.fn_addr, step.args),
        EEOP_SCAN_FETCHSOME => emit_deform(tupledesc, step.last_attr),
        // ... the giant switch mirrors execExprInterp.c case by case
    }
    emit_branch(opblocks[i + 1]);   // then LLVM folds blocks together
}
```

Structurally the SAME translation our stub does for `Expr` → CLIF
(`experiments/src/jit.rs:11-19`) — postgres just starts from
bytecode instead of an AST. It is NOT Neumann's whole-pipeline
compilation: operators still call each other through interpreted
indirection; only the leaves got fast.

### Step 3 — tuple deforming: the underrated half

> **In:** Step 2's `EEOP_*_FETCHSOME` steps and the `TupleDesc`
> that describes the row layout. **Out:** a decoder specialized to
> one schema, and the reason it is often worth more than the
> expression JIT.

**Deforming** is extracting attribute values from Postgres's
on-disk row format — variable-length fields, a null bitmap, and
alignment padding mean that reaching column 19 requires walking
columns 1–18, testing the null bitmap at each. The generic decoder
(`slot_deform_heap_tuple`) re-discovers the schema per row.

The hook is right where you would want it:

```c
// postgres/src/backend/jit/llvm/llvmjit_expr.c — the deform hook, 404-410
 404					if (tts_ops && desc && (context->base.flags & PGJIT_DEFORM))
 405					{
 406						INSTR_TIME_SET_CURRENT(deform_starttime);
 407						l_jit_deform =
 408							slot_compile_deform(context, desc,
 409												tts_ops,
 410												op->d.fetch.last_var);
```

Three things to notice at :404. First, `PGJIT_DEFORM` is a
*separate* flag from `PGJIT_EXPR` — you can have one without the
other, and `jit_tuple_deforming` (`jit.c:39`, default `true`) is the
GUC. Second, `desc` — the `TupleDesc` — is the specialization key:
`slot_compile_deform` in `llvmjit_deform.c` constant-folds each
attribute's offset, skips null-bitmap tests for `NOT NULL` columns,
and knows every alignment in advance. Third, `op->d.fetch.last_var`
bounds the work: the generated decoder stops at the highest column
the expression actually references. If `tts_ops` or `desc` is
missing, `:434-437` falls back to emitting a plain call to
`slot_getsomeattrs_int` — the generic path.

This routinely beats the expression JIT in profit because deforming
is per-ROW-per-ATTRIBUTE and pure branchy pointer math — the same
reason topic 12's PAX/columnar layouts win, arrived at from the
compiler side. And note the framing: a columnar layout makes the
whole problem *disappear* rather than compiling a faster solution to
it. Question 4.

### Step 4 — the gate: a cost estimate decides, and misfires four ways

> **In:** the finished plan's `total_cost` — a unitless planner
> estimate (topic 10) — and five GUCs. **Out:** a `jitFlags` word
> that is fixed for the whole query before any row is read. This is
> the step the chapter exists for.

**The headline correction: at this pin, `jit` defaults to OFF.**

```c
// postgres/src/backend/jit/jit.c — every JIT GUC's C default, 32-42
  32	/* GUCs */
  33	bool		jit_enabled = false;
// ... 34-36: jit_provider, jit_debugging_support, jit_dump_bitcode ...
  37	bool		jit_expressions = true;
// ...   38: jit_profiling_support ...
  39	bool		jit_tuple_deforming = true;
  40	double		jit_above_cost = 100000;
  41	double		jit_inline_above_cost = 500000;
  42	double		jit_optimize_above_cost = 500000;
```

Line 33 is the news. Upstream Postgres has flipped the default: the
same value appears in the generated GUC table
(`src/backend/utils/misc/guc_parameters.dat:1451-1456`,
`boot_val => 'false'`), in the shipped config sample
(`src/backend/utils/misc/postgresql.conf.sample:492` — `#jit = off`)
and in the docs (`doc/src/sgml/config.sgml:6836` — "The default is
`off`."). The community's answer to this chapter's title was, in
the end, to take the advice. **Check this against whatever version
you actually run** — releases through PG 17 shipped `jit = on`.

Now the gate itself. It is **three** cost thresholds and two
booleans, not one threshold:

```c
// postgres/src/backend/optimizer/plan/planner.c — the whole gate, 698-721
 698		result->jitFlags = PGJIT_NONE;
 699		if (jit_enabled && jit_above_cost >= 0 &&
 700			top_plan->total_cost > jit_above_cost)
 701		{
 702			result->jitFlags |= PGJIT_PERFORM;
// ... 703-706: comment — "how much effort should be put into better code" ...
 707			if (jit_optimize_above_cost >= 0 &&
 708				top_plan->total_cost > jit_optimize_above_cost)
 709				result->jitFlags |= PGJIT_OPT3;
 710			if (jit_inline_above_cost >= 0 &&
 711				top_plan->total_cost > jit_inline_above_cost)
 712				result->jitFlags |= PGJIT_INLINE;
// ... 713-716: comment — "which operations should be JITed" ...
 717			if (jit_expressions)
 718				result->jitFlags |= PGJIT_EXPR;
 719			if (jit_tuple_deforming)
 720				result->jitFlags |= PGJIT_DEFORM;
 721		}
```

Read `>= 0` on lines 699, 707, 710: **a negative value disables that
tier**, which is the documented escape hatch (`config.sgml:6483` —
"Setting this to `-1` disables JIT compilation"). The whole decision
is five comparisons against one number, `top_plan->total_cost`, and
it happens in `standard_planner`, before the executor starts.

```
 the four thresholds, in cost units, from jit.c:40-42

   total_cost >  100000  → PGJIT_PERFORM   compile at all
   total_cost >  500000  → PGJIT_OPT3      run LLVM -O3
   total_cost >  500000  → PGJIT_INLINE    inline pg internals
   (jit_expressions / jit_tuple_deforming are booleans, not costs)

 failure 1: estimate high, reality short → pay the compile for a
            fast query   (the classic complaint)
 failure 2: cost is in COST UNITS not ms — 100000 has no unit
            relationship with compile time on this machine, and
            the cost model was calibrated for I/O, not for LLVM
 failure 3: decision is per-QUERY, all-or-nothing, made BEFORE
            any row is seen — no adaptivity (contrast Umbra)
 failure 4: opt3 and inlining are gated by ANOTHER estimate at the
            SAME default (500000) — so in practice you cross into
            the two most expensive LLVM modes simultaneously
```

Failure 4 is worth the arithmetic. A plan whose cost lands anywhere
above 500000 gets `PGJIT_OPT3 | PGJIT_INLINE` *together* — the two
settings that dominate compile time — from a single estimate that
was never designed to predict compile time. And the cost model's own
units come from `seq_page_cost = 1.0`: 500000 cost units ≈ half a
million sequential page reads ≈ 4 GB of I/O at 8 KB pages. That is
the quantity Postgres uses to decide how hard to run a compiler.

There is a partial mitigation — two LLJIT tiers:

```c
// postgres/src/backend/jit/llvm/llvmjit.c — the two engines, 100-101
 100	static LLVMOrcLLJITRef llvm_opt0_orc;
 101	static LLVMOrcLLJITRef llvm_opt3_orc;
```

and the selection, one flag test:

```c
// postgres/src/backend/jit/llvm/llvmjit.c — tier selection in llvm_compile_module, 716-721
 716		LLVMOrcLLJITRef compile_orc;
 717
 718		if (context->base.flags & PGJIT_OPT3)
 719			compile_orc = llvm_opt3_orc;
 720		else
 721			compile_orc = llvm_opt0_orc;
```

(**LLJIT** is LLVM's ORC-based JIT engine; opt0 compiles fast and
produces slow code, opt3 the reverse.) But tier choice is still
estimate-driven — line 718 reads a flag that `planner.c:709` set
before execution began. This is the actual lesson of the chapter:
the compile-or-not decision is a bet, and Postgres places it with
the least reliable number in the system, once, before it can
possibly learn anything. `reading-umbra-tidy-tuples.md` is what
placing it *after* you have evidence looks like.

### Step 5 — lifecycle plumbing worth stealing

> **In:** a compiled LLVM module and a query that will eventually
> end. **Out:** a rule for who owns executable memory, and the
> teardown call that makes dangling function pointers impossible.

JIT-compiled code is memory that something must own.
`llvm_compile_module` (`llvmjit.c:710`) adds each module to an LLJIT
dylib and takes a **resource tracker** for it
(`LLVMOrcJITDylibCreateResourceTracker`, `:781`), which is stored on
the context (`LLVMOrcResourceTrackerRef resource_tracker;`
`llvmjit.c:51`). Release is two calls:

```c
// postgres/src/backend/jit/llvm/llvmjit.c — llvm_release_context teardown, 288-289
 288				LLVMOrcResourceTrackerRemove(jit_handle->resource_tracker);
 289				LLVMOrcReleaseResourceTracker(jit_handle->resource_tracker);
```

`:288` unmaps the code; `:289` drops the handle. The surrounding
block (`:290-299`) then clears dead symbol-pool entries, because ORC
would otherwise leak the mangled names. Ownership is
per-query-context: `llvm_release_context` (`:253`) is registered as
the provider's `release_context` callback at `llvmjit.c:155`, right
above `cb->compile_expr = llvm_compile_expr;` at `:156`. When the
query dies, the code dies — no dangling function pointers.

M19 note: cranelift's `JITModule` has the same obligation, and our
stub already encodes it —
`experiments/src/jit.rs:26-31` keeps the module alive *inside*
`CompiledExpr` so the `fn(*const f64) -> f64` at `:30` cannot
outlive its code. `experiments/src/jit.rs:21` cites this very
Postgres line for the pattern.

### Step 6 — what transfers to M19, and the break-even arithmetic

> **In:** Steps 2–5's mechanism and Step 4's failure list.
> **Out:** the gate you should build instead, expressed as a
> division you can actually evaluate at runtime.

- Compile the *expression*, keep the executor: exactly M19's scope.
- Gate on MEASURED cost, not an estimate.
- Deforming lesson: FalkorDB's property access (attribute fetch
  from the property store) is the deform-analogue — likely more
  profit than arithmetic JIT.

The gate Postgres cannot write, written out with this topic's own
measured numbers (`notes.md`, Apple M3 Pro, depth 8 = 511 nodes):

```
 Postgres's gate:
   compile  iff  planner_estimate > 100000        [cost units]
   — one number, two unknowns (is the estimate right? what does
     LLVM cost on this box?), evaluated before any evidence exists.

 A measured gate, same decision:
   rows_breakeven = compile_µs / (µs_per_row_interp − µs_per_row_jit)

   interp lane, 511 nodes:  0.95 M rows/s → 1.053  µs/row
   vector lane, 511 nodes: 11.8  M rows/s → 0.0847 µs/row
   assume the JIT lands at the vector lane's rate (this topic's own
   prediction — see notes.md):
     saving      = 1.053 − 0.0847      = 0.968 µs/row
     with a 500 µs cranelift compile:
     rows_breakeven = 500 / 0.968      = 516 rows

 Both inputs are things you can MEASURE, not estimate:
   compile_µs        — time your own compile() and keep a moving
                       average per node count
   µs_per_row_interp — you are already running the interpreter;
                       count rows and nanoseconds as you go
 So the rule becomes: interpret first, and switch to compiled code
 once rows_seen exceeds break-even and the query is still running.
 That is exactly Umbra's adaptive execution, and it needs no
 planner estimate at all.
```

Note what the arithmetic reveals about Postgres's specific pain:
because the gate fires *before* row one, a plan estimated at 600000
cost units that returns 3 rows pays `PGJIT_OPT3 | PGJIT_INLINE`
compile time — the most expensive mode — against a saving of
3 × 0.968 µs ≈ 3 µs. There is no compile fast enough to win that
bet. The bug is not the threshold's value; it is that a threshold
on an estimate can never be right.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| `execExprInterp.c:113-137` | threaded dispatch: `dispatch_table`, `EEO_DISPATCH()` | 1 |
| `execExprInterp.c:484-612` | the 121-entry `&&CASE_…` table + its `StaticAssertDecl` at `:608` | 1 |
| `execExpr.h:296` | `EEOP_LAST` — the step-kind count the table asserts against | 1 |
| `llvmjit.c:155-156` | provider hooks: `release_context`, `cb->compile_expr = llvm_compile_expr` | 2, 5 |
| `llvmjit_expr.c:80` | `llvm_compile_expr(ExprState *state)` — the entry point | 2 |
| `llvmjit_expr.c:301-307` | one LLVM basic block per step, all allocated upfront | 2 |
| `llvmjit_expr.c:324` | the giant `switch (opcode)` — 121 `case EEOP_`, mirror of the interpreter | 1–2 |
| `llvmjit_expr.c:344-348` | the five `EEOP_*_FETCHSOME` cases | 3 |
| `llvmjit_expr.c:404-410` | `slot_compile_deform` — the deform hook, gated on `PGJIT_DEFORM` | 3 |
| `llvmjit_expr.c:434-437` | the generic fallback: emit a call to `slot_getsomeattrs_int` | 3 |
| `llvmjit_expr.c:972-1007` | `EEOP_QUAL` — the clearest single opcode to diff against the interpreter | 1–2 |
| `jit.c:33` | **`bool jit_enabled = false;`** — the default flipped upstream | 4 |
| `jit.c:40-42` | `jit_above_cost` 100000, `jit_inline_above_cost` 500000, `jit_optimize_above_cost` 500000 | 4 |
| `guc_parameters.dat:1451-1456` | the generated GUC entry (`boot_val => 'false'`) | 4 |
| `postgresql.conf.sample:492` | `#jit = off` | 4 |
| `config.sgml:6836` / `:6483-6499` | docs: "The default is `off`"; `-1` disables a tier | 4 |
| `planner.c:698-721` | the whole gate — 3 cost tests, 2 booleans, 5 flags | 4 |
| `llvmjit.c:100-101` | two LLJITs: `llvm_opt0_orc` / `llvm_opt3_orc` | 4 |
| `llvmjit.c:716-721` | tier selection from `PGJIT_OPT3` | 4 |
| `llvmjit.c:363` | `llvm_get_function` — lookup, calling `llvm_compile_module` at `:375` | 5 |
| `llvmjit.c:710-781` | module → ThreadSafeModule (`:778`) → dylib + resource tracker (`:781`) | 5 |
| `llvmjit.c:288-289` | teardown: `ResourceTrackerRemove` + `ReleaseResourceTracker` | 5 |

Paths are relative to `src/backend/` except `execExpr.h`
(`src/include/executor/`) and the docs. Fetch without a clone:
`python3 tools/pinned-source.py show postgres src/backend/jit/llvm/llvmjit_expr.c -r 301:330`.

Pair `llvmjit_expr.c` with `src/backend/executor/execExprInterp.c`
side by side — every `case EEOP_*` in the JIT mirrors an
`EEO_CASE()` in the interpreter, 121 for 121, and seeing what each
block replaces is Step 1 and Step 2 in one diff. Then read
`planner.c:698` for the gate and `llvmjit.c` for the lifecycle.

## Questions for notes.md

1. Trace one EEOP through both executors: find `EEOP_QUAL` in
   `execExprInterp.c` and at `llvmjit_expr.c:972-1007`. What does
   LLVM get to do that the interpreter can't (cross-step constant
   prop, dead null-check elimination)? Start from
   `llvmjit_expr.c:321-322` — what exactly became a constant there,
   and what does the interpreter do instead?
2. Why does the JIT emit ONE function per ExprState with a block
   per step, rather than one function per step (call overhead +
   register state across steps)? Then read the copy-and-patch
   contrast in `reading-umbra-tidy-tuples.md`: that system emits
   one *stencil* per node and gets away with it. What does it do
   differently at the call boundary?
3. `jit_above_cost` is in planner cost units (`jit.c:40`). Propose
   the fix upstream keeps debating: what would a *time-based* gate
   need to know (a compile-time model per step count, plus a rows
   estimate) — and which half is still an estimate? Use Step 6's
   division and identify which of its two inputs Postgres could
   measure today without any new infrastructure.
4. Deform JIT: for a 20-column table where the query touches
   column 19, what does the generated decoder skip vs the generic
   `slot_deform_heap_tuple`, and which topic 12 layout makes the
   whole problem vanish? `llvmjit_expr.c:410` passes
   `op->d.fetch.last_var` — what does that bound, and what does it
   *not* let you skip?
5. For M19: postgres compiles per-query with no cache (the resource
   tracker at `llvmjit.c:288-289` destroys the code when the query
   ends). GraphBLAS caches per type-combo forever
   (`reading-graphblas-jit.md`). Which is right for Cypher
   expressions, and what's the cache key (expression shape with
   constants as parameters — count how many distinct shapes a
   workload of 1000 queries has)?

## Done when

Answer each before unfolding it.

- [ ] You can explain that Postgres already had bytecode (`ExprState`) before it had a JIT, and what the JIT therefore actually replaces.

  <details><summary>Answer</summary>

  `ExprState` is a flat array of `ExprEvalStep`s produced at plan
  time, run by a **threaded-dispatch** interpreter:
  `execExprInterp.c:121` defines `EEO_DISPATCH()` as
  `goto *((void *) op->opcode)`, where `op->opcode` has been
  rewritten to a label address from `dispatch_table`
  (`:114`). Every step ends with its own indirect branch, so the
  predictor gets per-opcode history — strictly better than SQLite's
  single shared `switch`. The JIT therefore replaces only (a) the
  remaining per-step indirect branch, and (b) the per-row loads of
  values that are constant for the whole query — `op->resvalue` and
  `op->resnull` become LLVM constants at `llvmjit_expr.c:321-322`.
  It does not replace any executor node. There are **121** step
  kinds and **121** `case EEOP_` in the JIT: a hand-maintained
  one-to-one mirror.

  </details>

- [ ] You can explain tuple deforming and why it is the underrated half of the win.

  <details><summary>Answer</summary>

  Deforming turns Postgres's on-disk row (null bitmap, varlena
  fields, alignment padding) into `Datum`s. Because field offsets
  are not fixed, reaching attribute *n* means walking attributes
  1..n−1 and testing the null bitmap at each — per row. The generic
  `slot_deform_heap_tuple` rediscovers the schema every row;
  `slot_compile_deform` (called at `llvmjit_expr.c:407-410`, gated
  on `PGJIT_DEFORM` at `:404`) generates a decoder specialized to
  one `TupleDesc`: offsets constant-folded, null tests omitted for
  `NOT NULL` columns, alignment known, and work bounded by
  `op->d.fetch.last_var`. It is underrated because its cost is
  per-row *per-attribute* and it is pure branchy pointer math —
  the exact shape a compiler is good at — whereas expression JIT
  usually removes only a handful of dispatches per row.

  </details>

- [ ] You can name all four ways the `jit_above_cost` gate misfires, propose a better gate, and state the current default of `jit` itself.

  <details><summary>Answer</summary>

  (1) The estimate can be high while the query is short, so you pay
  compile time for nothing. (2) The threshold is in planner cost
  units — 100000 (`jit.c:40`) — which have no unit relationship to
  milliseconds of LLVM on your hardware. (3) The decision is
  per-query, all-or-nothing, made in `standard_planner`
  (`planner.c:698-721`) before any row is read, so it can never
  adapt. (4) `jit_optimize_above_cost` and `jit_inline_above_cost`
  both default to 500000 (`jit.c:41-42`), so crossing one line
  turns on LLVM `-O3` *and* inlining together — the two most
  expensive modes, chosen by the same unreliable number. Better
  gate: interpret first, measure `µs_per_row` and `compile_µs`
  directly, and switch when `rows_seen > compile_µs / (µs_interp −
  µs_jit)`. And the current default: **`jit` is `off`** at pin
  `701f021` (`jit.c:33`, `guc_parameters.dat:1451-1456`,
  `postgresql.conf.sample:492`, `config.sgml:6836`) — upstream took
  the advice in this chapter's title. Releases through PG 17 shipped
  it `on`.

  </details>

- [ ] You can say why the JIT emits one function per `ExprState` with a block per step.

  <details><summary>Answer</summary>

  Because basic blocks are free and function calls are not. All
  blocks are allocated upfront at `llvmjit_expr.c:302-304`, so
  forward jumps resolve without a patch-up pass; then LLVM's own
  simplify-CFG pass merges adjacent blocks that have a single
  predecessor, which is most of them — the interpreter's per-step
  dispatch literally becomes fallthrough. If each step were a
  function, every step would pay a call, a return, and a full
  register spill at the boundary, and no value could stay in a
  register across steps — which is Neumann's §4.1 "the hot path does
  not cross a function boundary" rule. Copy-and-patch gets away with
  one stencil per node only because it uses the GHC calling
  convention and tail calls, so the "calls" lower to jumps and
  parameters stay in registers.

  </details>

- [ ] You can compute the row count at which compiling an expression pays for itself, and explain why Postgres structurally cannot use that number.

  <details><summary>Answer</summary>

  `rows_breakeven = compile_µs / (µs_per_row_interp −
  µs_per_row_jit)`. With this topic's depth-8 measurements
  (`notes.md`): 1.053 µs/row interpreted, 0.0847 µs/row for the
  vectorized lane the JIT is predicted to match, saving 0.968
  µs/row; a 500 µs compile pays back at **516 rows**. Postgres
  cannot use it because both inputs are unavailable at the moment
  it decides: `planner.c:698-721` runs before execution, so
  `rows_seen` is zero and `µs_per_row` has never been observed —
  all it has is `top_plan->total_cost`, an estimate in units
  calibrated for page reads. The fix is not a better threshold, it
  is deciding later; that is what adaptive execution means in
  `reading-umbra-tidy-tuples.md`.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the per-query-no-cache versus GraphBLAS-cache-forever contrast.

  <details><summary>Answer</summary>

  The contrast to write down: Postgres's compiled code is owned by
  the query — `llvm_release_context` calls
  `LLVMOrcResourceTrackerRemove` at `llvmjit.c:288`, so the code is
  unmapped when the query ends and the next identical query
  recompiles from scratch. GraphBLAS keys its kernels on a
  *semiring × types × sparsity* encoding and keeps them in an
  in-memory hash table plus an on-disk `.so` cache that survives
  process restarts (`reading-graphblas-jit.md`). Which is right
  depends on how many distinct artifacts the workload has: Postgres
  has effectively unbounded distinct expressions (every literal is
  a new one unless parameterized), GraphBLAS has a small closed set.
  For Cypher, parameterize the constants and key on expression
  *shape* — then count the distinct shapes in a real workload
  before choosing.

  </details>

## References

**Code** — all anchors verified at postgres `701f021` (PG 20devel)

| file | anchors |
|---|---|
| `src/backend/jit/jit.c` | `:33` `jit_enabled = false`; `:37-42` the other GUC defaults |
| `src/backend/optimizer/plan/planner.c` | `:698-721` the gate |
| `src/backend/jit/llvm/llvmjit_expr.c` | `:80` entry; `:301-307` blocks; `:324` the switch; `:344-348` FETCHSOME; `:404-410` deform hook; `:434-437` fallback; `:972-1007` `EEOP_QUAL` |
| `src/backend/jit/llvm/llvmjit.c` | `:51` tracker field; `:100-101` two LLJITs; `:155-156` provider hooks; `:253` release; `:288-289` teardown; `:363` `llvm_get_function`; `:710-781` compile + tracker |
| `src/backend/jit/llvm/llvmjit_deform.c` | `slot_compile_deform` — the specialized decoder |
| `src/backend/executor/execExprInterp.c` | `:113-137` threaded dispatch; `:484-612` dispatch table; `:608` the static assert |
| `src/include/executor/execExpr.h` | `:296` `EEOP_LAST` |
| `src/backend/utils/misc/guc_parameters.dat` | `:1451-1456` the generated `jit` GUC |
| `src/backend/utils/misc/postgresql.conf.sample` | `:467`, `:470`, `:472`, `:492` |
| `doc/src/sgml/config.sgml` | `:6483-6499` cost GUCs; `:6836` "The default is `off`" |

**Elsewhere in this repo**
- `experiments/src/jit.rs:11-19` — the same block-per-node
  translation for CLIF; `:26-31` the ownership pattern this
  chapter's Step 5 is the source of
- `reading-sqlite-vdbe.md` — the interpreter that does *not* thread
- `reading-umbra-tidy-tuples.md` — deciding after the evidence
  arrives instead of before
- `reading-neumann-vldb11.md` — §4.1's rule for what stays
  precompiled, which is exactly why Postgres JITs leaves only
