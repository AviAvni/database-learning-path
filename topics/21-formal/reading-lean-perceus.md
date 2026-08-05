# Perceus: reference counting precise enough to reuse memory

How does a pure functional language get in-place-update performance? Not by
giving up purity — by making the reference count precise enough that the runtime
can *see*, at each constructor, that nobody else is looking. This chapter reads
the two papers behind that idea as a systems story, because they explain why
Lean 4 is fast enough to be this topic's proof target, and what an
`Arc`-everywhere Rust engine is leaving on the table.

**Two papers, two languages, and the attribution matters.**

- **Ullrich & de Moura, "Counting Immutable Beans" (IFL 2019)** is **Lean 4's**
  runtime. It contributes ownership-based RC, **borrow inference**, and the first
  `reset`/`reuse` story.
- **Reinking, Xie, de Moura & Leijen, "Perceus: Garbage Free Reference Counting
  with Reuse" (PLDI 2021 / MSR-TR-2020-42)** is **Koka's**. Its own §5 says: "Our
  work is closely based on the reference counting algorithm in the Lean theorem
  prover as described by Ullrich and de Moura [46] … We extend their work with
  drop- and reuse specialization."

So Perceus is downstream of Lean, not the other way round, and — the correction
this chapter turns on — **Perceus deliberately does *not* have borrow
inference.** Its conclusion lists integrating "selective borrowing" as future
work, and says doing so "would make certain programs **no longer be garbage
free**". Borrowing and garbage-freedom pull against each other; the two papers
sit on opposite sides of that trade.

Code anchors are `leanprover/lean4` at **`v4.24.0`**. This repo's pin table
(`resources/codebases.md`) has no lean4 entry, so fetch with the explicit ref:

```
python3 tools/pinned-source.py --ref v4.24.0 show leanprover/lean4 src/include/lean/lean.h -r 112:136
```

## The problem in one sentence

Pure functional semantics say every update copies the structure, and the obvious
fix — reference counting — adds an increment or decrement to every pointer move,
atomic if the object might be shared across threads; the question is how much of
that counting a compiler can delete, and how often the count that survives is
exactly 1 at the moment it would license mutating in place instead of copying.

## The concepts, step by step

### Step 1 — immutability means copying, and what that costs

> **In:** `map f xs` over a one-million-element list.
> **Out:** the byte count a literal implementation allocates, from Lean's actual
> object layout.

In a pure functional language, "update element 3" *means* "build a new value that
differs at element 3". The semantics are clean — old readers keep a consistent
snapshot, no aliasing bugs — but taken literally, O(1) mutations become O(n)
copies plus allocator traffic.

Put a number on it. Lean's object header is four fields:

```c
// leanprover/lean4@v4.24.0 src/include/lean/lean.h, lines 131-136 — the header
   131  typedef struct {
   132      int      m_rc;
   133      unsigned m_cs_sz:16;
   134      unsigned m_other:8;
   135      unsigned m_tag:8;
   136  } lean_object;
```

That is `4 + 2 + 1 + 1 = 8` bytes, and a constructor is the header followed by
its pointer fields:

```c
// leanprover/lean4@v4.24.0 src/include/lean/lean.h, lines 170-173 — a constructor
   170  typedef struct {
   171      lean_object   m_header;
   172      lean_object * m_objs[];
   173  } lean_ctor_object;
```

So a `List.cons` is **8 + 8 + 8 = 24 bytes** today. **Work the copy cost.** A
`map` over `N = 1,000,000` cells, implemented literally, allocates
`1,000,000 × 24 B = 24 MB` for the result while the 24 MB input is still live —
**48 MB peak** to transform a list that is 24 MB.

**A verified drift worth noticing.** The Beans paper (§7.1) says "In a 64-bit
machine, the ctor value header is **16 bytes** long, twice the size of the header
used in OCaml", giving "**32 bytes** to implement a `List Cons` value: 16 bytes
for the header, and 16 bytes for storing the list head and tail." At `v4.24.0`
the header is 8 bytes and the cell 24. The paper is not wrong; it is six years
old, and the runtime got tighter. Cite the paper for 2019 and `lean.h:131-136`
for now.

The escapes each cost something. A tracing GC buys allocation throughput but adds
latency, and — the subtle loss — **can never mutate in place**, because it does
not know how many references a value has *right now*.

### Step 2 — the count is the license: ownership, borrowing, and the calling convention

> **In:** a function that takes a heap value.
> **Out:** the two conventions Lean's runtime defines, and which one permits
> destructive update.

**Reference counting** tracks, per heap object, how many pointers refer to it:
copy a pointer → increment, drop one → decrement, hits zero → free. What RC knows
that a tracing collector does not is the count *at this instant*, and **count == 1
is a license to mutate in place**.

Lean's runtime writes the two conventions down, in a comment, before any of the
analysis exists:

```c
// leanprover/lean4@v4.24.0 src/include/lean/lean.h, lines 142-150 — verbatim
   142  1- "standard" calling convention if it consumes/decrements the RC.
   143     In this calling convention each argument should be viewed as a resource that is consumed by the function.
   144     This is roughly equivalent to `S && a` in C++, where `S` is a smart pointer, and `a` is the argument.
   145     When this calling convention is used for an argument `x`, then it is safe to perform destructive updates to
   146     `x` if its RC is 1.
   147
   148  2- "borrowed" calling convention if it doesn't consume/decrement the RC, and it is the responsibility of the caller
   149     to decrement the RC.
   150     This is roughly equivalent to `S const & a` in C++, where `S` is a smart pointer, and `a` is the argument.
```

Line 145–146 is the whole thesis in two lines: **owned + RC == 1 ⇒ destructive
update is safe.** This is Rust's `T` versus `&T`, spelled out as a runtime ABI —
and Step 7 is about the fact that Lean *infers* which one each parameter gets,
while Rust makes you write it.

### Step 3 — precise (ownership) RC: drop at the last use, not at scope exit

> **In:** the naive scoped discipline — inc on entry, dec at end of scope.
> **Out:** Perceus §2.2's transfer-of-ownership rule and its measured effect on
> `map`.

Perceus §2.2 replaces "retain until scope exit" with "transfer ownership". In the
`Cons` branch of `map`, the head and tail are `dup`ped (a `dup(x)` increments and
returns `x`) and then `drop(xs)` **frees the input cell immediately**, before the
recursive call builds the output. The paper's Figure 1b:

```
fun map( xs, f ) {                        -- Perceus Fig. 1b, §2.2
  match(xs) {
    Cons(x,xx) {
      dup(x); dup(xx); drop(xs)
      Cons( dup(f)(x), map(xx, f))
    }
    Nil { drop(xs); drop(f); Nil }
  }
}
```

The paper is candid that this looks worse: "At first blush, this seems more
expensive than the scoped approach but, as we will see, this change enables many
further optimizations. More importantly, transferring ownership, rather than
retaining it, means we can free an object immediately when no more references
remain. This both increases cache locality and decreases memory usage. **For
`map`, the memory usage is halved**: the list `xs` is deallocated while the new
list `ys` is being allocated."

**Work it.** Step 1's 48 MB peak for `N = 1,000,000` becomes **24 MB** — the
input dies one cell at a time as the output is built, so only one list is ever
fully live. That is the "halved" claim, in bytes, for Lean's current cell size.

### Step 4 — drop specialization: inline the branch, then delete the dead half

> **In:** the generic `drop`.
> **Out:** why inlining it is what makes Step 5 possible.

Perceus §2.3 gives the basic operation as pseudocode:

```
fun drop( x ) {                            -- Perceus §2.3
  if (is-unique(x)) then drop children of x; free(x)
  else decref(x)
}
```

Drop specialization inlines that at each call site, producing Figure 1c:

```
Cons(x,xx) {                               -- Perceus Fig. 1c, §2.3
  dup(x); dup(xx)
  if (is-unique(xs))
    then drop(x); drop(xx); free(xs)
    else decref(xs)
  Cons( dup(f)(x), map(xx, f))
}
```

Now `dup(x)` immediately followed by `drop(x)` is visible to the optimiser on the
unique path and cancels. Inlining a runtime check to expose algebraic
cancellation is exactly the compiler move you would make in any hot path; the
novelty is doing it to memory-management code.

### Step 5 — reuse analysis: hand the freed cell to the next constructor

> **In:** Step 4's `free(xs)` immediately followed by an allocation of the same
> size.
> **Out:** the reuse token, and the allocation count it removes from a red-black
> insert.

Perceus §2.4: "Instead of freeing `xs` and immediately allocating a fresh `Cons`
node, we can try to reuse `xs` directly as first described by Ullrich and de
Moura. Reuse analysis … analyses each match branch, and tries to pair each
matched pattern to allocated constructors **of the same size** in the branch."

The pairing produces a **reuse token**:

```
fun map( xs, f ) {                         -- Perceus §2.4
  match(xs) {
    Cons(x,xx) {
      val ru = drop-reuse(xs)
      Cons@ru( f(x), map(xx, f))
    }
    Nil -> Nil
  }
}
```

and `Cons@ru` compiles to a branch (§2.5): `if (ru != NULL) then { ru->head := x;
ru->tail := xx; ru } else Cons(x,xx)` — in-place when the token is live, `malloc`
otherwise.

**Reuse specialization** (§2.5) sharpens this: "we only specialize constructors
if at least one of the fields stays the same." For red-black insert that is
almost every field, so a rebalance becomes `if (ru!=NULL) then { ru->left := y;
ru }` — one store instead of five.

**Work the arithmetic the paper sets up.** §2.5, on Okasaki's rebalancing after
inlining `bal-left`: "every matched `Node` constructor has a corresponding `Node`
allocation – if we consider all branches we can see that we either match one
`Node` and allocate one, or we match three nodes deep and allocate three. With
reuse analysis this means that **every `Node` is reused in the fast path without
doing any allocations**."

So take the `rbtree` benchmark's **42 million insertions** (§4). If a
rebalance-heavy insert rebuilds a path of `d` nodes, the no-reuse version does
`42 × 10⁶ × d` allocations and the same number of frees; the reuse version does
**zero** on the unique path. At `d = 3` — the deepest rebalance case the paper
names — that is `1.26 × 10⁸` allocate/free pairs deleted from one benchmark run,
before counting the path nodes above the rebalance. The measured consequence is
in §4: the "no-opt" build, with drop/reuse specialization and reuse analysis
disabled, is "**more than 2× slower**".

Two runtime helpers make the fast path visible in C:

```c
// leanprover/lean4@v4.24.0 src/include/lean/lean.h, lines 543-549 and 863-874
   543  static inline bool lean_is_exclusive(lean_object * o) {
   544      if (LEAN_LIKELY(lean_is_st(o))) {
   545          return o->m_rc == 1;
   546      } else {
   547          return false;
   548      }
   549  }
   863  static inline lean_obj_res lean_ensure_exclusive_array(lean_obj_arg a) {
   864      if (lean_is_exclusive(a)) return a;
   865      return lean_copy_array(a);
   866  }
   868  static inline lean_object * lean_array_uset(lean_obj_arg a, size_t i, lean_obj_arg v) {
   869      lean_object * r   = lean_ensure_exclusive_array(a);
   870      lean_object ** it = lean_array_cptr(r) + i;
   871      lean_dec(*it);
   872      *it = v;
   873      return r;
   874  }
```

`lean_ensure_exclusive_array` (863–866) is `Arc::make_mut`, letter for letter,
and `lean_array_uset` (868–874) is a *functional* array write that mutates when
unshared. Note line 547: for a multi-threaded object `lean_is_exclusive` returns
**false unconditionally**, even at count 1 — Step 8's point.

### Step 6 — FBIP: reuse as a programming discipline

> **In:** Step 5's reuse, applied deliberately rather than opportunistically.
> **Out:** what Perceus §2.6 claims you can now write without allocating.

The paper's framing: "Just like tail-call optimization lets us describe loops in
terms of regular function calls, **reuse analysis lets us describe in-place
mutating imperative algorithms in a purely functional way** (and get persistence
as well)." That is **FBIP**, "functional but in place".

The worked example is Knuth's 1968 problem — traverse a tree in order with no
extra stack or heap. Morris's classic answer (Fig. 2, in C) threads pointers
through the tree itself; the paper's verdict is "The algorithm is subtle, though.
Since it transforms the tree into an intermediate graph, we need to state
invariants over the so-called Morris loops to prove its correctness."

The FBIP version (Fig. 3) instead defines an explicit `visitor` type — "our
visitor data type can be generically derived as a list of the *derivative* of the
tree data type" — and walks `Up`/`Down`. The payoff sentence: "**each `Bin`
matches up with a `BinR`, each `BinR` with a `BinL`, and finally each `BinL` with
a `Bin`. Since they all have the same size**, if the tree is unique, each branch
updates the tree nodes in-place at runtime without any allocation, where the
visitor structure is effectively overlaid over the tree nodes." All calls are
tail calls, so it is also a loop.

The transferable rule: **make the constructors on each side of a match the same
arity**, and reuse fires. That is a design constraint you can apply in Rust too,
even without the compiler pass — it is why an in-place `Vec` transform beats
collect-into-new when the element sizes match.

### Step 7 — borrow inference is Lean's, and it is worth far less than reuse

> **In:** Beans §5.2's analysis, and Figure 6's ablation columns.
> **Out:** three ratios you compute yourself, which reorder the passes by value.

Beans §5.2 infers, per parameter, whether it is **owned** or **borrowed**. The
algorithm is a fixpoint: start optimistically with every parameter borrowed
(`β(c) = Bⁿ`) and promote a parameter to owned when it is consumed — used in a
`reset`, or passed to a function that takes it owned. The paper states the
trade-off explicitly: "when we mark a parameter as borrowed, we reduce the number
of RC operations needed, but we also **prevent reset and reuse**." Never mark `x`
borrowed if the body contains `let y = reset x`.

That tension is measurable, and this is where the usual story gets the ordering
wrong. Beans **Figure 6** (arithmetic mean of 50 runs via `temci`, on an i7-3770
with 16 GB running Ubuntu 18.04, Clang 9.0.0; each column normalized to the base
run time, `rbmap` for the `rbmap_*` rows):

| benchmark | base | `-reuse` | `-borrow` | `-ST` |
|---|---|---|---|---|
| binarytrees | 1.00 | 0.98 | 1.14 | 1.22 |
| deriv | 1.00 | 1.00 | 1.16 | 1.42 |
| const_fold | 1.00 | 1.64 | **0.90** | 1.23 |
| parser | 1.00 | 1.00 | 1.00 | 1.68 |
| qsort | 1.00 | 1.00 | 1.00 | 1.13 |
| rbmap | 1.00 | **3.23** | 1.07 | 1.71 |
| rbmap_10 | 1.49 | 3.62 | 1.52 | 2.43 |
| rbmap_1 | 4.72 | 5.42 | 4.47 | 8.02 |
| unionfind | 1.00 | 1.41 | 1.00 | 2.31 |
| **geom. mean** | **1.24** | **1.74** | **1.27** | **1.89** |

`-reuse` disables `reset`/`reuse`; `-borrow` assumes all parameters owned; `-ST`
uses atomic RC for all values. The base column is not 1.00 because `rbmap_10` and
`rbmap_1` are normalized to `rbmap`, not to themselves — so **compare columns to
the base column, not to 1**.

**Work the three ratios.**

- reuse: `1.74 / 1.24 = 1.403` → turning reuse off costs **40%** overall, and
  **3.23×** on `rbmap`.
- borrow inference: `1.27 / 1.24 = 1.024` → **2.4%** overall.
- atomic RC: `1.89 / 1.24 = 1.524` → **52%** overall.

Two things fall out. First, **reuse is worth roughly sixteen times what borrow
inference is worth** on this suite, which inverts the order these passes are
usually presented in. Second, on `const_fold` the `-borrow` build is **0.90 —
10% *faster* without borrow inference**, exactly the §5.2 trade-off firing:
marking parameters borrowed suppressed reuse that was worth more than the RC ops
it saved. Read Figure 6's typographic convention before quoting it further:
"Digits whose order of magnitude is no larger than that of twice the standard
deviation are marked by squiggly lines" — and the last digit of the `-borrow`
geometric mean carries one. A 2.4% overall effect on this suite is at the noise
floor.

The paper's own summary is correspondingly narrow: reset/reuse "significantly
improve performance in the benchmarks `const_fold`, `rbmap`, and `unionfind`",
while "the borrowed inference heuristic provides significant speedups in
benchmarks `binarytrees` and `deriv`" — two of nine.

### Step 8 — the atomic tax, and why sharing a value across threads kills reuse

> **In:** an object that might be reachable from another thread.
> **Out:** two measurements of what atomics cost, and the reuse that disappears
> with them.

Lean encodes thread-sharedness in the sign of the count:

```c
// leanprover/lean4@v4.24.0 src/include/lean/lean.h, lines 115-117 and 487-497
   115  The reference counter `m_rc` field also encodes whether the object is single threaded (> 0), multi threaded (< 0), or
   116  reference counting is not needed (== 0). We don't use reference counting for objects stored in compact regions, or
   117  marked as persistent.
   487  static inline void lean_inc_ref_n(lean_object * o, size_t n) {
   488      if (LEAN_LIKELY(lean_is_st(o))) {
   489          o->m_rc += n;
   490      } else if (o->m_rc != 0) {
   492          std::atomic_fetch_sub_explicit(lean_get_rc_mt_addr(o), n, std::memory_order_relaxed);
   496      }
   497  }
```

Line 489 is a **non-atomic** add on the single-threaded fast path — no lock
prefix, no fence, no cache-line ping-pong. Line 492 subtracts rather than adds
because multi-threaded counts are negative (line 115). Beans §7.2 adds that
single-threaded values need **no memory fence at all**, while MT uses a relaxed
fetch-add on `inc` and release/acquire on `dec`.

**Two independent measurements of what you pay to give that up:**

- Beans Figure 6, `-ST` column: `1.89 / 1.24 = 1.524`, a **52%** geometric-mean
  slowdown from using atomic RC for all values, and `2.31×` on `unionfind`.
- Perceus §4, last paragraph: "we also ran our benchmarks using just atomic
  operations for our reference counts to see the impact of the thread-shared
  flag. We observed a slowdown from **5% (rbtree) up to 59% (nqueens)** across our
  benchmarks."

But the second-order effect is larger than either number. `lean_is_exclusive`
(543–549) returns **false for any multi-threaded object**, whatever its count. So
the moment a value becomes thread-shared it does not merely pay atomics — **it
loses every in-place update in Step 5**, and every `map` over it reverts to the
allocating path. That is the exact cost structure of `Arc<T>` in a Rust engine:
`Arc::make_mut` on a value you handed to a background thread copies, forever
after, even when the other reference is long gone.

### Step 9 — garbage-free, and what Perceus deliberately gives up

> **In:** the headline claim.
> **Out:** its precise definition, its one measured counterexample, and the
> feature Perceus refuses in order to keep it.

The abstract: "Perceus emits precise reference counting instructions such that
(cycle-free) programs are **garbage free, where only live references are
retained**." §1 restates it as a theorem obligation: Perceus is proved "both sound
(i.e. never drops a live reference), and garbage free (i.e. only retains
reachable references)". Note the parenthetical in the abstract — **cycle-free**.
Perceus does not collect cycles; §6 lists cycle collection as open.

Three things sharpen the claim into something you can argue with.

**It is not always a memory win.** §4, on `cfold`: "The 'no-opt' version of Koka
also uses **11% less memory**; this is because the reuse analysis essentially
holds on to memory for later reuse. Just like with scoped based reference
counting that may lead to increased memory usage in some situations." Holding a
cell to reuse it *is* retaining a dead object — garbage-freedom is a property of
the emitted `drop`s, not a guarantee that peak RSS is minimal. On `deriv`, OCaml
uses slightly *less* memory than Koka, which the paper attributes to
case-of-case inlining that Koka does not do.

**It is the reason Perceus has no borrow inference.** §6: "We would like to
integrate selective 'borrowing' into Perceus – this would **make certain programs
no longer be garbage free**, but we believe it could deliver further performance
improvements if judiciously applied." A borrowed parameter means the callee holds
a reference it is not accounted for and cannot drop early; that is precisely a
retained-but-dead reference. Step 7's 2.4% is the price Perceus declines to pay
for.

**It is not uniqueness typing.** §5: linear types "like linear Haskell, or the
uniqueness typing of Clean, can offer static guarantees that the corresponding
objects are unique at runtime… However, this usually also requires writing
multiple versions of a function for each case (unique- versus shared argument).
**By contrast, reuse analysis relies on dynamic runtime information**, and thus
reuse can be performed generally. This is also what enables FBIP to use a single
function that can be used for both unique or shared objects (since the uniqueness
property is not part of the type)." One `map`, two behaviours, chosen by a branch
— the trade is a runtime check for not having to write the function twice.

### Step 10 — why this is in a database curriculum

> **In:** an engine written in Rust with `Arc` in the hot path.
> **Out:** three transfers, and the one design rule behind them.

- **The RC == 1 fast path is delta-matrix thinking.** Mutate in place when you
  are the only owner, copy-on-write otherwise — Redis's shared objects,
  FalkorDB's tensor sharing, and `Arc::make_mut` are the same branch.
  `lean_ensure_exclusive_array` (`lean.h:863-866`) is that branch as a runtime
  primitive rather than a library call.
- **Borrowed parameters are the zero-cost read path.** An executor passing
  `&Value` down a pipeline (topic 11) is doing by hand what Beans §5.2 infers —
  and Step 7 says exactly how much that is worth on a suite of symbolic
  workloads, which is less than you would guess.
- **Thread-sharing is the cliff, not the atomics.** Step 8: crossing into
  multi-threaded ownership costs 52% in counting *and* disables in-place update
  entirely. An engine that wraps everything in `Arc` "just in case" has paid both
  halves before measuring either.

The transferable design rule: **ownership information precise enough to act on
turns "immutable" and "in-place" from opposites into a runtime branch** — and the
measured lesson of Step 7 is that the branch (reuse) is worth far more than the
static analysis that avoids counting (borrowing).

## M21 taste: the proof-vs-test trade-off

Property (topic 20): the delta-matrix invariant `DP ∩ M = ∅ ∧ DM ⊆ M`, preserved
by `set`/`remove`/`wait`.

- **proptest** (topic 16): minutes to write, samples the space, finds shallow
  counterexamples fast, says nothing about the cases it did not draw.
- **TLC**: model `M`/`DP`/`DM` as small sets, exhaustive at `n = 4` — see
  [reading-tlaplus-raft.md](reading-tlaplus-raft.md) for how fast that state
  space grows and why 4 is where you stop.
- **Lean**: `theorem set_preserves_inv : inv m → inv (set m i j)` — unbounded, no
  `MaxLog = 3`, and you will spend a day on set-theory lemmas.

Do it once, in all three, to calibrate which properties deserve which tool. The
answer is usually: proptest for everything, TLC for concurrency, Lean for the one
invariant the whole design rests on.

## How to read the papers (with the concepts in hand)

**Read Beans (IFL 2019) first** — it is Lean 4's runtime and the shorter paper.

- §2–4 for the IR and the `reset`/`reuse` instructions (Step 5).
- **§5.2 borrow inference** — the fixpoint and the "prevents reset and reuse"
  sentence (Step 7).
- §7.1 value representation and §7.2 the ST/MT/persistent tags (Steps 1, 8).
- **§8 and Figure 6** — read the ablation columns *before* the cross-language
  comparison in Figure 7, and compute the three ratios of Step 7 yourself. Also
  in §8: on `const_fold` Lean spends **17%** of runtime deallocating where OCaml
  spends **90%** in GC, and Lean is **5× as fast as OCaml** on that benchmark.

**Then Perceus (PLDI 2021 / MSR-TR-2020-42).**

- **§2.2–2.6 are the payload** and are written as a worked example on `map` —
  Figure 1a–1g is the whole algorithm as a sequence of program transformations.
  Read it with Steps 3–6 open.
- §3's linear resource calculus `λ₁` and §4's soundness/garbage-free theorems are
  skimmable on a first pass. Note that "borrowing" in §3 is a *typing
  environment*, not Beans' calling convention — do not conflate them.
- **§4 Benchmarks**: read the system list first (Koka 2.0.3 + gcc 9.3.0 +
  customized mimalloc; OCaml 4.08.1; GHC 8.6.5; Swift 5.3; Java SE 15.0.1 with
  G1; C++ gcc 9.3.0 + libc allocator), then the caveat the authors put in front of
  it — "we view these results therefore mostly as evidence that the Perceus
  reference counting technique is viable … **not as a direct comparison of
  absolute performance between systems**". Then the per-benchmark paragraphs,
  which is where every number lives: `rbtree` is 42M insertions and Koka lands
  "within 10% of the C++ performance" using `std::map`, while Java is close on
  time but uses "almost 10× the memory of Koka (1.7 GiB vs. 170 MiB)" — check
  that ratio: `1.7 × 1024 / 170 = 10.24`.
- **§5 Related Work and §6 Conclusion** — the attribution to Lean and the
  borrowing-versus-garbage-free admission (Step 9). Do not skip them; they are
  where the framing in the first half of this guide comes from.

Then read `src/include/lean/lean.h` at `v4.24.0` in this order: 112–136 (Step 1),
138–160 (Step 2), 466–511 (Step 8), 543–561 and 863–874 (Step 5).

## Questions (answer in notes.md)

1. Recompute Step 7's three ratios from Figure 6 and rank the passes by measured
   value. Then explain `const_fold`'s `-borrow` entry of **0.90** using the
   sentence from Beans §5.2 about what borrowing prevents.
2. Redo Step 1's arithmetic for a tree rather than a list: Lean's `Node(color,
   left, key, value, right)`. How many bytes per node at `lean.h:131-136`'s
   layout, and what does a 1M-node `tmap` allocate with reuse and without?
3. Where exactly does `Arc<T>` in a Rust engine pay what Beans-style borrow
   inference removes, and where does it pay the *larger* cost from Step 8? Be
   specific about which one `Arc::clone` in a hot loop is.
4. Reuse tokens need an `is-unique` check per constructor. Name a data structure
   where that branch is pure loss, and say why — then check your answer against
   the `deriv` paragraph in Perceus §4.
5. "Garbage free" and `cfold` using 11% *more* memory than "no-opt" are both true.
   Reconcile them in two sentences, and say what that means for a
   memory-budgeted buffer pool (topic 6).
6. Rank Lean, TLC and proptest for `DP ∩ M = ∅` by (cost to write, strength of
   guarantee, maintenance cost under refactor). Which column decides it for a
   codebase that changes weekly?

## Done when

Answer each before unfolding it.

- [ ] You can say which paper belongs to which language, and what Perceus does *not* have.

  <details><summary>Answer</summary>

  **Beans (IFL 2019, Ullrich & de Moura) is Lean 4's** runtime: ownership-based
  RC, **borrow inference**, `reset`/`reuse`. **Perceus (PLDI 2021, Reinking, Xie,
  de Moura, Leijen) is Koka's**, and its §5 says it is "closely based on the
  reference counting algorithm in the Lean theorem prover", extending it with
  drop- and reuse specialization plus the `λ₁` formalization.

  **Perceus has no borrow inference.** §6: integrating "selective 'borrowing'"
  is future work and "would make certain programs no longer be garbage free".
  The "borrowing" that does appear in Perceus §3 is a typing environment in the
  linear resource calculus, not a calling convention.

  </details>

- [ ] You can compute what `map` costs at each stage of the pipeline, in bytes.

  <details><summary>Answer</summary>

  A Lean `List.cons` at `v4.24.0` is **24 bytes**: an 8-byte header
  (`lean.h:131-136` — `int` + 16 + 8 + 8 bits) plus two pointers
  (`lean_ctor_object`, `lean.h:170-173`). For `N = 1,000,000`:

  - literal copying / scoped RC — both lists live at peak: `2 × 24 MB = 48 MB`;
  - precise ownership RC (Perceus §2.2, "for `map`, the memory usage is
    **halved**") — the input dies cell by cell: **24 MB**;
  - plus reuse analysis (§2.4) — the freed cell becomes the token for the next
    `Cons`: still 24 MB live, but **zero allocations**.

  The Beans paper (§7.1) says 32 bytes per `Cons` (16-byte header); that is the
  2019 layout, and the runtime has since tightened it. Cite the paper for 2019
  and `lean.h` for now.

  </details>

- [ ] You can explain the reuse token and reuse specialization, and quote the red-black arithmetic.

  <details><summary>Answer</summary>

  Reuse analysis (§2.4) pairs each matched pattern with a constructor **of the
  same size** allocated in the same branch, replaces `drop` with `drop-reuse`
  returning a token `ru`, and attaches it: `Cons@ru(f(x), map(xx,f))`. That
  compiles to `if (ru != NULL) then { ru->head := x; ru->tail := xx; ru } else
  Cons(x,xx)`.

  Reuse specialization (§2.5) applies "only … if at least one of the fields stays
  the same", so a red-black rebalance becomes `if (ru!=NULL) then { ru->left :=
  y; ru }` — one store, not five.

  The arithmetic: after inlining `bal-left`, "we either match one `Node` and
  allocate one, or we match three nodes deep and allocate three… every `Node` is
  reused in the fast path **without doing any allocations**." Over the `rbtree`
  benchmark's 42M insertions, a 3-node rebalance path is `1.26 × 10⁸`
  allocate/free pairs removed; measured, the "no-opt" build is "more than 2×
  slower".

  </details>

- [ ] You can state, with numbers, why reuse matters far more than borrow inference.

  <details><summary>Answer</summary>

  From Beans **Figure 6** geometric means (base column **1.24**, because
  `rbmap_10`/`rbmap_1` are normalized to `rbmap`):

  - `-reuse` `1.74 / 1.24 = 1.403` → **40%**, and **3.23×** on `rbmap`;
  - `-borrow` `1.27 / 1.24 = 1.024` → **2.4%**;
  - `-ST` `1.89 / 1.24 = 1.524` → **52%**.

  Reuse is worth about **16× what borrow inference is worth** on this suite. On
  `const_fold`, `-borrow` is **0.90** — 10% *faster* without it — because
  Beans §5.2's trade-off fires: "when we mark a parameter as borrowed, we reduce
  the number of RC operations needed, but we also **prevent reset and reuse**".
  And Figure 6's caption marks digits within twice the standard deviation with a
  squiggle; the `-borrow` geomean's last digit carries one, so 2.4% is at the
  noise floor.

  </details>

- [ ] You can explain the atomic-RC cost and the larger second-order cost of thread sharing.

  <details><summary>Answer</summary>

  Lean encodes thread-sharedness in the *sign* of `m_rc` (`lean.h:115-117`), so
  `lean_inc_ref_n` (`:487-497`) takes a **non-atomic** `o->m_rc += n` on the
  single-threaded path and a relaxed `atomic_fetch_sub_explicit` otherwise
  (subtract, because MT counts are negative). Beans §7.2: no memory fence at all
  for ST values.

  Measured cost of losing that: Beans Figure 6 `-ST` is `1.89/1.24 = 1.52`
  (**52%** geomean, `2.31×` on `unionfind`); Perceus §4 independently reports
  "a slowdown from **5% (rbtree) up to 59% (nqueens)**" from using atomics
  everywhere.

  The bigger cost is not the atomics. `lean_is_exclusive` (`:543-549`) returns
  **false for any MT object regardless of its count**, so a thread-shared value
  loses *every* in-place update. `Arc<T>` in a Rust engine pays exactly this: once
  a value has been handed to another thread, `Arc::make_mut` copies forever after.

  </details>

- [ ] You can state "garbage free" precisely and name its one measured counterexample.

  <details><summary>Answer</summary>

  Perceus emits precise RC instructions such that **(cycle-free)** programs are
  garbage free — "only retains reachable references" (§1); proved sound (never
  drops a live reference) and garbage free. Cycles are not collected; §6 lists
  cycle collection as open.

  The counterexample is `cfold` in §4: the **"no-opt" build uses 11% *less*
  memory**, "because the reuse analysis essentially holds on to memory for later
  reuse. Just like with scoped based reference counting that may lead to
  increased memory usage in some situations." Holding a cell for reuse *is*
  retaining a dead object, so garbage-freedom is a property of where the `drop`s
  are emitted, not a promise about peak RSS. (On `deriv`, OCaml also uses
  slightly less memory than Koka, which §4 attributes to case-of-case inlining.)

  </details>

- [ ] You can explain why reuse analysis is not uniqueness typing, and what that buys.

  <details><summary>Answer</summary>

  Perceus §5: linear types "like linear Haskell, or the uniqueness typing of
  Clean, can offer static guarantees that the corresponding objects are unique at
  runtime… However, this usually also requires **writing multiple versions of a
  function** for each case (unique- versus shared argument). By contrast, reuse
  analysis relies on **dynamic runtime information**… This is also what enables
  FBIP to use a single function that can be used for both unique or shared
  objects (since the uniqueness property is **not part of the type**)."

  So the trade is a runtime `is-unique` branch in exchange for writing `map`
  once. The same value used persistently degrades gracefully: §2.5 notes the
  red-black algorithm "adapts to copying exactly the shared spine of the tree
  (and no more), while still rebalancing in place for any unshared parts".

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the recomputed Figure 6 ratios and the tree-node byte count.

  <details><summary>Answer</summary>

  The shape to check yours against for question 2: `Node(color, left, key, value,
  right)` in Lean is an 8-byte header plus five fields; with all five boxed that
  is `8 + 5×8 = 48` bytes, and Lean can unbox the scalars (`m_other` holds "the
  number of fields in a constructor object", `lean.h:129`), so a tuned layout is
  smaller. A 1M-node `tmap` allocates `~48 MB` without reuse and **0 bytes** with
  it, because §2.6's `Bin`/`BinR`/`BinL` all have the same size — which is the
  precondition, not a coincidence.

  </details>

## References

**Papers**

- Sebastian Ullrich, Leonardo de Moura — *Counting Immutable Beans: Reference
  Counting Optimized for Purely Functional Programming*, IFL 2019
  ([arXiv:1908.05647](https://arxiv.org/abs/1908.05647)). **Lean 4's runtime.**
  §5.2 borrow inference; §7.1 value layout (16-byte ctor header, 32-byte `Cons`
  in 2019); §7.2 the ST/MT/persistent tags; §8 and **Figure 6** the ablation
  table of Step 7 — i7-3770, 16 GB, Ubuntu 18.04, Clang 9.0.0, arithmetic mean of
  50 runs via `temci`.
- Alex Reinking, Ningning Xie, Leonardo de Moura, Daan Leijen — *Perceus: Garbage
  Free Reference Counting with Reuse*, PLDI 2021 (MSR-TR-2020-42, Nov 22 2020).
  **Koka's runtime.** §2.2 precise RC and the halved `map`; §2.3 drop
  specialization; §2.4 reuse analysis; §2.5 reuse specialization and the
  red-black arithmetic; §2.6 FBIP and Morris; §4 benchmarks (Figure 9, median of
  10 runs normalized to Koka) and the atomic-RC experiment; §5 the attribution to
  Lean and the uniqueness-typing comparison; §6 the borrowing-versus-garbage-free
  admission.
- Joseph M. Morris — *Traversing binary trees simply and cheaply*, IPL 1979 — the
  algorithm Perceus Figure 2 shows in C and Figure 3 replaces.
- Chris Okasaki — *Purely Functional Data Structures* — the red-black insertion
  §2.5 uses as its reuse-specialization example.

**Code** — `leanprover/lean4` at `v4.24.0` (no pin-table entry; fetch with
`tools/pinned-source.py --ref v4.24.0`)

| Anchor | What |
|---|---|
| `src/include/lean/lean.h:112-136` | the object header; `m_rc`'s sign encodes ST (> 0) / MT (< 0) / no-RC (== 0); 8 bytes on 64-bit |
| `src/include/lean/lean.h:138-160` | the "standard" vs "borrowed" calling conventions, and the RC == 1 licence at 145–146 |
| `src/include/lean/lean.h:170-173` | `lean_ctor_object` — header plus a flexible array of fields |
| `src/include/lean/lean.h:466-481` | `lean_is_mt`, `lean_is_st`, `lean_is_persistent`, `lean_has_rc` |
| `src/include/lean/lean.h:487-511` | `lean_inc_ref_n` (non-atomic ST fast path) and `lean_dec_ref` |
| `src/include/lean/lean.h:543-561` | `lean_is_exclusive` — false for MT objects at any count — and `lean_is_shared` |
| `src/include/lean/lean.h:863-874` | `lean_ensure_exclusive_array` (= `Arc::make_mut`) and `lean_array_uset` |

**In this topic**
- [reading-tlaplus-raft.md](reading-tlaplus-raft.md) — the model-checking side of
  the proof-vs-test trade-off in the M21 taste.
- `topics/21-formal/README.md` §5 — where Lean sits on this topic's cost ladder.
