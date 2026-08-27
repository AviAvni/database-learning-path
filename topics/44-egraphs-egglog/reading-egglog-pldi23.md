# egglog: a Datalog engine that happens to be an e-graph

The previous chapter ends on an unfinished sentence. Relational
e-matching turns the e-graph into a database *whenever you want to
match*, and POPL'22 §6.4 flags the obvious cost: the translation is
rebuilt from scratch, which is only affordable because matching happens
in big batches between rebuilds. **"Better Together: Unifying Datalog
and Equality Saturation"** — Zhang, Wang, Flatt, Cao, Zucker, Rosenthal,
Tatlock and Willsey, PLDI 2023 (arXiv:2304.04332) — is what happens when
you stop translating and make the database primary.

The claim is bigger than a performance one. egglog is a **Datalog
engine with two extensions** — user-extensible equality, and functions
with a `:merge` expression — and those two extensions are enough to make
equality saturation a special case. Congruence closure stops being a
built-in algorithm and becomes what a particular `:merge` does. Once
that is true, everything Datalog knows — semi-naive evaluation,
lattices, stratification, query optimisation — applies to e-graphs for
free.

This chapter builds the Datalog vocabulary from scratch, works
`:merge` and semi-naive evaluation on concrete numbers, and closes on
what the paper measured and what it gave up. Read
[reading-relational-ematching.md](reading-relational-ematching.md)
first; this one assumes its Steps 5–7 (atoms, bodies, join variables).

## The problem in one sentence

Equality saturation needed the things Datalog has (incremental
evaluation, lattice-valued analyses, a query optimiser) and Datalog
needed the thing equality saturation has (a fast, built-in equivalence
relation) — and each community had been building bad versions of the
other's feature until someone noticed that a **function whose merge
operation is `union` is exactly congruence closure**.

## The concepts, step by step

### Step 1 — Datalog, in the amount this chapter needs

> **In:** nothing. **Out:** the six words — fact, rule, body, head,
> immediate consequence, fixpoint — that Steps 4 and 7 restate for
> egglog.

A **Datalog program** is a set of relations and a set of rules. A
**fact** is a tuple asserted directly; a **rule** has a head and a body,

```
   TC(x, y) :- TC(x, z), E(z, y).
```

read as: *whenever* the body's atoms can all be matched by one
assignment of the variables, add the head to the database. Body
matching is exactly the conjunctive query of the previous chapter.

Evaluation applies the **immediate consequence operator** `T_P` — fire
every rule once against the current database, collect all the heads —
and repeats until nothing new appears. That is the **fixpoint**, and it
exists because ordinary Datalog is **monotone**: rules only ever add
tuples, so the database grows and, being finite, must stop.

The paper's Figure 1 runs the classic example, and the trace is worth
copying because Step 7 measures against it:

```
   E(1,2). E(2,3). E(3,4).           iter   E              TC
   TC(x,y) :- E(x,y).                  0    ∅              ∅
   TC(x,y) :- TC(x,z), E(z,y).         1    {(1,2),(2,3),(3,4)}   ∅
                                       2    …              {(1,2),(2,3),(3,4)}
                                       3    …              … (1,3),(2,4)
                                       4    …              … (1,4)
```

Note iteration 3. It re-derives (1,2), (2,3) and (3,4) — every tuple
found in iteration 2 — because the rule body is checked against the
whole database again. That waste is Step 7's subject.

### Step 2 — what each side was missing

> **In:** Step 1's Datalog, and equality saturation from topic 21.
> **Out:** the two concrete failures the paper opens with, which are the
> reason the unification is not just elegant.

Paper §1 names one on each side, and both are real systems:

- **Herbie** (a floating-point accuracy optimiser) uses equality
  saturation with rules that are *unsound*: `x/x → 1` is only valid for
  `x ≠ 0`, and equality saturation has no good way to express the
  condition. So Herbie runs with the unsound rules and then validates
  and discards results afterwards — and cannot run saturation for
  longer, because more iterations means more unsoundness.
- **cclyzer++** (an LLVM points-to analysis in Datalog) needed
  Steensgaard-style unification — a union-find — and found Datalog's
  built-in equivalence relations too slow, so it wrote "an ad-hoc
  implementation of union-find", whose complexity "led to bugs in the
  pointer analysis".

One system wants Datalog's analyses inside its e-graph; the other wants
an e-graph's union-find inside its Datalog. §1: "EqSat struggles to
support rich analyses, and equational reasoning in Datalog is complex
and slow."

### Step 3 — functions, not relations

> **In:** Step 1's notion of a relation. **Out:** egglog's storage
> model, and the constraint that makes `:merge` necessary.

egglog stores data as **partial functions**, not relations (§3.2).
Every user-defined function is backed by a **map** rather than a set,
and a relation is sugar: an n-ary relation `R` is a function to the
built-in `unit` type, defined exactly where the tuple is present.

The map enforces something a set cannot: a **functional dependency**
from the argument columns to the output column. `f(v₁ … v_k)` has at
most one output value, always. In relational terms, egglog's tables all
have a declared key — which is also what an e-graph's hashcons
guarantees (previous chapter, Step 6), now stated as a schema property
rather than an implementation trick.

The paper uses "table" for both the map behind a function and the set
behind a relation, and so will this chapter.

### Step 4 — `:merge`, worked on shortest paths

> **In:** the functional dependency of Step 3. **Out:** what happens
> when a rule tries to violate it, and why the answer is a lattice.

If `path(1,3) ↦ 30` is already in the table and a rule fires with
`(set (path 1 3) 20)`, the functional dependency is about to break. A
`:merge` expression says how to resolve it. Paper Figure 3b:

```lisp
;; paper Figure 3b, lines 1-2 and 6-7 — reachability with path length
 1  (function edge (i64 i64) i64)
 2  (function path (i64 i64) i64 :merge (min old new))
 6  (rule ((= (path x y) xy) (= (edge y z) yz))
 7        ((set (path x z) (+ xy yz))))
```

With `(set (edge 1 2) 10)`, `(set (edge 2 3) 10)`, `(set (edge 1 3) 30)`:

```
   the direct edge is found first     path(1,3) ↦ 30
   the two-hop rule fires             set(path(1,3), 10 + 10)  = 20
   conflict, so evaluate :merge       (min old new) = (min 30 20) = 20
   result                             path(1,3) ↦ 20            ✓ paper prints 20
```

Some vocabulary, because the paper's next sentence uses it. A
**partial order** `⊑` is a reflexive, antisymmetric, transitive
relation. A **lattice** over a domain adds a **join** `⊔`: the least
element that is above both arguments (their *supremum*). `min` looks
like the wrong direction until you read the paper's own definition
(§3.2): it is the join of the **min lattice**, where `x ⊑ y ⟺ x ≥ y`.
Order the values by *worseness* and taking the minimum is climbing.

This is the same construction as Flix's lattice semantics and as egg's
e-class analyses (topic 21, Step 9), but egglog does not require the
`:merge` expression to be a lattice join at all — it can be any egglog
expression. That freedom is what Step 6 needs.

### Step 5 — sorts, ids, and get-or-make-set

> **In:** Step 3's functions. **Out:** egglog's equality, and the one
> operation that turns a function call into an e-node.

A **sort** declared by the user is "a set of opaque integer values
called ids and an equivalence relation over those ids" (§3.3),
implemented by a union-find. Two ids are equal iff they canonicalise to
the same id, and **egglog keeps every id in the database canonical**.
Those ids are e-class ids; the paper says so.

`union` is an action that merges two ids of a user-defined sort. Values
of *base* types (`i64`, `String`) cannot be unioned — they are only
equal to themselves — which is what keeps the constant `2` from
accidentally becoming the constant `3`.

Then the small mechanism that carries the most weight. A function may
declare a `:default`, and calling `(f x)` is a lookup that falls back to
it: "Calling a function `(f x)` will first see if the map for function
`f` defines an output for `x`. If so, it returns that output. Otherwise,
egglog evaluates the `:default` expression, stores the result in the
map, and returns it" (§3.3). For a function returning a user-defined
sort the default default is **make-set**: mint a fresh union-find id.

So calling a constructor is a **get-or-make-set**. Read that next to
topic 21's `EGraph::add`, which is a hashcons lookup that inserts a new
e-class on a miss. They are the same operation, arrived at from
opposite directions.

### Step 6 — congruence, as a consequence rather than an algorithm

> **In:** Steps 3–5: the functional dependency, `:merge`, and unionable
> ids. **Out:** the paper's central identification, worked on a
> two-entry table.

Constructors of a `datatype` get `:merge` = `union` (§3.4). Watch what
that alone produces. Take `Add`'s table with two entries:

```
   Add: (a, b) ↦ c
        (a, d) ↦ e                    with b ≠ d, c ≠ e
```

Now a rule unions `b` and `d`, and `b` becomes canonical. egglog
canonicalises the database, so the second row is rewritten:

```
   Add: (a, b) ↦ c
        (a, b) ↦ e                    ← the functional dependency is violated
```

The conflict invokes `Add`'s `:merge`, which is `union`, so `c` and `e`
are unioned — and that is precisely the congruence axiom:
`b ≡ d ⟹ Add(a,b) ≡ Add(a,d)`. Unioning `c` and `e` may in turn break
another table's dependency, so the process repeats to fixpoint.

**Congruence closure is not implemented in egglog. It is what
maintaining a functional dependency under a union-find does.** Compare
topic 21: egg's `rebuild` is a hand-written worklist algorithm with the
congruence invariant baked in. Here it is a schema constraint plus a
merge policy, and `min` in the same slot gives you shortest paths
instead.

The paper's formal version (§4.2) is two operators applied
alternately: the inflationary immediate consequence operator `T_P↑`,
which fires the rules and may produce a **pre-instance** (a database
whose functional dependencies are broken), and the **rebuilding
operator** `R`, whose `≡_R` is the equivalence closure of the current
equality plus every pair `(n₁, n₂)` such that some `f(v₁…v_k)` maps to
both. `R^∞` is that run to fixpoint.

One footnote worth stopping on (§4.2, footnote 4): egglog's consequence
operator has to union with the old database explicitly because, unlike
standard Datalog, **egglog rules are not always monotone**. The example
given is a rule reading a lower-bound analysis, `Q(e) :- lo(e) ↦ 5` —
`lo(e)` can *increase* over time, so a fact derivable now may not be
derivable later. Monotonicity was the reason Datalog terminates; egglog
keeps termination by making the database inflationary by construction
instead.

### Step 7 — semi-naive evaluation, and the duplicates it creates

> **In:** Step 1's re-derivation waste and Step 6's two operators.
> **Out:** the delta-rule expansion, worked on this topic's lane 2 —
> including the exact number of duplicate derivations it produces.

Naive evaluation re-derives everything every iteration (Step 1's trace,
iteration 3). **Semi-naive** evaluation (§4.3) keeps a differential
database `ΔDB_i` of the tuples that are new or updated this iteration,
and expands each rule into one **delta rule** per body atom:

```
   A :- A₁ … A_m        ⇒        A :- A₁ … A_{j-1}, ΔA_j, A_{j+1} … A_m
                                 for each j ∈ 1…m
```

The j-th delta rule ranges atom j over the new tuples and every other
atom over the whole database. Their union is exactly the derivations
that use at least one new tuple — and Theorem 4.1 says the semi-naive
evaluation of an egglog program produces the same database as the naive
one, which is the property you actually need.

Work it on lane 2 of this topic's bench. The query has m = 2 atoms:

```
   Q(root, a) ← R_f(root, a, x), R_g(x, a)
```

The e-graph has 20,000 constants (60,000 tuples); then 8 constants
arrive, contributing 8 new `R_f` tuples, 8 new `R_g` tuples and 8 new
constant tuples — 24, which is what the lane prints. The two delta
rules:

```
   j = 1:  ΔR_f(root, a, x), R_g(x, a)     8 new f-tuples ⋈ full R_g   →  8 matches
   j = 2:  R_f(root, a, x), ΔR_g(x, a)     full R_f ⋈ 8 new g-tuples   →  8 matches
                                                                   union → 16
                                                                   dedup →  8
```

**Every one of the 8 answers is derived twice**, because each involves
one new `f` tuple *and* one new `g` tuple, so both delta rules find it.
That is not a bug in the expansion; it is inherent to "at least one
atom is new", and it is why `semi_naive::delta_matches` must
deduplicate. Set that against the naive column the lane prints today —
**20,008 matches, 100,040 probes, 11.0 ms** — and you have the whole
argument in two rows.

The general shape: semi-naive replaces one query over the full database
with m queries, each with one small atom. It wins when the delta is
small relative to the database, which in a saturation loop is true from
about iteration three onwards, and it loses when the delta is most of
the database, which is true on iteration one.

### Step 8 — what it measured

> **In:** Steps 6–7. **Out:** the paper's numbers, and which of them is
> attributable to which idea.

§5.3, the microbenchmark, is designed to separate the two contributions.
Three systems on egg's `math` suite, populated with the same initial
terms, run with egg's default BackOff scheduler for 100 iterations,
median of seven runs, on an M2 with 16 GB (footnote 8):

| system | what it isolates | result at iteration 100 |
|---|---|---|
| `egg` | the baseline | — |
| `egglogNI` | relational matching + query optimiser, **no** semi-naive | grows *the same e-graph* **3.34×** faster |
| `egglog` | plus semi-naive | **9.27×** faster, and a slightly larger e-graph |

The `egglogNI` row is the one that matters for attribution: it produces
the identical e-graph, so 3.34× is purely better joins — the previous
chapter's contribution, engineered. The extra step to 9.27× is
semi-naive evaluation, and egglog explores *more* in that time, which is
why the paper is careful to say "slightly larger e-graph" rather than
claiming a clean speedup.

The case studies (§6):

- **Points-to analysis** (§6.1): a Steensgaard-style unification-based
  analysis, where Datalog's weakness was equality. egglog is **4.96×**
  faster than `patched` (the fastest *sound* Soufflé encoding
  available), **1.94×** faster than cclyzer++, and **1.59×** faster than
  egglogNI.
- **Herbie** (§6.2): egglog's analyses let the unsound rewrites be
  guarded, so Herbie can saturate longer. The honest summary is in the
  paper's own count: in **104** benchmarks the sound analysis finds a
  *more* accurate program than the unsound ruleset, and in **135** the
  unsound ruleset still wins. Soundness bought the ability to run
  longer, not uniformly better answers.

### Step 9 — what egglog gives up

> **In:** everything above. **Out:** the honest boundary, so the choice
> between egg and egglog is a choice rather than a fashion.

- **It is a language, not a library.** §5.2 argues this is a feature —
  the compiler sees the guards, rules are typechecked, and a program can
  declare many sorts and functions instead of egg's "single, ad-hoc
  datatype". The cost is that a host-language escape hatch (an arbitrary
  Rust closure in a conditional rewrite) is no longer free.
- **The core semantics is a subset.** §4 is defined over *core egglog*,
  which has one atom in the head, no `union` action, and `:merge`
  restricted to union-on-ids and lattice-join-on-constants. Full egglog
  allows any expression there, and the theorems are not stated for it.
- **Non-monotone rules are allowed** (Step 6's footnote), which means
  the reassuring Datalog story — "monotone, therefore a least fixpoint,
  therefore order does not matter" — does not transfer wholesale.
- **The paper's own scale.** §5: "approximately 4,200 lines of Rust".
  The implementation guide in this topic reads a codebase that has since
  grown a separate `core-relations` crate with its own query planner;
  the 2023 numbers were measured on the smaller thing.

## How to read the paper (with the concepts in hand)

1. **§1** — the two failing systems. It is the motivation and it is
   concrete; do not skip it for the abstract.
2. **§2** is background; if Step 1 landed, skim it.
3. **§3** is the language tour and is best read at a terminal with
   egglog installed, one figure at a time. Figures 3a → 3b → 4a → 4b is
   a deliberate escalation: Datalog, then lattices, then unification,
   then equality saturation, each one figure apart.
4. **§4.2** — read the definition of `R`, the rebuilding operator, next
   to Step 6's worked table, and read footnote 4 rather than skipping it.
5. **§4.3** is one page and is Step 7.
6. **§5.1** for the implementation's shape, **§5.3** for the numbers
   with Step 8's attribution table in view.
7. **§6.1** if you care about program analysis, **§6.2** if you care
   about what "sound" costs in practice.

## Where each step lives in the code

Anchors are `egraphs-good/egglog` at the pinned commit; the next chapter
reads them properly.

| step | where |
|---|---|
| 3, functions as maps | `core-relations/src/table/mod.rs:1-5` — a general table; "timestamp" and "merge function" live above it |
| 5, ids and canonicalisation | `union-find/src/lib.rs:1-12` — and note it is union **by min id**, not by rank |
| 6, rebuilding | `egglog-bridge/src/lib.rs:722` `fn rebuild` |
| 7, semi-naive | `core-relations/src/query.rs:252-256` — one cached plan, re-added each iteration with a `GeConst` timestamp constraint |
| 7, in this crate | `semi_naive.rs` (the stub), lane 2 of `bin/ematch_bench.rs` |

## Questions (answer in notes.md)

1. Write the `:merge` expression that makes a function behave like egg's
   *interval* e-class analysis (each e-class carries `[lo, hi]`), and
   say what lattice it is the join of. What goes wrong if you use `min`
   on the lower bound and `min` on the upper?
2. Step 6 shows congruence emerging from `:merge = union`. Write the
   converse: an egglog function whose `:merge` is *not* a lattice join
   and not `union`, and describe a database on which the result depends
   on the order the conflicts are resolved in.
3. Lane 2's delta produces 16 raw derivations for 8 answers. For a rule
   with m atoms where the delta touches all of them, how many times is a
   single answer derived, and what does that imply about semi-naive's
   overhead as rules get wider?
4. §5.3 attributes 3.34× to better joins and the rest of 9.27× to
   semi-naive. Design the experiment that would separate the query
   *optimiser*'s contribution from the *generic join algorithm*'s. What
   would you have to hold fixed?
5. Footnote 4 says egglog rules can be non-monotone. Construct a
   two-rule egglog program whose final database depends on the order the
   rules fire in, and say which of Datalog's guarantees you have lost.
6. egg needs a separate e-class *analysis* mechanism for facts like
   constant folding; egglog uses ordinary rules. Name one thing an
   analysis can express that a rule cannot, and one the other way round.

## Done when

Answer each before unfolding it.

- [ ] You can explain why an egglog function needs a `:merge` and a
      Datalog relation does not.
  <details><summary>Answer</summary>

  A relation is backed by a set: adding a tuple that is already there is
  a no-op, and there is nothing to reconcile. An egglog function is
  backed by a **map**, which enforces a functional dependency from the
  argument columns to the output (§3.2), so two derivations that agree
  on the arguments and disagree on the output are a violation, not a
  duplicate. `:merge` is the policy for that case — `(min old new)` for
  shortest paths, `union` for a constructor.
  </details>

- [ ] You can walk the shortest-path example and say why `min` is a
      *join* and not a meet.
  <details><summary>Answer</summary>

  `path(1,3)` gets 30 from the direct edge, then the two-hop rule fires
  with 10 + 10 = 20; `(min 30 20) = 20` and the program prints 20. It is
  a join because the lattice is ordered by worseness: §3.2 defines
  `x ⊑ y ⟺ x ≥ y`, so the *supremum* of {30, 20} under that order is
  the numerically smaller 20. Same operator, inverted order — the sign
  error to watch for whenever a paper calls `min` a join.
  </details>

- [ ] You can derive congruence closure from `:merge = union` on a
      two-row table.
  <details><summary>Answer</summary>

  `Add: (a,b) ↦ c, (a,d) ↦ e`. Union `b` and `d`, with `b` canonical.
  egglog canonicalises the database, so both rows become `(a,b)`, which
  breaks `Add`'s functional dependency. The conflict invokes `:merge`,
  which for a constructor is `union`, so `c ≡ e` — the congruence axiom
  `b ≡ d ⟹ Add(a,b) ≡ Add(a,d)`. Unioning `c` and `e` may break another
  table, so it repeats to fixpoint (§3.4, §4.2's `R`). Congruence is not
  a subroutine here; it is what dependency maintenance does.
  </details>

- [ ] You can expand a two-atom rule into its delta rules and predict
      how many duplicates lane 2 produces.
  <details><summary>Answer</summary>

  `Q(root,a) ← R_f(root,a,x), R_g(x,a)` expands to
  `ΔR_f, R_g` and `R_f, ΔR_g`. Each of the 8 new answers involves one
  new `f` tuple and one new `g` tuple, so both delta rules find all 8:
  16 raw derivations, deduplicated to 8. Against naive's 20,008 matches
  and 100,040 probes for the same 8 answers. The duplication is inherent
  to "at least one atom is new" and is why `delta_matches` must dedup.
  </details>

- [ ] You can state the 3.34× and 9.27× correctly, including what each
      is measured against.
  <details><summary>Answer</summary>

  §5.3, `math` suite, 100 iterations, median of seven, M2/16 GB.
  `egglogNI` — egglog with semi-naive **disabled** — grows *the same
  e-graph* as egg and is **3.34×** faster at iteration 100, so that
  number is attributable to relational matching and query planning
  alone. Full `egglog` is **9.27×** faster and explores a *slightly
  larger* e-graph, so the increment is semi-naive evaluation and is not
  a like-for-like ratio. Both are against egg, not against a naive
  matcher.
  </details>

- [ ] You can say what egglog gives up relative to egg.
  <details><summary>Answer</summary>

  Host-language escape hatches (guards are egglog expressions, not Rust
  closures — §5.2 argues this is worth it); the formal semantics covers
  only *core* egglog, with a single head atom, no `union` action and
  `:merge` restricted to union-on-ids or a lattice join (§4); and
  Datalog's monotonicity guarantee, since egglog rules can be
  non-monotone (§4.2 footnote 4), which is why the consequence operator
  is inflationary by construction.
  </details>

## References

- Yihong Zhang, Yisu Remy Wang, Oliver Flatt, David Cao, Philip Zucker,
  Eli Rosenthal, Zachary Tatlock, Max Willsey, **"Better Together:
  Unifying Datalog and Equality Saturation"**, PLDI 2023,
  arXiv:2304.04332. §1 (Herbie and cclyzer++), §3.2 (`:merge`, the min
  lattice), §3.3 (sorts, `:default`, get-or-make-set), §3.4 (congruence
  from `:merge = union`), §4.2 (`T_P↑`, `R`, footnote 4 on
  monotonicity), §4.3 + Algorithm 1 + Theorem 4.1 (semi-naive), §5.1–5.3
  (implementation and microbenchmark), §6.1–6.2 (case studies).
- Isaac Balbin, Kotagiri Ramamohanarao, **"A generalization of the
  differential approach to recursive query evaluation"**, J. Logic
  Programming 1987 — the semi-naive evaluation the paper cites.
- Previous chapter:
  [reading-relational-ematching.md](reading-relational-ematching.md).
  Next: [reading-egglog-source.md](reading-egglog-source.md), which
  reads the engine these ideas turned into.
- Topic 21's [egg chapter](../21-formal/reading-egg-popl21.md) for the
  e-class analysis and `rebuild` that Step 6 replaces.
