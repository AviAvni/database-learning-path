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
**1111.0 ms per 100-change batch** (this topic's measured headline —
`../../FINDINGS.md` row 27, reproduced in `README.md`'s "The problem,
measured" lane); DBSP's claim is that for *any* query built from its
operators, the version that costs per-change instead of per-database is
not designed but *derived* — by one definition and a handful of rewrite
rules.

## The concepts, step by step

### Step 1 — Z-sets: make deletion a first-class value

> **In:** a collection plus a batch of table changes (rows inserted,
> rows deleted). **Out:** one *value* — a Z-set — in which the sign of
> each element's weight encodes insert (+) vs delete (−), and which is
> closed under addition and negation (an abelian group).

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

> **In:** a stream — one Z-set per logical clock tick (a transaction's
> changes, or a full snapshot). **Out:** the four circuit primitives
> (z⁻¹, I, D, and a lifted query Q) plus the inversion identity
> D(I(s)) = I(D(s)) = s that makes I and D mutually inverse.

A **stream** is an infinite sequence of values, one per logical clock
tick — a function ℕ→group, where each tick's value is a Z-set (one
transaction's worth of changes, or one snapshot). Circuits are built
from exactly four operators:

```
  z^-1  delay (one-tick memory)            operator/z1.rs:221 Z1
  I     integrate: running sum             operator/integrate.rs:85
  D     differentiate: a - z^-1(a)         operator/differentiate.rs:38
  Q     any query, lifted pointwise
```

The paper pins each down precisely (§2). **Delay** (Def 2.5):
z⁻¹(s)[0] = 0 and z⁻¹(s)[t] = s[t−1] for t ≥ 1 — output the input one
tick late (the only stateful primitive, one value of memory).
**Differentiation** (Def 2.17): D(s) := s − z⁻¹(s), so D(s)[t] =
s[t] − s[t−1] — feldera writes this comment verbatim at
`differentiate.rs:31` (`differentiate(a) = a - z^-1(a)`):

```rust
// feldera crates/dbsp/src/operator/differentiate.rs
30  /// Computes the difference between current and previous value
31  /// of `self`: `differentiate(a) = a - z^-1(a)`.
38  pub fn differentiate(&self) -> Stream<C, D> {
```

**Integration** (Def 2.19, Prop 2.20): I(s)[t] = Σ_{i≤t} s[i] — the
running sum; feldera's `integrate.rs:80` documents it with the same
example, `input 1,1,1,1,1… → output 1,2,3,4,5…`. Worked on s = id =
[0 1 2 3 4 …]: D(id) = [0 1 1 1 1 …] (each tick minus the last) and
I(id) = [0 1 3 6 10 …] (partial sums). "Lifted" means an ordinary query
Q applied independently at every tick. The identity everything hangs on
is the paper's **Theorem 2.22 (inversion): I(D(s)) = D(I(s)) = s** —
integrate and differentiate are mutually inverse, which only works
because Step 1 gave us subtraction.

### Step 3 — incrementalization, defined in one line

> **In:** any query Q that maps a full state to a full view. **Out:** its
> change-to-change version, *defined* as Q^Δ := D ∘ Q ∘ I (Def 3.1) —
> correct by construction, ruinous if run literally.

The incremental version of any query is *defined* as
**Q^Δ = D ∘ Q ∘ I**: integrate the input deltas back into full states,
run the ordinary query on each state, differentiate the outputs back
into deltas. Read as a spec, it is trivially correct — feed in change
streams, get out exactly the view's change stream. Read as an
implementation, it is the enemy itself: materialize the whole database
and recompute the whole view every tick. The calculus' work is rewriting
Q^Δ until the Is and Ds vanish or shrink — Step 4.

### Step 4 — the rewrite rules: push I and D through the query

> **In:** Q^Δ = D ∘ Q ∘ I, with the expensive I and D wrapped around Q.
> **Out:** an equivalent circuit in which I and D are pushed inward until
> only small per-operator state survives — nothing for linear operators,
> two delayed integrals for a join, one integral for a nonlinear operator.

Three theorems do almost all the work; quote them as the paper states
them (§3):

```
  linear (Thm 3.3):    Q^Δ = Q                       for LTI Q
  bilinear (Thm 3.4):  (a×b)^Δ = a×b + z^-1(I(a))×b + a×z^-1(I(b))
  chain (Prop 3.2):    (Q1∘Q2)^Δ = Q1^Δ ∘ Q2^Δ       incrementalize COMPOSITIONALLY
```

**Linear** operators (map, filter, flat_map, union — those that
distribute over addition) are their own incremental versions (Thm 3.3):
deltas stream straight through, zero state. The **bilinear** join is
linear in each input separately, and Theorem 3.4 is exact — note it uses
the *delayed* integrals `z⁻¹(I(a))` and `z⁻¹(I(b))`, i.e. each input's
accumulated state *as of the previous tick*. The paper then rewrites it
into "the familiar formula for incremental equi-joins," Δ(a×b) =
Δa×Δb + a×Δb + Δa×b, where `a`,`b` are the accumulated relations. Those
delayed integrals are precisely differential's arrangements, which is
why `djoin.rs:43` insists "A, B are the states BEFORE the deltas."

Worked example (scalar × as the bilinear op, to keep the arithmetic in
view). Let the change streams be a = [2 3 1 …], b = [5 1 4 …]. Then
I(a) = [2 5 6 …], z⁻¹(I(a)) = [0 2 5 …], z⁻¹(I(b)) = [0 5 6 …]. Theorem
3.4 per tick:

- t=0: 2·5 + 0·5 + 2·0 = **10**
- t=1: 3·1 + 2·1 + 3·5 = **20**
- t=2: 1·4 + 5·4 + 1·6 = **30**

Cross-check against the definition Q^Δ = D∘Q∘I: I(a)·I(b) =
[10 30 60], and D of that = [10 20 30]. Same stream — the theorem is the
definition with the I/D pushed through the multiply. As an operator, the
state is exactly two integrals, one delayed (`z⁻¹`):

```rust
// ILLUSTRATION — shape of Thm 3.4 as a stepping operator;
// the real bilinear delta lives in experiments/src/djoin.rs:43
// and feldera crates/dbsp/src/operator/join.rs:350 (join_generic).
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

The genuinely **nonlinear** operators keep their integral — that stored
I(input) is the state, and it's *all* the state. `distinct` is the
paper's canonical example (Def 4.3; its incremental form is derived
specially in Prop 4.7, still O(|change|)). The order-sensitive
aggregates `min`/`max`/`top-k` are nonlinear because deleting the current
survivor (the current maximum) forces the operator to consult the
runner-up. Even SQL `GROUP BY` `SUM`/`COUNT` keep state: the underlying
summation is linear and "automatically incremental" on its own (§7.4),
but emitting the grouped *relation* `(key, aggregate)` composes it with
the nonlinear `makeset` step, so a group's output row must be retracted
and re-emitted when its aggregate changes — "the count function… is not
linear since it uses the makeset non-linear function" (§7.4). The
`zset.rs` `distinct_is_not_linear` test pins the operator the whole
scheme turns on (question 2).

### Step 5 — the chain rule: why this covers a whole SQL dialect

> **In:** a composite query Q1 ∘ Q2. **Out:** (Q1 ∘ Q2)^Δ = Q1^Δ ∘ Q2^Δ
> (Prop 3.2) — incrementalize each primitive once, and a whole dialect
> falls out for free.

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

> **In:** a recursive (fixpoint) query — e.g. transitive closure.
> **Out:** a nested circuit: δ₀ introduces an inner stream, an inner loop
> iterates the query to fixpoint within one outer tick, and ∫ reads the
> fixpoint back out — no partially-ordered timestamps required.

DBSP handles recursion by nesting: an inner circuit with its own clock
runs to fixpoint *within* each outer tick (`DelayedFeedback`, z1.rs:37,
wires the cycle; `delta0.rs:22` is feldera's counterpart of the paper's
**δ₀** stream-introduction operator — "the delta function… δ₀(v)[t] = v
for t=0, else 0" (§5), which imports a parent-circuit value as the
inner stream at inner-time 0; the dual **∫** reads the fixpoint back
out). Same expressive result as differential's lattice timestamps, but
staged — outer tick, then inner fixpoint — rather than a general product
order. The trade (question 3): DBSP gives up mixing epochs mid-iteration
and out-of-order input within a tick; it gains engineering simplicity
and clean per-tick transactional semantics — Feldera's "synchronous
circuit" story.

### Step 7 — what the calculus buys a database

> **In:** the finished calculus (operators, Q^Δ, the rewrite theorems).
> **Out:** three database-grade payoffs — per-tick transactions, state
> that is nothing but integrals (so checkpointing is trivial), and the
> FalkorDB/M27 delta-matrix mapping.

- **Per-tick transactions**: each input Z-set batch = one transaction;
  outputs are exactly the view deltas for that transaction. This is the
  contract M27's standing Cypher queries want: mutation batch in, result
  delta out, push to subscribers.
- **State = integrals**: every stateful operator's memory is I(something),
  spillable to storage (feldera's `storage/` crate) — checkpointing is
  checkpointing integrals, nothing else (z1.rs's `CommittedZ1` :231).
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
| `operator/z1.rs:231` `CommittedZ1` | 7 | checkpointing integrals |

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

## Done when

Answer each before unfolding it.

- [ ] Why do Z-sets make deletion a first-class value, and why can't plain sets?
  <details><summary>answer</summary>

  Z-sets attach an integer weight to every element and form an abelian
  group, so a deletion is just adding the element with weight −1 and every
  value has a negation. Plain sets have no subtraction — there is no set
  that "removes" another — so a change and a collection cannot share a
  type. Group structure is the precondition for I and D (Step 1).

  </details>
- [ ] Name the four operators and write incrementalization in one line.
  <details><summary>answer</summary>

  z⁻¹ (delay), I (integrate, running sum), D (differentiate, a − z⁻¹(a)),
  and a lifted query Q. Incrementalization is Q^Δ := D ∘ Q ∘ I (Def 3.1).

  </details>
- [ ] Prove the bilinear rule by expanding `Q^Δ = D∘Q∘I`.
  <details><summary>answer</summary>

  Expand D(I(a)×I(b))[t] = I(a)[t]×I(b)[t] − I(a)[t−1]×I(b)[t−1]. Write
  I(a)[t] = I(a)[t−1] + a[t] and likewise for b, multiply out, and the
  cross terms collect into Theorem 3.4: a×b + z⁻¹(I(a))×b + a×z⁻¹(I(b)).
  The delayed integrals z⁻¹(I(·)) are why the code keeps *delayed* traces
  (the arrangements / states-before-the-deltas).

  </details>
- [ ] Explain the chain rule and why it covers a whole dialect, not one query.
  <details><summary>answer</summary>

  (Q1∘Q2)^Δ = Q1^Δ ∘ Q2^Δ (Prop 3.2), proved by inserting I∘D = id
  between the two stages. So you give each primitive its ^Δ form once and
  compose; no per-query delta derivation is ever needed — that is what
  Feldera's SQL-to-circuit compiler exploits.

  </details>
- [ ] Say how recursion is handled by nested circuits.
  <details><summary>answer</summary>

  δ₀ introduces an inner stream from an outer value (§5); an inner loop
  with its own clock and a z⁻¹ back-edge iterates the query to fixpoint
  within one outer tick; ∫ reads the fixpoint back out. No
  partially-ordered timestamps — the nesting is staged, outer then inner.

  </details>
- [ ] You wrote answers to all questions in notes.md, including the wedge count.
  <details><summary>answer</summary>

  This topic measures the full-recompute wedge join at 1111.0 ms per
  100-change batch (`../../FINDINGS.md` row 27 / README measured lane).
  Your notes should carry the DBSP circuit for the wedge count and mark
  which arrows carry deltas vs integrals.

  </details>

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
