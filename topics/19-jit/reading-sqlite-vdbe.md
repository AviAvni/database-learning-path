# Reading guide — SQLite VDBE ([`~/repos/sqlite/src/vdbe.c`](https://github.com/sqlite/sqlite))

The oldest shipping answer to interpretation overhead: don't walk
the AST, flatten it to bytecode once at prepare time, then run a
register machine. 25 years in production, zero JIT, and for
SQLite's workload (few rows per query, embedded) it is the RIGHT
point on the spectrum.

## Anchor map

| anchor | what it is |
|---|---|
| src/vdbe.c:1049 | THE loop: `switch( pOp->opcode )` |
| src/vdbe.c:1062 | comment: file is ordered by `case OP_` convention |
| src/vdbeInt.h:55 | `struct VdbeOp` — opcode, p1,p2,p3 ints, p4 union, p5 flags |
| src/vdbe.c:1098 | OP_Goto — jump = set pOp, `break` re-enters switch |
| src/vdbe.c:1154 / :1187 | OP_Gosub / OP_Return — subroutines via a register |
| src/vdbe.c:1209 / :1264 | OP_InitCoroutine / OP_Yield — coroutines! |
| src/vdbe.c:1284 | OP_HaltIfNull — constraint checks as opcodes |
| 199 `case OP_` total | the entire ISA |

## 1. The machine model

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

Run `EXPLAIN SELECT ...` in any sqlite3 shell to see programs.
Question 1 asks you to read one.

## 2. Dispatch cost — what bytecode buys and what it doesn't

One `switch` = one indirect branch per opcode. The branch predictor
sees ONE hot indirect jump with 199 targets — mispredict-prone
(topic 17's branchy filter, interpreter edition). Threaded dispatch
(computed goto per-op) gives the predictor per-op history; SQLite
gains ~limited benefit and keeps the portable switch by default
(look for `SQLITE_THREADSAFE`-adjacent perf notes and the
OP_-macros). Either way you pay ~5-20 cycles of dispatch per op —
fine when each op does real work (B-tree step), brutal when ops are
`Add r1 r2 r3` on millions of rows. That's the JIT's opening, and
SQLite's workload simply doesn't have it.

## 3. Coroutines — the feature bytecode gets for free

OP_InitCoroutine/OP_Yield (vdbe.c:1209, :1264): a subquery becomes
a coroutine — its program counter lives in a register, Yield swaps
pc values. `INSERT INTO t SELECT ...` streams without materializing
the SELECT. A tree-walking interpreter would need real coroutines
or callbacks; *flattened* bytecode makes suspension trivial (save
one integer). This is the same resumability argument as topic 7's
io_uring state machines and Neumann's §Q2 pull-model pain.

## 4. The ISA design (vdbeInt.h:55)

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

## 5. What transfers to M19

FalkorDB's eval.rs walks an expression tree per row — it sits LEFT
of SQLite on the spectrum. M19's cranelift JIT jumps two steps
right. The VDBE lesson: there is a defensible middle (flatten to
a register program, interpret that) that costs zero compile time
and already kills tree-walk overhead — worth benching as a fourth
lane in jit_bench if the JIT crossover disappoints (question 5).

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
