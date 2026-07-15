# Perceus: reference counting precise enough to reuse memory

How does a pure functional language (Lean 4, Koka) get in-place
update performance? Two compiler passes — borrow inference and
reuse tokens — make reference counting precise enough that copying
mostly disappears. This chapter builds the problem and both ideas
step by step, then routes you through the two runtime papers as a
*systems* story: they explain why Lean 4 is fast enough to be the
M21 proof target, and what `Arc`-everywhere Rust engines leave on
the table.

## The problem in one sentence

Pure functional semantics say every update copies the structure,
and the naive fix — reference counting — adds an inc/dec (often an
*atomic* one, ~10-40 cycles contended) to every pointer move; two
compiler passes eliminate most of the counting and turn the copies
into in-place loops with zero allocation.

## The concepts, step by step

### Step 1 — immutability means copying

In a pure functional language, values are never mutated: "update
element 3 of the list" *means* "build a new list that differs at
element 3." Semantically clean — old readers keep a consistent
value, no aliasing bugs — but taken literally it turns O(1)
mutations into O(n) copies plus allocator traffic. The whole game
of a functional-language runtime is to keep the semantics while
making the copies not happen. The known escapes each cost
something: a GC (garbage collector) buys allocation throughput
but adds latency and — the subtle loss — can never mutate in
place, because it doesn't know how many references a value has
*right now*.

### Step 2 — reference counting, and its tax

**Reference counting** (RC) tracks, per heap object, how many
pointers refer to it; copy a pointer → increment, drop one →
decrement, count hits zero → free. RC knows something a tracing GC
doesn't: the count *right now* — and RC == 1 means "I am the only
owner," which is a license to mutate in place. The tax is that the
counting itself is chatty: naive RC emits inc/dec on every pointer
move, and in a multithreaded runtime those are atomic operations
on shared cache lines — the `Arc<T>` tax from topics 2/9
(contended atomics: ~10-40+ cycles each, plus the coherency
ping-pong). A hot loop that clones an `Arc` per element can spend
more time counting than computing.

### Step 3 — borrow inference (Immutable Beans): don't count what you only look at

Most inc/dec pairs bracket a function call that merely *reads* its
argument. Lean's compiler pass infers, per parameter, whether the
function **borrows** it (only inspects — caller keeps ownership, no
RC ops emitted at all) or **owns** it (consumes — the caller
transfers its reference, and the callee is responsible for the
eventual dec). Exactly Rust's `&T` vs `T` distinction, *inferred*
instead of written. Result: most inc/dec pairs simply vanish from
the emitted code — the read path of the program stops paying the
RC tax entirely, without the programmer annotating anything.

### Step 4 — reuse tokens: functional-but-in-place

Step 2's license gets cashed here. When a value's count is 1 at its
*last use*, the compiler hands its memory to the constructor about
to be allocated — a **reuse token**:

```
  match xs with
  | Cons x rest => Cons (f x) (map f rest)
        │                │
        └─ if RC(xs)==1 ─┘   reuse xs's cell in place: map becomes
                             an in-place loop, zero allocation
```

What the compiler actually emits for `map`, in Rust-ish form:

```rust
fn map(f: &Closure, xs: Ptr<Cons>) -> Ptr<Cons> {
    if rc(xs) == 1 {
        // reuse token: we are the only owner — xs's cell is handed
        // to the Cons about to be built. map becomes an in-place loop.
        xs.head = f.call(xs.head);
        xs.tail = map(f, xs.tail);
        xs                              // zero allocation
    } else {
        let out = alloc(Cons { head: f.call(xs.head), tail: map(f, xs.tail) });
        dec(xs);                        // dropped at exact last use —
        out                             //   peak memory = live data
    }
}
```

The programmer wrote a pure `map`; unshared inputs run it as an
in-place loop with zero allocation, shared inputs transparently
copy. Copy-on-write, decided per cell at runtime, by a branch the
compiler inserted. The cost: that RC==1 check is a branch per
constructor — question 2 asks when it stops paying.

### Step 5 — Perceus: garbage-free, drop at the exact last use

Perceus (Koka's refinement) makes the counting *precise*: a
reference is dec'd at its exact last use (precise liveness
analysis), not at scope exit. Two consequences: more values hit
RC==1 in time for step 4's reuse (a reference lingering to end of
scope blocks reuse), and — the headline claim — the program is
**garbage-free**: at every point, peak memory equals live data,
with no GC headroom and no deferred frees. The ladder so far:

```
  naive RC:    inc on copy, dec on scope exit    (chatty, atomic)
  Beans:       borrow inference kills most pairs
  Perceus:     drop-at-last-use + reuse ⇒ uniqueness typing effect
               without the type system
```

"Uniqueness typing effect without the type system": languages like
Clean prove uniqueness statically and demand annotations; Perceus
gets the same in-place behavior from a runtime count plus
compile-time precision. What a memory-budgeted system buys from
the garbage-free property is question 3.

### Step 6 — why this is in a database curriculum

- **The RC(1) fast path is delta-matrix thinking**: mutate in place
  when you're the only owner, copy-on-write otherwise — it's Redis's
  shared objects, FalkorDB's tensor sharing, and `Arc::make_mut` as
  a compiler pass.
- **Borrowed params = zero-cost read path**: an executor passing
  `&Value` down a pipeline (topic 11) is doing manual Beans.
- **Proof relevance**: Lean's kernel checks proofs by *running*
  terms; a fast runtime is why mathlib-scale proof search is viable,
  which is why Lean 4 (not Coq) is the M21 proof target.

The transferable design rule: ownership information precise enough
to act on turns "immutable" and "in-place" from opposites into a
runtime branch.

## How to read the paper (with the concepts in hand)

- **Ullrich & de Moura, "Counting Immutable Beans"** — read first:
  the problem framing (steps 1-2), borrow inference (step 3), and
  the first reuse story (step 4). This is Lean 4's actual runtime;
  read the benchmark section asking "which wins come from borrows,
  which from reuse?"
- **Reinking, Xie, de Moura, Leijen, "Perceus"** — read second:
  drop-at-last-use and the garbage-free claim (step 5), plus the
  sharper reuse analysis. The formal core is skimmable; the
  examples and the "functional but in place" section are the
  payload. Keep asking the systems question: what would each pass
  do to a Rust engine that currently clones an `Arc` in a hot loop?

## M21 taste: the proof-vs-test trade-off

Property (topic 20): delta-matrix invariant `DP ∩ M = ∅ ∧ DM ⊆ M`
preserved by set/remove/wait.

- proptest (topic 16): minutes to write, samples the space.
- TLC: model M/DP/DM as small sets, exhaustive at n=4.
- Lean: `theorem set_preserves_inv : inv m → inv (set m i j)` —
  unbounded, but you'll spend a day on set-theory lemmas. Do it once
  to calibrate which properties deserve which tool.

## Questions (answer in notes.md)

1. Where exactly does `Arc<T>` in a Rust engine pay costs that
   Beans-style borrow inference eliminates? (Think: clone in a hot
   loop vs `&` reborrow — topic 9's contended counter.)
2. Reuse tokens require RC==1 checks at runtime. When does that
   branch cost more than it saves (small cells? shared-by-design
   structures like interned strings)?
3. Perceus "garbage-free" claim: what does peak-memory = live-data
   buy a memory-budgeted buffer pool (topic 6) design?
4. Lean proof vs TLC vs proptest for `DP ∩ M = ∅`: rank by (cost to
   write, strength of guarantee, maintenance under refactor).
5. Koka's effect types let Perceus assume no hidden aliasing. What's
   the moral equivalent in Rust that makes `Arc::make_mut` sound?

## References

**Papers**
- Ullrich, de Moura — "Counting Immutable Beans: Reference Counting
  Optimized for Purely Functional Programming" (IFL 2019,
  [arXiv:1908.05647](https://arxiv.org/abs/1908.05647)) — borrow
  inference + the first reuse story; this is Lean 4's runtime
- Reinking, Xie, de Moura, Leijen — "Perceus: Garbage Free
  Reference Counting with Reuse" (PLDI 2021) — drop-at-last-use,
  the garbage-free claim, and the sharper reuse analysis
