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

## The problem in one sentence

Walking an AST (abstract syntax tree — the parsed expression as
linked nodes) costs a recursive virtual call plus a pointer chase
*per node per row*; SQLite pays a one-time flattening at prepare
time and gets per-row cost down to a few array-indexed `switch`
dispatches — with zero compile latency, which for queries that touch
five rows beats any JIT.

## The concepts, step by step

### Step 1 — flatten once: from AST to a bytecode program

**Bytecode** is a program encoded as an array of small fixed-format
instructions for a software-defined machine (a "virtual machine" —
here the VDBE, Virtual DataBase Engine). At `sqlite3_prepare` time
the SQL is parsed to an AST and immediately *code-generated* into a
flat `VdbeOp[]` array; execution never sees the tree:

```
 prepare:  SQL ──parse──► AST ──codegen──► VdbeOp[] program
 execute:  pc = 0
           for(;;){ pOp = &aOp[pc];
             switch(pOp->opcode){ ... }        ← vdbe.c:1049
             pc++ or jump }

 state: array of Mem registers (typed values), array of cursors
 (open B-tree positions).  A register machine, NOT a stack machine
 — p1/p2/p3 name registers directly, no push/pop traffic.
```

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

A **stack machine** (JVM, Python) makes every instruction implicitly
pop operands and push results — simple codegen, but `a*b + c*d`
costs ~7 push/pop-shuffling dispatches. A **register machine** names
its operands directly: each VdbeOp carries integers p1, p2, p3 that
index into an array of `Mem` registers (typed value slots), so
`Add r1 r2 r3` is one instruction and intermediate values just *stay
put*. Fewer instructions = fewer dispatches = less interpreter tax
per row; the price is that the code generator must do register
allocation (decide which value lives in which slot). Alongside the
registers sits an array of **cursors** — open positions inside
B-trees (topic 1's structure; a cursor is "where I am in table t") —
which the opcode set manipulates directly (OpenRead, Rewind, Next,
Column).

### Step 3 — the ISA: fixed-width ops, one convention per field

The instruction format (vdbeInt.h:55) is a fixed struct — the entire
instruction set is 199 opcodes over this one shape:

```c
struct VdbeOp {
  u8 opcode;          /* one byte, 199 used */
  signed char p4type; /* what the union holds */
  u16 p5;             /* flags */
  int p1, p2, p3;     /* register/cursor/jump operands */
  union p4 { int i; char *z; ... KeyInfo*, FuncDef* ... };
}
```

Fixed 24-ish-byte ops, arrays not linked lists — the program is
cache-linear. p2 is *always* the jump target by convention, so the
code generator can fix up forward jumps in one pass. Compare
Umbra's IR (also fixed-width, also single-pass-friendly): same
instinct, different target (interpretation vs fast native lowering).

### Step 4 — dispatch cost: what bytecode buys and what it doesn't

The interpreter core is one `switch` on the opcode — compiled to one
**indirect branch** (a jump whose target comes from data, so the CPU
must *predict* where it goes). The predictor sees ONE hot indirect
jump with 199 possible targets — mispredict-prone (topic 17's
branchy filter, interpreter edition). Threaded dispatch (a computed
goto at the end of *each* opcode) gives the predictor per-op
history; SQLite gains limited benefit and keeps the portable switch
by default (look for the OP_-macros and perf notes).

Either way you pay ~5–20 cycles of dispatch per op. The verdict
depends entirely on what an op *does*: dispatch is noise when the op
is a B-tree step (hundreds of cycles of real work), brutal when the
op is `Add r1 r2 r3` (1 cycle of work, 20 of dispatch) executed
millions of times. That per-op work ratio is the JIT's entire
opening — and SQLite's few-rows workload simply doesn't have it.

### Step 5 — coroutines: the feature flattening gives you for free

A **coroutine** is a function that can suspend mid-execution and be
resumed later. For a tree-walking interpreter, suspension is hard —
the "where was I" state is a native call stack. For flattened
bytecode it is trivial: the entire position is one integer, the
program counter. OP_InitCoroutine/OP_Yield (vdbe.c:1209, :1264)
exploit this: a subquery becomes a coroutine whose pc lives in a
register, and Yield just swaps pc values — so
`INSERT INTO t SELECT ...` streams rows from the SELECT program
without materializing it. This is the same resumability argument as
topic 7's io_uring state machines and Neumann's Q2 pull-model pain.

### Step 6 — where the VDBE sits, and what transfers to M19

Place it on the topic's spectrum: FalkorDB's eval.rs walks an
expression tree per row — it sits LEFT of SQLite. M19's cranelift
JIT jumps two steps right. The VDBE lesson: there is a defensible
middle (flatten to a register program, interpret that) that costs
zero compile time and already kills tree-walk overhead — worth
benching as a fourth lane in jit_bench if the JIT crossover
disappoints (question 5). And why SQLite never JITs: its queries
touch a handful of rows, so no compile cost — however small — can
amortize.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| src/vdbe.c:1049 | THE loop: `switch( pOp->opcode )` | 1, 4 |
| src/vdbe.c:1062 | comment: file is ordered by `case OP_` convention | 1 |
| src/vdbeInt.h:55 | `struct VdbeOp` — opcode, p1,p2,p3 ints, p4 union, p5 flags | 3 |
| src/vdbe.c:1098 | OP_Goto — jump = set pOp, `break` re-enters switch | 4 |
| src/vdbe.c:1154 / :1187 | OP_Gosub / OP_Return — subroutines via a register | 2, 5 |
| src/vdbe.c:1209 / :1264 | OP_InitCoroutine / OP_Yield — coroutines! | 5 |
| src/vdbe.c:1284 | OP_HaltIfNull — constraint checks as opcodes | 3 |
| 199 `case OP_` total | the entire ISA | 3 |

Start at the dispatch loop (:1049) and read opcodes in file order —
the `case OP_` comment convention makes the 199-case file navigable.
Keep an `EXPLAIN` output from Step 1 beside you and find each opcode
it uses; Step 5's coroutine pair is the detour worth taking whole.

## Questions for notes.md

1. Run `EXPLAIN SELECT a+1 FROM t WHERE b<10` (any SQLite). Paste
   the program; identify the loop (Rewind/Next), the filter (Ge/Lt
   with p2 jump), the expression ops. How many dispatched ops per
   row?
2. Register machine vs stack machine: count the ops `a*b + c*d`
   needs on each. Why did SQLite pick registers (fewer dispatches,
   at the cost of codegen doing register allocation)?
3. OP_Yield: trace pc swapping between coroutine and caller. What
   exactly is saved/restored (ONE register holding pc — why is
   that sufficient, i.e. where do the coroutine's locals live)?
4. Why is `case OP_Column` (the B-tree record decoder) enormous
   while `case OP_Add` is ~10 lines — and what does that say about
   where VDBE dispatch overhead actually matters?
5. Sketch the fourth lane: a bytecode compiler for our `Expr` enum
   (flatten to `Vec<Op>` with register slots, interpret with one
   match). Predict where it lands between interp and JIT in
   rows/s, then (stretch) build it and check.

## References

**Code**
- [sqlite](https://github.com/sqlite/sqlite) `src/vdbe.c` — start at
  the dispatch loop (:1049) and read opcodes in file order; the
  `case OP_` comment convention makes it navigable
- [sqlite](https://github.com/sqlite/sqlite) `src/vdbeInt.h` —
  `struct VdbeOp` and the register/cursor state
- `EXPLAIN` in any sqlite3 shell — the fastest way to see programs
