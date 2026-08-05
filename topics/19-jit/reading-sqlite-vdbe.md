# SQLite's VDBE: the bytecode floor

The oldest shipping answer to interpretation overhead: don't walk
the AST, flatten it to bytecode once at prepare time, then run a
register machine. 25 years in production, zero JIT, and for
SQLite's workload (few rows per query, embedded) it is the RIGHT
point on the spectrum — the floor every JIT must beat before its
compile time counts. This chapter builds the machine step by step —
why bytecode beats a tree walk, what a register machine is, what
dispatch costs, and the coroutine trick flattening gives you for
free — then maps each step into vdbe.c.

**Version.** Every anchor below is against sqlite at the pin in
`resources/codebases.md`, **`951de30`**, where `src/vdbe.c` is 9456
lines. Retrieve any of them with
`python3 tools/pinned-source.py show sqlite src/vdbe.c -r 1926:2010`.
SQLite's opcode numbering is generated at build time by
`mkopcodeh.tcl` scanning this file, so line numbers here move
between releases but the *structure* has been stable since 2004.

## The problem in one sentence

Walking an AST (abstract syntax tree — the parsed expression as
linked nodes) costs a recursive virtual call plus a pointer chase
*per node per row*; SQLite pays a one-time flattening at prepare
time and gets per-row cost down to a few array-indexed `switch`
dispatches — with zero compile latency, which for queries that touch
five rows beats any JIT.

## The concepts, step by step

### Step 1 — flatten once: from AST to a bytecode program

> **In:** SQL text and, from topic 19's framing, a tree-walking
> evaluator that pays a dispatch per node per row.
> **Out:** a contiguous `VdbeOp[]` array and one `for(;;)` loop
> that walks it — the object every later step operates on.

**Bytecode** is a program encoded as an array of small fixed-format
instructions for a software-defined machine (a "virtual machine" —
here the VDBE, Virtual DataBase Engine). At `sqlite3_prepare` time
the SQL is parsed to an AST and immediately *code-generated* into a
flat `VdbeOp[]` array; execution never sees the tree:

```
 prepare:  SQL ──parse──► AST ──codegen──► VdbeOp[] program
 execute:  for(pOp=&aOp[p->pc]; 1; pOp++){    ← vdbe.c:966
             switch(pOp->opcode){ ... }       ← vdbe.c:1049
             ... break, or goto jump_to_p2 }  ← vdbe.c:1221
           }                                  ← vdbe.c:9357

 state: array of Mem registers (typed values), array of cursors
 (open B-tree positions).  A register machine, NOT a stack machine
 — p1/p2/p3 name registers directly, no push/pop traffic.
```

The loop header carries the whole design in one line:

```c
// sqlite/src/vdbe.c — the entire interpreter loop header, 966-972
 966	  for(pOp=&aOp[p->pc]; 1; pOp++){
 967	    /* Errors are detected by individual opcodes, with an immediate
 968	    ** jumps to abort_due_to_error. */
 969	    assert( rc==SQLITE_OK );
 970
 971	    assert( pOp>=aOp && pOp<&aOp[p->nOp]);
 972	    nVmStep++;
```

Read line 966 twice. The program counter is not an index — it is a
**pointer walked with `pOp++`**, over an array. Sequential execution
is a pointer increment with no bounds check in a release build (971
is an `assert`), and the hardware prefetcher sees a perfectly linear
instruction stream. That is the entire structural win over a tree
walk, before a single opcode has run. `nVmStep` at 972 is what
`sqlite3_stmt_status(SQLITE_STMTSTATUS_VM_STEP)` reports — the count
you will use in question 1.

What flattening buys immediately: instructions live contiguously
(cache-linear, no pointer chasing), the interpreter is one loop
instead of recursion, and the program is *inspectable* — run
`EXPLAIN SELECT ...` in any sqlite3 shell to see one. Question 1
asks you to read one:

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

### Step 2 — registers, not a stack: the machine model

> **In:** Step 1's `VdbeOp[]` array and its `Mem` register file.
> **Out:** the operand-naming convention (p1/p2/p3 as register
> indices) and a *count* of dispatches per expression — the unit
> Step 4 prices.

A **stack machine** (JVM, Python) makes every instruction implicitly
pop operands and push results — simple codegen, but `a*b + c*d`
costs ~7 push/pop-shuffling dispatches. A **register machine** names
its operands directly: each VdbeOp carries integers p1, p2, p3 that
index into an array of `Mem` registers (typed value slots), so
`Add r1 r2 r3` is one instruction and intermediate values just *stay
put*. Fewer instructions = fewer dispatches = less interpreter tax
per row; the price is that the code generator must do register
allocation (decide which value lives in which slot).

Do the count, because it is the whole argument:

```
 a*b + c*d  on a STACK machine (JVM-style):
   load a; load b; mul; load c; load d; mul; add
   = 7 dispatched instructions, and every intermediate goes
     through the operand stack (a store + a load each)

 a*b + c*d  on the VDBE (register machine):
   Column  0 ia  r1        ; a → r1
   Column  0 ib  r2        ; b → r2
   Multiply r1 r2 r5       ; r5 = r1*r2   ← operands NAMED
   Column  0 ic  r3
   Column  0 id  r4
   Multiply r3 r4 r6
   Add      r5 r6 r7       ; r7 = r5+r6
   = 7 dispatched instructions too — BUT the four Column ops are
     work the stack machine also needs. Compare arithmetic only:
       stack:    mul, mul, add + 3 implicit push/pop pairs
       register: mul, mul, add + 0 shuffling ops
     3 dispatches vs 7 → 2.3× fewer, for the same three flops.
```

Alongside the registers sits an array of **cursors** — open
positions inside B-trees (topic 1's structure; a cursor is "where I
am in table t") — which the opcode set manipulates directly
(OpenRead `:4421`, Rewind `:6407`, Next `:6545`, Column `:3010`).

### Step 3 — the ISA: fixed-width ops, one convention per field

> **In:** Step 2's operand convention. **Out:** the exact byte
> layout of one instruction, the real size of the instruction set,
> and the reason forward jumps need only one codegen pass.

**Corrected anchor.** The struct is in **`src/vdbe.h:55`**, not
`src/vdbeInt.h:55`. `vdbeInt.h` only carries `typedef struct VdbeOp
Op;` at its line 46. The old anchor pointed at the right line number
of the wrong file, which is the most expensive kind of wrong: it
looks checkable and isn't.

```c
// sqlite/src/vdbe.h — struct VdbeOp, 55-95, p4 union elided
  55	struct VdbeOp {
  56	  u8 opcode;          /* What operation to perform */
  57	  signed char p4type; /* One of the P4_xxx constants for p4 */
  58	  u16 p5;             /* Fifth parameter is an unsigned 16-bit integer */
  59	  int p1;             /* First operand */
  60	  int p2;             /* Second parameter (often the jump destination) */
  61	  int p3;             /* The third parameter */
  62	  union p4union {     /* fourth parameter */
// ... 63-81: 16 alternatives — int, char*, FuncDef*, CollSeq*, Mem*,
// ...        KeyInfo*, SubProgram*, Table*, Index*, … ...
  82	  } p4;
  83	#ifdef SQLITE_ENABLE_EXPLAIN_COMMENTS
  84	  char *zComment;          /* Comment to improve readability */
  85	#endif
// ... 86-93: iSrcLine (SQLITE_VDBE_COVERAGE), nExec/nCycle
// ...        (SQLITE_ENABLE_STMT_SCANSTATUS || VDBE_PROFILE) ...
  94	};
  95	typedef struct VdbeOp VdbeOp;
```

Compute the size, because "fixed 24-ish bytes" is the kind of claim
that should never be approximate:

```
 opcode   u8            1 byte
 p4type   signed char   1
 p5       u16           2
 p1,p2,p3 int × 3      12          subtotal 16, already 8-aligned
 p4       union         8          (largest member is a pointer)
                       ----
 plain 64-bit build:   24 bytes exactly

 with SQLITE_ENABLE_EXPLAIN_COMMENTS: +8  (zComment) = 32
 with SQLITE_VDBE_COVERAGE:           +8  (u32 + pad) = 40
 with VDBE_PROFILE:                   +16 (nExec, nCycle) = 56
```

24 bytes means **2.67 instructions per 64-byte cache line**; a
1000-op program is 24 KB and fits in L1d on most machines. It also
means the *debug* builds you measure with are 2.3× wider than the
shipping one — measure the plain build (Step 7).

**Corrected count: the ISA is 190 opcodes, not 199.** Count it the
way SQLite's own build does. `src/vdbe.c:1060-1065` documents the
convention:

> "The makefile for SQLite generates two C files "opcodes.h" and
> "opcodes.c" by scanning this file looking for lines that **begin
> with** "case OP_"."

So the opcode count is the number of *flush-left* `case OP_` lines:

```
 grep -cE '^case OP_' src/vdbe.c   → 190      ← the ISA
 grep -cE  'case OP_' src/vdbe.c   → 199      ← the naive count

 the 9 extra are:
   :1062                 the documentation comment quoted above
   :1948-1951 (4)        the INNER switch inside OP_Add's body
   :1976-1979 (4)        a THIRD switch, for the floating-point path
```

The naive 199 is what the topic README still prints. Hold on to
those 8 inner cases — they are Step 4's punchline.

p2 is *always* the jump target by convention (`vdbe.h:60`: "often
the jump destination"), and every jumping opcode funnels through one
shared label:

```c
// sqlite/src/vdbe.c — the shared jump tail, 1219-1225 (inside OP_InitCoroutine)
1219	  /* Most jump operations do a goto to this spot in order to update
1220	  ** the pOp pointer. */
1221	jump_to_p2:
1222	  assert( pOp->p2>0 );       /* There are never any jumps to instruction 0 */
1223	  assert( pOp->p2<p->nOp );  /* Jumps must be in range */
1224	  pOp = &aOp[pOp->p2 - 1];
1225	  break;
```

Line 1224 is why forward jumps need one codegen pass: the target is
an operand the generator can patch in place later, and `-1`
compensates for the `pOp++` in the loop header at :966. Nothing in
the program moves when a jump is resolved, because instructions are
fixed-width.

Compare Umbra's IR — the instinct is the same (contiguous array,
integer offsets, single-pass friendly) but the layout is *not*: see
`reading-umbra-tidy-tuples.md`, where the instructions are
**variable-length** because Umbra never has to index into the middle
of one at runtime, only append.

### Step 4 — dispatch cost: what bytecode buys and what it doesn't

> **In:** Step 3's 190-case instruction set and Step 2's dispatch
> count per expression. **Out:** a cycle price per dispatch, and
> the work-per-op ratio that decides whether that price matters —
> the number that defines a JIT's entire opening.

The interpreter core is one `switch` on the opcode, at
`src/vdbe.c:1049`, closing at `:9357` — 8,300 lines of cases. A
`switch` over a dense integer range compiles to one **indirect
branch** (a jump whose target comes from data, so the CPU must
*predict* where it goes) through a jump table. The predictor sees
ONE hot indirect jump with 190 possible targets — mispredict-prone
(topic 17's branchy filter, interpreter edition).

**Corrected claim.** The previous version of this guide said
SQLite "gains limited benefit and keeps the portable switch by
default" from threaded dispatch. That implies a threaded option
exists in the source and is switched off. At `951de30` it does not:

```
 grep -niE 'computed.goto|COMPUTED_GOTO|dispatch_table|&&CASE' src/vdbe.c
   → no matches
```

There is exactly one `switch` in `vdbe.c` (`:1049`, plus the two
*inner* ones inside OP_Add), and exactly two ways out of an opcode
body: `break` (fall to the loop's `pOp++`) or `goto jump_to_p2`
(`:1221`). The honest sentence is: **SQLite's VDBE uses a single
portable `switch`; the threaded-dispatch alternative is not
implemented in this file.** Postgres's *expression* interpreter, by
contrast, really does thread — `execExprInterp.c:119-122` defines
`EEO_DISPATCH()` as `goto *((void *) op->opcode)` — so the contrast
this guide wants is available; it is just across projects, not
inside SQLite. See `reading-postgres-jit.md`.

Either way you pay ~5–20 cycles of dispatch per op. The verdict
depends entirely on what an op *does*. Measure that with a ruler —
the line span of each case body:

| opcode | body | lines | dispatch as a share of the op |
|---|---|---|---|
| `OP_Column` | `:3010`–`:3339` | ~330 | noise — record decode dominates |
| `OP_Add`/`Sub`/`Mul`/`Div`/`Rem` | `:1926`–`:2010` | ~85 | still not one flop |
| `OP_Goto` | `:1098`–`:1104` | ~7 | dispatch *is* the op |

**The `OP_Add` correction is the important one.** The previous
version called it "~10 lines". It is 85, and its shape is the best
argument for a JIT anywhere in this topic:

```c
// sqlite/src/vdbe.c — OP_Add's shared body, 1938-1948 and 1975-1976
1938	  pIn1 = &aMem[pOp->p1];
1939	  type1 = pIn1->flags;
1940	  pIn2 = &aMem[pOp->p2];
1941	  type2 = pIn2->flags;
1942	  pOut = &aMem[pOp->p3];
1943	  if( (type1 & type2 & MEM_Int)!=0 ){
1944	int_math:
1945	    iA = pIn1->u.i;
1946	    iB = pIn2->u.i;
1947	    switch( pOp->opcode ){
1948	      case OP_Add:       if( sqlite3AddInt64(&iB,iA) ) goto fp_math;  break;
// ... 1949-1974: the other four integer ops, the NULL path, numericType() ...
1975	    switch( pOp->opcode ){
1976	      case OP_Add:         rB += rA;       break;
```

Count what one `a+b` on two doubles costs here: an outer dispatch at
:1049, three register-array loads (:1938–:1942), a dynamic type test
(:1943), a fall to the float path, two `sqlite3VdbeRealValue` calls,
**a second dispatch on the same opcode at :1975**, and then — at
:1976 — the single `addsd` that is the actual work. The interpreter
switches on `pOp->opcode` *twice* for one arithmetic instruction,
and re-derives the operand types on every row even though they were
knowable at prepare time.

That is exactly the overhead a JIT deletes: types are resolved once
at compile time, so the emitted code for `a+b` is one `addsd` with
no dispatch, no flag test, and no second switch. This topic's own
lane measures the same effect from the other side — the tree-walking
interpreter in `experiments/src/interp.rs:8-16` falls from 89.4 to
0.95 M rows/s (**94×**) as an expression grows 7 → 511 nodes, while
the vectorized lane falls only 47×, because the vectorized lane pays
its dispatch once per node per *batch*.

And SQLite's few-rows workload simply doesn't have the ratio that
would justify paying compile time. Do the division in Step 6.

### Step 5 — coroutines: the feature flattening gives you for free

> **In:** Step 1's "program counter is a pointer into an array".
> **Out:** a suspend/resume primitive that costs one integer — the
> concrete answer to Neumann's complaint about resumability state.

A **coroutine** is a function that can suspend mid-execution and be
resumed later. For a tree-walking interpreter, suspension is hard —
the "where was I" state is a native call stack. For flattened
bytecode it is trivial: the entire position is one integer, the
program counter.

```c
// sqlite/src/vdbe.c — OP_Yield, the whole opcode, 1264-1274
1264	case OP_Yield: {            /* in1, jump0 */
1265	  int pcDest;
1266	  pIn1 = &aMem[pOp->p1];
1267	  assert( VdbeMemDynamic(pIn1)==0 );
1268	  pIn1->flags = MEM_Int;
1269	  pcDest = (int)pIn1->u.i;
1270	  pIn1->u.i = (int)(pOp - aOp);
1271	  REGISTER_TRACE(pOp->p1, pIn1);
1272	  pOp = &aOp[pcDest];
1273	  break;
1274	}
```

Lines 1269–1272 are the entire context switch: read the other side's
pc out of register p1, write *my* pc into the same register, jump.
One `Mem` slot holds a whole coroutine's resumption state, and
`OP_InitCoroutine` seeds it at `:1215` with `pOut->u.i = pOp->p3 -
1` (the `-1` again compensating for `pOp++`). A subquery becomes a
coroutine whose pc lives in a register, so
`INSERT INTO t SELECT ...` streams rows from the SELECT program
without materializing it.

This is the same resumability argument as topic 7's io_uring state
machines and — importantly — the *inverse* of Neumann's §1 complaint
that operator resumption bookkeeping is "bad code locality and
complex book-keeping". Neumann's escape is to make code that never
needs to resume. SQLite's is to make resumption cost one integer.
Both are valid; they optimize different workloads. Question 3.

### Step 6 — where the VDBE sits, and what transfers to M19

> **In:** Steps 1–5's machine and its per-op cost. **Out:** a
> break-even row count you compute, and a placement on this topic's
> spectrum.

Place it on the topic's spectrum: FalkorDB's expression evaluator
walks a tree per row — it sits LEFT of SQLite. M19's cranelift JIT
jumps two steps right. The VDBE lesson: there is a defensible
middle (flatten to a register program, interpret that) that costs
zero compile time and already kills tree-walk overhead.

Now price SQLite's refusal to JIT, using this topic's own measured
rates (`notes.md`, Apple M3 Pro, 2026-07-10, depth 8 = 511 nodes):

```
 rows_breakeven = compile_time / (per_row_interp − per_row_jit)

 interpreter, 511 nodes: 0.95 M rows/s  →  1.053 µs/row
 vectorized,  511 nodes: 11.8 M rows/s  →  0.0847 µs/row
 saving if a JIT merely matched the vectorized lane:
                          1.053 − 0.0847 = 0.968 µs/row

 a 500 µs cranelift compile pays back after
     500 / 0.968 = 516 rows

 SQLite's median query touches far fewer than 516 rows. At 5 rows:
     JIT total  = 500 µs + 5 × 0.085 µs = 500.4 µs
     interp     =          5 × 1.053 µs =   5.3 µs
     the JIT is 94× SLOWER end to end.
```

That is the whole answer to "why has SQLite never JIT'd". It is not
conservatism; it is the arithmetic of a workload where the compile
fee is never amortized. Change the workload — an OLAP scan of 10
million rows — and the same division gives the opposite verdict:
compile 500 µs, save 9.68 s.

Worth benching a fourth lane in jit_bench if the JIT crossover
disappoints (question 5): flattening our `Expr` to a `Vec<Op>` costs
no compile latency at all and should land between `interp` and
`vector`.

### Step 7 — how to read this file: turn on VDBE_PROFILE

> **In:** the source at the pin, and a build you control.
> **Out:** a per-opcode cycle count, so every claim in Steps 4–6
> becomes something you measured rather than something you read.

`vdbe.c` ships with the instrument already installed. Three sites:

```c
// sqlite/src/vdbe.c — the VDBE_PROFILE cycle counter, 27-31 / 974-977 / 9320-9322
  27	#if defined(VDBE_PROFILE)  \
  28	 || defined(SQLITE_PERFORMANCE_TRACE) \
  29	 || defined(SQLITE_ENABLE_STMT_SCANSTATUS)
  30	# include "hwtime.h"
  31	#endif
// ... 32-973: everything up to the top of the dispatch loop ...
 974	#if defined(VDBE_PROFILE)
 975	    pOp->nExec++;
 976	    pnCycle = &pOp->nCycle;
 977	    if( sqlite3NProfileCnt==0 ) *pnCycle -= sqlite3Hwtime();
// ... 978-9319: the switch and all 190 opcode bodies ...
9320	#if defined(VDBE_PROFILE)
9321	    *pnCycle += sqlite3NProfileCnt ? sqlite3NProfileCnt : sqlite3Hwtime();
9322	    pnCycle = 0;
```

Subtract the timestamp before the opcode, add it after: `nCycle`
accumulates real cycles per instruction *slot* (not per opcode kind
— per address in the program), and `nExec` counts executions. Build
with `-DVDBE_PROFILE`, run your query, and you get the measured
version of Step 4's table for your own workload. Remember Step 3:
this build's `VdbeOp` is 56 bytes, not 24, so absolute cache
behaviour differs — use it for *ratios* between opcodes.

Two neighbouring hooks are worth knowing: `SQLITE_DEBUG` adds an
operand sanity pass over p1/p2/p3 at `:1009-1044` before every
dispatch (so never benchmark a `SQLITE_DEBUG` build), and
`SQLITE_ENABLE_STMT_SCANSTATUS` is the same counters exposed through
the public `sqlite3_stmt_scanstatus()` API without a custom build.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| `src/vdbe.h:55-95` | `struct VdbeOp` — 24 bytes: opcode, p4type, p5, p1..p3, p4 union | 3 |
| `src/vdbeInt.h:46` | `typedef struct VdbeOp Op;` — the *only* thing here (old anchor's target) | 3 |
| `src/vdbe.c:966` | THE loop header: `for(pOp=&aOp[p->pc]; 1; pOp++)` | 1 |
| `src/vdbe.c:1049` | THE dispatch: `switch( pOp->opcode )`, closing at `:9357` | 1, 4 |
| `src/vdbe.c:1060-1065` | the flush-left `case OP_` convention mkopcodeh scans for | 3 |
| `src/vdbe.c:1098` | OP_Goto — 7 lines; dispatch *is* the op | 4 |
| `src/vdbe.c:1154` / `:1187` | OP_Gosub / OP_Return — subroutines via a register | 2, 5 |
| `src/vdbe.c:1209-1226` | OP_InitCoroutine (`:1215` seeds pc) + the shared `jump_to_p2` tail | 3, 5 |
| `src/vdbe.c:1238` / `:1264-1274` | OP_EndCoroutine / OP_Yield — the pc swap | 5 |
| `src/vdbe.c:1284` | OP_HaltIfNull — constraint checks as opcodes | 3 |
| `src/vdbe.c:1926-2010` | OP_Add and friends — 85 lines, with switches at `:1947` and `:1975` | 4 |
| `src/vdbe.c:3010-3339` | OP_Column — ~330 lines of record decode | 4 |
| `src/vdbe.c:27-31`, `:974-977`, `:9320-9322` | VDBE_PROFILE per-opcode cycle accounting | 7 |
| `grep -cE '^case OP_'` → **190** | the entire ISA | 3 |

Start at the loop header (`:966`), then the dispatch (`:1049`), then
read opcodes in file order — the flush-left `case OP_` convention
makes the 190-case file navigable. Keep an `EXPLAIN` output from
Step 1 beside you and find each opcode it uses; Step 5's coroutine
pair is the detour worth taking whole. Read `OP_Add` (`:1926`) and
`OP_Column` (`:3010`) back to back — that contrast is Step 4.

## Questions for notes.md

1. Run `EXPLAIN SELECT a+1 FROM t WHERE b<10` (any SQLite). Paste
   the program; identify the loop (Rewind/Next), the filter (Ge/Lt
   with p2 jump), the expression ops. How many dispatched ops per
   row? Cross-check your hand count against
   `sqlite3_stmt_status(stmt, SQLITE_STMTSTATUS_VM_STEP, 0)`, which
   returns the `nVmStep` incremented at `src/vdbe.c:972`.
2. Register machine vs stack machine: count the ops `a*b + c*d`
   needs on each (Step 2 does it — redo it without looking). Why
   did SQLite pick registers, and what did it have to buy with that
   choice (register allocation in the code generator)?
3. OP_Yield: trace the pc swap at `src/vdbe.c:1269-1272` between
   coroutine and caller. What exactly is saved/restored (ONE
   register holding pc — why is that sufficient, i.e. where do the
   coroutine's locals live)? Then answer the inverse: what would
   this cost in Neumann's compiled pipeline, where there is no
   program counter to save?
4. `OP_Column` is ~330 lines (`:3010-3339`) and `OP_Add` is ~85
   (`:1926-2010`) — but only ~3 of `OP_Add`'s lines are arithmetic.
   Compute the ratio of *useful work* to dispatch for each, and say
   which one a JIT should target first. Bonus: what do the two
   inner switches at `:1947` and `:1975` cost, and why can't the
   compiler hoist them?
5. Sketch the fourth lane: a bytecode compiler for our `Expr` enum
   (flatten to `Vec<Op>` with register slots, interpret with one
   match). Predict where it lands between `interp` (0.95 M rows/s
   at 511 nodes) and `vector` (11.8 M rows/s) — then (stretch)
   build it and check. Justify the prediction with a dispatch
   count, not a feeling.

## Done when

Answer each before unfolding it.

- [ ] You can explain what flattening an AST into bytecode buys before any compilation is involved — and check it against this topic's measured interpreter numbers, which fall 94× from 7 nodes to 511.

  <details><summary>Answer</summary>

  It buys three things, none of which require a compiler: (1) the
  program is contiguous, so the pc is a pointer increment
  (`vdbe.c:966`) and the prefetcher sees a linear stream, instead of
  chasing `Box<Expr>` pointers; (2) the recursion is gone — one loop
  replaces a call per node, so there is no stack traffic per node;
  (3) any work that can be resolved at prepare time (register
  assignment, jump targets, affinities) is resolved once instead of
  per row. What it does *not* buy is elimination of per-op dispatch
  or of dynamic type tests — `OP_Add` still switches twice
  (`:1947`, `:1975`) and still tests `MEM_Int` flags at `:1943`
  every row. That residue is what this topic's 94× degradation
  measures: `experiments/src/interp.rs` goes 89.4 → 0.95 M rows/s
  from 7 to 511 nodes because the per-node dispatch cost is paid per
  row and scales with node count. Flattening removes the pointer
  chase; only compiling removes the dispatch.
  </details>

- [ ] You can say why a register machine beats a stack machine here, and count the ops for `a*b + c*d` under each.

  <details><summary>Answer</summary>

  A stack machine needs 7 dispatched instructions (`load a, load b,
  mul, load c, load d, mul, add`) and every intermediate makes a
  round trip through the operand stack. The VDBE needs 4 `Column`
  ops (which the stack machine also needs) plus exactly 3 arithmetic
  ops (`Multiply r1 r2 r5`, `Multiply r3 r4 r6`, `Add r5 r6 r7`)
  because p1/p2/p3 name the operands directly — 3 dispatches vs 7
  for the same three flops, and zero shuffling. The price is that
  the code generator must do register allocation. SQLite pays it
  because dispatch, at ~5–20 cycles, is the expensive resource and
  compile-time register allocation is free.
  </details>

- [ ] You can explain what dispatch cost bytecode removes and what it leaves behind, and state correctly how SQLite dispatches.

  <details><summary>Answer</summary>

  Removed: the per-node recursive call, the pointer chase, and the
  re-parse. Left behind: one indirect branch per opcode through a
  190-target jump table at `vdbe.c:1049`, ~5–20 cycles depending on
  prediction, *plus* whatever dynamic type dispatch the opcode body
  does itself. On the correction: at pin `951de30` SQLite has **no
  computed-goto / threaded-dispatch option** — grepping
  `computed goto|dispatch_table|&&CASE` in `vdbe.c` finds nothing.
  It is one portable `switch`, exited by `break` or `goto
  jump_to_p2` (`:1221`). The project that *does* thread its
  expression interpreter is Postgres —
  `execExprInterp.c:119-122`, `EEO_DISPATCH()` → `goto *((void *)
  op->opcode)`.
  </details>

- [ ] You can trace `OP_Yield` and explain how flattening gives coroutines for free.

  <details><summary>Answer</summary>

  `vdbe.c:1264-1274`. `pIn1` is register p1. Line 1269 reads the
  destination pc out of it; line 1270 writes the *current* pc (`pOp
  - aOp`, an integer offset) back into the same register; line 1272
  sets `pOp = &aOp[pcDest]`. A single `Mem` slot is the entire
  saved context of a coroutine. It is free because flattening made
  the execution position a scalar: with a tree walk the position is
  a native call stack, which you cannot store in a register. Locals
  live in the coroutine's own VDBE registers, which are never
  reused by the caller, so nothing else needs saving.
  `OP_InitCoroutine:1215` seeds the register with `p3 - 1`.
  </details>

- [ ] You can compute why SQLite has never needed a JIT, from a compile time and two measured per-row rates.

  <details><summary>Answer</summary>

  `rows = compile_time / (per_row_interp − per_row_jit)`. Using this
  topic's depth-8 measurements (`notes.md`): interpreter 0.95 M
  rows/s = 1.053 µs/row, vectorized 11.8 M rows/s = 0.0847 µs/row,
  saving 0.968 µs/row. A 500 µs compile breaks even at **516 rows**.
  SQLite's design centre is queries touching a handful of rows: at
  5 rows the JIT costs 500.4 µs against the interpreter's 5.3 µs —
  94× *slower*. The refusal is arithmetic, not conservatism.
  Neumann's Table 1 shows the same effect from the OLTP side: TPC-C
  transactions touch under 30 tuples, so switching HyPer's codegen
  from C++ to LLVM bought +4.8% throughput and a 20× compile-time
  reduction — the compile time was the only thing that mattered.
  </details>

- [ ] You can state the true size and layout of the instruction set, and how you counted.

  <details><summary>Answer</summary>

  **190 opcodes**, counted the way SQLite's own build counts them:
  `mkopcodeh` scans `vdbe.c` for lines that *begin with* `case OP_`
  (the convention is documented at `:1060-1065`), so
  `grep -cE '^case OP_' src/vdbe.c` → 190. The naive
  `grep -cE 'case OP_'` gives 199 because it also catches the
  documentation comment at `:1062` and the eight inner-switch cases
  at `:1948-1951` and `:1976-1979`. Layout: `struct VdbeOp` at
  **`src/vdbe.h:55`** (not `vdbeInt.h`) — `u8 opcode` + `signed char
  p4type` + `u16 p5` + three `int` operands + an 8-byte union = 24
  bytes exactly in a plain 64-bit build, growing to 32/40/56 under
  `SQLITE_ENABLE_EXPLAIN_COMMENTS`, `SQLITE_VDBE_COVERAGE` and
  `VDBE_PROFILE`.
  </details>

- [ ] You wrote answers to all five questions in notes.md, including a sketch of a bytecode lane for this topic's `Expr` enum.

  <details><summary>Answer</summary>

  The sketch that matches this chapter: lower `Expr`
  (`experiments/src/expr.rs:11-21`) to `Vec<Op>` in postorder, with
  a register counter assigning each node a slot — `Op::Col{col,
  dst}`, `Op::Const{v, dst}`, `Op::Add{a, b, dst}`, and so on — then
  interpret with one `for op in &prog { match op { … } }` over a
  `Vec<f64>` register file. Prediction to justify with numbers: it
  removes the pointer chase and the recursive call but keeps one
  dispatch per node per row, so it should land close to `interp`
  (0.95 M rows/s at 511 nodes) — perhaps 1.5–3× better — and far
  below `vector` (11.8 M rows/s), which pays one dispatch per node
  per *batch*. If it lands near `vector`, your interpreter lane was
  measuring allocation, not dispatch.
  </details>

## References

**Code** — all anchors verified at sqlite `951de30`

| anchor | what |
|---|---|
| `src/vdbe.c:966` | the loop header (`pOp++` — the pc is a pointer) |
| `src/vdbe.c:1049`–`:9357` | the one `switch`, and its close |
| `src/vdbe.c:1060-1065` | the `case OP_` convention that defines the ISA size |
| `src/vdbe.c:1221-1225` | `jump_to_p2`, the shared jump tail |
| `src/vdbe.c:1264-1274` | `OP_Yield` — a coroutine switch in 4 lines |
| `src/vdbe.c:1926-2010` | `OP_Add`, with its two inner switches |
| `src/vdbe.c:3010-3339` | `OP_Column`, the other end of the work-per-op scale |
| `src/vdbe.c:27-31`, `:974-977`, `:9320-9322` | `VDBE_PROFILE` |
| `src/vdbe.h:55-95` | `struct VdbeOp` |
| `src/vdbeInt.h:46` | `typedef struct VdbeOp Op;` |

Fetch any of these without a clone:
`python3 tools/pinned-source.py show sqlite src/vdbe.c -r 966:980`.

**Elsewhere in this repo**
- `experiments/src/interp.rs:8-16` — the tree walk the VDBE replaces
- `reading-postgres-jit.md` — the threaded-dispatch interpreter
  SQLite does *not* have (`execExprInterp.c:119-122`)
- `reading-umbra-tidy-tuples.md` — the contrasting IR layout
  (contiguous but variable-length)
- `reading-neumann-vldb11.md` — §1's resumability complaint, of
  which `OP_Yield` is the cheerful counter-example

**Tools**
- `EXPLAIN` in any sqlite3 shell — the fastest way to see programs
- `-DVDBE_PROFILE` + `hwtime.h` — per-opcode cycles (Step 7)
- `sqlite3_stmt_scanstatus()` — the same counters without a custom
  build (`SQLITE_ENABLE_STMT_SCANSTATUS`)
