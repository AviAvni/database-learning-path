# Reading guide — "Counting Immutable Beans" (IFL'19) + "Perceus" (PLDI'21)

The Lean 4 / Koka runtime papers. Read them as a *systems* story:
how a pure functional language gets in-place update performance —
and what that teaches a Rust engine about reference counting.

## The problem

Pure FP = every update copies. Naive RC = inc/dec traffic on every
pointer move (the `Arc<T>` tax, topic 2/9: contended atomics).
GC = throughput but latency + no destructive reuse. Lean's compiler
(Immutable Beans) and Koka's (Perceus) make RC *precise* enough that
copying mostly disappears.

## The two ideas

**1. Borrowed vs owned parameters (Beans).** The compiler infers
which parameters a function merely *inspects* (borrowed — no RC ops)
vs *consumes* (owned — caller transfers the reference). Exactly
Rust's `&T` vs `T`, inferred instead of written. Result: most
inc/dec pairs vanish.

**2. Reuse tokens / functional-but-in-place (both papers).** When a
value's count is 1 at its last use, its memory is handed to the
constructor about to be allocated:

```
  match xs with
  | Cons x rest => Cons (f x) (map f rest)
        │                │
        └─ if RC(xs)==1 ─┘   reuse xs's cell in place: map becomes
                             an in-place loop, zero allocation
```

Perceus refines this to *garbage-free* RC: a reference is dropped at
the exact last use (precise liveness), so peak memory equals live
data — no GC headroom.

```
  naive RC:    inc on copy, dec on scope exit    (chatty, atomic)
  Beans:       borrow inference kills most pairs
  Perceus:     drop-at-last-use + reuse ⇒ uniqueness typing effect
               without the type system
```

## Why this is in a database curriculum

- **The RC(1) fast path is delta-matrix thinking**: mutate in place
  when you're the only owner, copy-on-write otherwise — it's Redis's
  shared objects, FalkorDB's tensor sharing, and `Arc::make_mut` as
  a compiler pass.
- **Borrowed params = zero-cost read path**: an executor passing
  `&Value` down a pipeline (topic 11) is doing manual Beans.
- **Proof relevance**: Lean's kernel checks proofs by *running*
  terms; a fast runtime is why mathlib-scale proof search is viable,
  which is why Lean 4 (not Coq) is the M21 proof target.

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
