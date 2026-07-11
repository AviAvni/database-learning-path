# Perceus: reference counting precise enough to reuse memory

How does a pure functional language (Lean 4, Koka) get in-place
update performance? Two compiler passes — borrow inference and
reuse tokens — make reference counting precise enough that copying
mostly disappears. Read the two runtime papers as a *systems*
story: they explain why Lean 4 is fast enough to be the M21 proof
target, and what `Arc`-everywhere Rust engines leave on the table.

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

## References

**Papers**
- Ullrich, de Moura — "Counting Immutable Beans: Reference Counting
  Optimized for Purely Functional Programming" (IFL 2019,
  [arXiv:1908.05647](https://arxiv.org/abs/1908.05647)) — borrow
  inference + the first reuse story; this is Lean 4's runtime
- Reinking, Xie, de Moura, Leijen — "Perceus: Garbage Free
  Reference Counting with Reuse" (PLDI 2021) — drop-at-last-use,
  the garbage-free claim, and the sharper reuse analysis
