# DBSP: incremental view maintenance as a calculus

The VLDB '23 best paper reduces incremental view maintenance to an
algebra of four stream operators and two identities, so that
incrementalizing ANY query becomes a mechanical rewrite. This chapter
builds the calculus one operator at a time — the group structure, the
four primitives, the definition of "incremental," and the rewrite rules
that make it cheap — then anchors every piece in Feldera's production
Rust implementation, where each operator of the calculus is a file.

## The problem in one sentence

The topic bench recomputes a 2-hop wedge join from scratch in
**894.3 ms per 100-change batch**; DBSP's claim is that for *any* query
built from its operators, the version that costs per-change instead of
per-database is not designed but *derived* — by one definition and a
handful of rewrite rules.

## The concepts, step by step

### Step 1 — Z-sets: make deletion a first-class value

A **Z-set** is a collection where every element carries an integer
weight — weight +2 means "present twice," weight −1 means "one copy
removed" — so a batch of table changes is itself a value: inserts are
positive weights, deletes negative. The algebraic point of the weights:
Z-sets form an **abelian group** (you can add two of them
element-wise, and every value has a negation), whereas plain sets do
not — there is no set that "subtracts." The entire calculus runs on this:
a *change* to a collection and the collection itself have the same type,
and undoing something is adding its negation. (Where the group structure
is genuinely load-bearing — and what it does to `distinct`, which our
zset.rs `distinct_is_not_linear` test pokes at — is question 2.)

### Step 2 — streams and the four operators

A **stream** is an infinite sequence of values, one per logical clock
tick — a function ℕ→group, where each tick's value is a Z-set (one
transaction's worth of changes, or one snapshot). Circuits are built
from exactly four operators:

```
  z^-1  delay (one-tick memory)            operator/z1.rs:221 Z1
  I     integrate: running sum             operator/integrate.rs:85
  D     differentiate: x[t] - x[t-1]       operator/differentiate.rs:38
  Q     any query, lifted pointwise
```

`z^-1` outputs its input one tick late (the only stateful primitive —
one value of memory). `I` turns a stream of deltas into a stream of
accumulated states (running sum). `D` turns states back into deltas.
"Lifted" means an ordinary query Q applied independently at every tick.
The two identities that everything hangs on: **D(I(x)) = x and
I(D(x)) = x** — integrate and differentiate are mutually inverse, which
only works because Step 1 gave us subtraction.

### Step 3 — incrementalization, defined in one line

The incremental version of any query is *defined* as
**Q^Δ = D ∘ Q ∘ I**: integrate the input deltas back into full states,
run the ordinary query on each state, differentiate the outputs back
into deltas. Read as a spec, it is trivially correct — feed in change
streams, get out exactly the view's change stream. Read as an
implementation, it is the enemy itself: materialize the whole database
and recompute the whole view every tick. The calculus' work is rewriting
Q^Δ until the Is and Ds vanish or shrink — Step 4.

### Step 4 — the rewrite rules: push I and D through the query

Three rules do almost all the work:

```
  linear Q:      Q^Δ = Q                      (deltas stream through)
  bilinear join: (A⋈B)^Δ = ΔA⋈I(B) + I(A)⋈ΔB + ΔA⋈ΔB
                                 ^ the z^-1-delayed integrals = arrangements
  chain rule:    (Q1∘Q2)^Δ = Q1^Δ ∘ Q2^Δ     (incrementalize COMPOSITIONALLY)
```

**Linear** operators (map, filter, flat_map, union — those that
distribute over addition) are their own incremental versions: deltas
stream straight through, zero state. The **bilinear** join (linear in
each input separately) needs exactly two pieces of state — the integrals
of its inputs, one of them delayed — which are precisely differential's
arrangements. As an operator — note the state is exactly two integrals,
one of them delayed (`z^-1`):

```rust
struct IncJoin { ia: ZSet, ib_delayed: ZSet }    // I(A), z^-1(I(B))

fn step(&mut self, da: &ZSet, db: &ZSet) -> ZSet {
    // (A⋈B)^Δ = ΔA ⋈ z^-1(I(B))  +  I(A) ⋈ ΔB
    self.ia.merge(da);                           // integrate A first...
    let out = join(da, &self.ib_delayed)         // ...ΔA sees B BEFORE this tick
        .plus(&join(&self.ia, db));              // ΔB sees A including ΔA:
    self.ib_delayed.merge(db);                   //   the ΔA⋈ΔB term, absorbed
    out                                          // = the view delta, exactly
}
```

Nonlinear operators (distinct, count, sum, top-k, min/max) keep their
integral — that stored I(input) is the state, and it's *all* the state.

### Step 5 — the chain rule: why this covers a whole SQL dialect

The chain rule — (Q1∘Q2)^Δ = Q1^Δ ∘ Q2^Δ — is the paper's practical
bombshell: incrementalization is **compositional**, so you
incrementalize operator-by-operator, and a whole SQL dialect (joins,
aggregates, window functions, recursion) is covered by giving each
primitive its ^Δ form *once*. That's Feldera's SQL-to-circuit compiler:
parse SQL to a circuit of primitives, replace each primitive by its
known incremental form, done. No per-query cleverness, no view-specific
delta derivations — the property every hand-rolled IVM system (including
RisingWave's executors, next guide) has to approximate operator by
operator, DBSP gets as a theorem.

### Step 6 — recursion: nested circuits instead of lattice times

DBSP handles recursion by nesting: an inner circuit with its own clock
runs to fixpoint *within* each outer tick (`DelayedFeedback`, z1.rs:37,
wires the cycle; `delta0.rs` injects an outer-clock stream into the
inner circuit — the paper's δ₀). Same expressive result as
differential's lattice timestamps, but staged — outer tick, then inner
fixpoint — rather than a general product order. The trade (question 3):
DBSP gives up mixing epochs mid-iteration and out-of-order input within
a tick; it gains engineering simplicity and clean per-tick transactional
semantics — Feldera's "synchronous circuit" story.

### Step 7 — what the calculus buys a database

- **Per-tick transactions**: each input Z-set batch = one transaction;
  outputs are exactly the view deltas for that transaction. This is the
  contract M27's standing Cypher queries want: mutation batch in, result
  delta out, push to subscribers.
- **State = integrals**: every stateful operator's memory is I(something),
  spillable to storage (feldera's `storage/` crate) — checkpointing is
  checkpointing integrals, nothing else (z1.rs's `CommittedZ1` :241).
- **The FalkorDB mapping (M27)**: delta matrix DP−DM is ΔA for one tick;
  `wait` = I. A standing pattern query is Q; what M27 must build is Q^Δ —
  masked SpGEMM terms ΔA·A + A·ΔA + ΔA·ΔA instead of recomputing A²
  (our tri.rs stub is exactly this with scalar sets).

## Where each step lives in the code

[feldera](https://github.com/feldera/feldera) `crates/dbsp/src/`:

| anchor | step | what it is |
|---|---|---|
| `algebra/zset/` | 1 | the ZSet/IndexedZSet traits — weighted collections as a trait hierarchy over "batch" storage |
| `operator/z1.rs:221` | 2 | `Z1` — the delay; `DelayedFeedback` :37 is how cycles (recursion) are wired |
| `operator/integrate.rs:85` | 2 | `integrate` — the running trace; `integrate_nested` :158 for inner circuit clocks |
| `operator/differentiate.rs:38` | 2 | D; note `differentiate_with_initial_value` :105 for bootstrapping from a snapshot |
| `operator/join.rs:123/:283/:350` | 4 | `join`, `stream_join_generic`, `join_generic` — the ^Δ forms specialized |
| `operator/distinct.rs`, `aggregate.rs` | 4 | the nonlinear ops, each carrying its integral |
| `operator/delta0.rs` | 6 | injects an outer-clock stream into a nested circuit — the paper's δ₀ |
| `operator/z1.rs:241` `CommittedZ1` | 7 | checkpointing integrals |

Paper route: read §1–4 (the algebra — Steps 1–5) with the operator table
open; read §5 (recursion — Step 6) if the differential guide left
questions about what nesting trades against lattice times.

## Questions to answer in notes.md

1. Prove the bilinear rule from Q^Δ = D∘Q∘I by expanding
   I(a)[t]·I(b)[t] − I(a)[t−1]·I(b)[t−1]. Note where z^-1 appears —
   that's why the code's join keeps *delayed* traces.
2. Z-sets with i64 weights form an abelian group; sets don't (no
   negatives). Where exactly does the theory need inverses? What happens
   to `distinct` — and why does the paper single it out as the operator
   that breaks linearity (compare our zset.rs `distinct_is_not_linear`
   test)?
3. Differential timestamps: arbitrary lattice, updates at mixed times
   consolidate freely. DBSP: strict tick-by-tick semantics, recursion via
   nesting. What does DBSP *give up* (hint: out-of-order input within a
   tick; multi-epoch overlap of iterations) and what does it gain
   (engineering simplicity, per-tick transactional semantics — Feldera's
   "synchronous circuit" story)?
4. Take `MATCH (a)-[]->(b)-[]->(c) RETURN count(*)` — the wedge count in
   ivm_bench. Write its DBSP circuit (two-input bilinear join + linear
   count), mark which arrows carry deltas and which carry integrals, and
   identify what FalkorDB already stores (A, ΔA as delta matrices) vs
   what M27 must add (the arranged join state — nothing! wedges need only
   A itself: the integrals ARE the adjacency matrices).

## References

**Papers**
- Budiu, Chajed, McSherry, Ryzhyk, Tannen — "DBSP: Automatic
  Incremental View Maintenance for Rich Query Languages" (VLDB 2023,
  [arXiv:2203.16684](https://arxiv.org/abs/2203.16684)) — read §1-4
  (the algebra), §5 (recursion) if the differential guide left
  questions

**Code**
- [feldera](https://github.com/feldera/feldera) `crates/dbsp/src/` —
  the production implementation; `algebra/zset/`, `operator/z1.rs`,
  `operator/integrate.rs`, `operator/differentiate.rs`,
  `operator/join.rs`, `operator/delta0.rs` per the anchor table
