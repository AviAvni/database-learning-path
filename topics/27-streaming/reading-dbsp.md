# Reading guide — DBSP (VLDB '23 best paper) + Feldera code

**Sources:**
- Budiu, Chajed, McSherry, Ryzhyk, Tannen — "DBSP: Automatic Incremental
  View Maintenance for Rich Query Languages" (VLDB 2023) — read §1-4
  (the algebra), §5 (recursion) if the differential guide left questions
- `~/repos/feldera/crates/dbsp/src/` — the production implementation

## 1. DBSP's move: make IVM a *calculus*, not a system

Differential is a brilliant system; DBSP is the theory that explains it
with four operators. Streams are functions ℕ→group; circuits are built
from:

```
  z^-1  delay (one-tick memory)            operator/z1.rs:221 Z1
  I     integrate: running sum             operator/integrate.rs:85
  D     differentiate: x[t] - x[t-1]       operator/differentiate.rs:38
  Q     any query, lifted pointwise
```

with the two identities D(I(x)) = x and I(D(x)) = x. The incremental
version of any query is *defined* as `Q^Δ = D ∘ Q ∘ I` — and then a
rewrite system pushes I/D inward:

```
  linear Q:      Q^Δ = Q                      (deltas stream through)
  bilinear join: (A⋈B)^Δ = ΔA⋈I(B) + I(A)⋈ΔB + ΔA⋈ΔB
                                 ^ the z^-1-delayed integrals = arrangements
  chain rule:    (Q1∘Q2)^Δ = Q1^Δ ∘ Q2^Δ     (incrementalize COMPOSITIONALLY)
```

The chain rule is the paper's practical bombshell: you incrementalize
operator-by-operator, so a whole SQL dialect (joins, aggregates, window
functions, recursion) is covered by giving each primitive its ^Δ form
once. That's Feldera's SQL-to-circuit compiler.

**Q1.** Prove the bilinear rule from Q^Δ = D∘Q∘I by expanding
I(a)[t]·I(b)[t] − I(a)[t−1]·I(b)[t−1]. Note where z^-1 appears — that's
why the code's join keeps *delayed* traces.

**Q2.** Z-sets with i64 weights form an abelian group; sets don't
(no negatives). Where exactly does the theory need inverses? What happens
to `distinct` — and why does the paper single it out as the operator that
breaks linearity (compare our zset.rs `distinct_is_not_linear` test)?

## 2. Feldera code anchors

| anchor | what it is |
|---|---|
| `algebra/zset/` | the ZSet/IndexedZSet traits — weighted collections as a trait hierarchy over "batch" storage |
| `operator/z1.rs:221` | `Z1` — the delay; `DelayedFeedback` :37 is how cycles (recursion) are wired |
| `operator/integrate.rs:85` | `integrate` — the running trace; `integrate_nested` :158 for inner circuit clocks |
| `operator/differentiate.rs:38` | D; note `differentiate_with_initial_value` :105 for bootstrapping from a snapshot |
| `operator/join.rs:123/:283/:350` | `join`, `stream_join_generic`, `join_generic` — the ^Δ forms specialized |
| `operator/distinct.rs`, `aggregate.rs` | the nonlinear ops, each carrying its integral |
| `operator/delta0.rs` | injects an outer-clock stream into a nested circuit — the paper's δ₀ |

Nested circuits are DBSP's recursion answer: an inner circuit with its own
clock runs to fixpoint per outer tick — same expressive result as
differential's lattice times, but staged (outer tick, then inner
fixpoint) rather than a general product order.

**Q3.** Differential timestamps: arbitrary lattice, updates at mixed
times consolidate freely. DBSP: strict tick-by-tick semantics, recursion
via nesting. What does DBSP *give up* (hint: out-of-order input within a
tick; multi-epoch overlap of iterations) and what does it gain
(engineering simplicity, per-tick transactional semantics — Feldera's
"synchronous circuit" story)?

## 3. The database claims

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

**Q4.** Take `MATCH (a)-[]->(b)-[]->(c) RETURN count(*)` — the wedge
count in ivm_bench. Write its DBSP circuit (two-input bilinear join +
linear count), mark which arrows carry deltas and which carry integrals,
and identify what FalkorDB already stores (A, ΔA as delta matrices) vs
what M27 must add (the arranged join state — nothing! wedges need only A
itself: the integrals ARE the adjacency matrices).
