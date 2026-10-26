# CozoDB: semi-naive at the rule level, magic sets for the query, and graph algorithms as fixed rules

Cozo is the Datalog engine that is a *database*: an embeddable, transactional
store (the README lists in-memory, SQLite and RocksDB storage
backends) whose query language is Datalog, with vector search and full-text
indexes inside it. For this topic it matters for three reasons. Its
recursion is a **coarser** semi-naive than Soufflé's or Ascent's, and the
difference is measurable from lane 1. It rewrites a query's bound arguments
into the recursion (**magic sets**), the Datalog form of what topic 47 does
with semi-joins. And it lets a graph algorithm — BFS, PageRank, shortest
paths — stand in a rule body as a black box, the way FalkorDB's `algo.*`
procedures stand in a Cypher query.

Read at `cozodb/cozo` commit `481af05` (version 0.7.6, a fresh clone at
`~/repos/cozo`; line numbers belong to it). No paper accompanies the engine;
this chapter is source-only, so every claim is an anchor you can open.

Read [reading-souffle.md](reading-souffle.md) first for the words *delta*,
*stratum* and *derivation*.

## The problem in one sentence

Where does a database whose rules are written by users, evaluated inside a
transaction, and mixed with stored relations and built-in algorithms, draw the
line on how clever its recursion is — and what does that line cost on the
nonlinear rule that every closure-style program contains?

## The concepts, step by step

### Step 1 — the evaluation skeleton: strata, epochs, `EpochStore`

> **In:** a compiled program, already split into strata. **Out:** the outer
> loop and the three-part store each rule's results live in.

Cozo stratifies the program (`query/stratify.rs`), then evaluates the strata
in order; each stratum runs `semi_naive_magic_evaluate`, which loops over
**epochs** — its word for a round:

```rust
// cozo-core/src/query/eval.rs:129-133 — the epoch loop (cozo 481af05)
129          for epoch in 0u32.. {
130              debug!("epoch {}", epoch);
131              let mut to_merge = BTreeMap::new();
132              let borrowed_stores = stores as &BTreeMap<_, _>;
133              if epoch == 0 {
```

Epoch 0 evaluates every rule in full (`initial_rule_non_aggr_eval`, line
305); every later epoch calls `incremental_rule_non_aggr_eval` (line 507),
merges what came back into each relation's store, and stops when no store
has a delta (the merge at lines 292–300: `changed |= old_store.has_delta()` at 296, `if !changed { break; }` at 298). Each rule
set's results live in an `EpochStore` holding a `total` and a `delta`
(`runtime/temp_store.rs:249-254`), both backed by
`BTreeMap<Tuple, bool>` (`temp_store.rs:27-28`) — an **ordered** in-memory
map, not a hash set. That is the first difference from lane 1 (hash-indexed)
and the same choice as Soufflé's B-trees: lookups by prefix are range
scans, which is how `EpochStore::prefix_iter` (`temp_store.rs:315`) finds the tuples
whose first columns equal a key. Lane 2's sorted relations take this side.

The rules of one stratum are evaluated in parallel: the epoch-0 closure is
dispatched with rayon's `par_iter` over the stratum's rule sets
(`eval.rs:200`; the incremental epochs do the same at line 275), except entry rules
under a `:limit`, which run sequentially so the limit is deterministic.

### Step 2 — semi-naive, one rule at a time: `need_complete_run`

> **In:** one rule in an epoch ≥ 1. **Out:** the three cases Cozo
> distinguishes, and what the third costs.

For each rule the engine asks which relations in its body changed in the
previous epoch, and how many times each appears:

```rust
// cozo-core/src/query/eval.rs:520-537 — incremental evaluation of one rule (cozo 481af05)
520              let mut need_complete_run = false;
521              let mut dependencies_changed = false;
522
523              for (symb, multiplicity) in rule.contained_rules.iter() {
524                  if stores.get(symb).unwrap().has_delta() {
525                      dependencies_changed = true;
526                      if *multiplicity == ContainedRuleMultiplicity::Many {
527                          need_complete_run = true;
528                          break;
529                      }
530                  }
531              }
532
533              if !dependencies_changed {
534                  continue;
535              }
536
537              if need_complete_run {
```

`contained_rules` maps each derived relation the body mentions to
`One` or `Many` — `Many` when the relation occurs more than once
(`data/program.rs:910-925`, the `Entry::Occupied` arm setting `Many`).
Three cases follow. **(a)** nothing the rule reads changed → skip it (line
534). **(b)** something changed and every changed relation occurs once →
evaluate the rule with the *delta* in place of that relation's total, one
pass per changed relation (lines 570–600, `rule.relation.iter(self,
Some(delta_key), stores)`). **(c)** a changed relation occurs **more than
once** → line 537: a *complete run*, the whole rule over the whole
relations, with every re-derived tuple discarded by `prev_store.exists`
(line 542, logged as "rederived").

Case (c) is naive evaluation, applied to that rule. Cozo has no split into
`new×old, old×new` — no analogue of Soufflé's per-atom versions or Ascent's
`versions_base`. So for linear transitive closure (`tc` once in the body)
Cozo does case (b), exactly lane 1's `semi-naive` row; for the nonlinear
form `tc(x,z), tc(z,y)` it does case (c), exactly lane 1's `naive` row —
same derivations by construction, since a complete run re-derives every
tuple of tc ⋈ tc each epoch. Lane 1, power-law n=1500, derivations:

```
tc nonlinear  naive                       5,882,220   ← what Cozo's rule does
tc nonlinear  semi-naive, textbook        2,749,550
tc nonlinear  semi-naive, old-delta       2,143,923   ← Soufflé / Ascent
```

— **2.74×** the derivations (5,882,220 / 2,143,923) for the rule written
the natural way. The authors made a trade, visible in the comment at line
541, "improvement: the clauses can actually be evaluated in parallel": the
engine is built for many small rules over stored data inside a transaction,
not for large single-rule closures, and a per-rule test is cheap to reason
about. Two things soften it. Programs are usually written linear (`edge`
once), which lands in case (b). And the `rederived` filter at line 542 means
the *stored* result is still correct and duplicate-free — only the work is
repeated.

### Step 3 — merging an epoch: where "delta" comes from

> **In:** the `RegularTempStore` an epoch's rules produced. **Out:** how a
> store computes its delta, and the one shortcut it takes.

```rust
// cozo-core/src/runtime/temp_store.rs:67-88 — merge a round's output into the total store (cozo 481af05)
67      pub(crate) fn merge_in(&mut self, prev: &mut Self, mut new: Self) -> bool {
68          prev.inner.clear();
69          if new.inner.is_empty() {
70              return false;
71          }
72          if self.inner.is_empty() {
73              mem::swap(&mut new, self);
74              return true;
75          }
76          for (k, v) in new.inner {
77              match self.inner.entry(k) {
78                  Entry::Vacant(ent) => {
79                      prev.inner.insert(ent.key().clone(), v);
80                      ent.insert(v);
81                  }
82                  Entry::Occupied(mut ent) => {
83                      ent.insert(v);
84                  }
85              }
86          }
87          false
88      }
```

Line 68 clears the old delta (`prev`); lines 78–81 add each genuinely new
key to both the total and the new delta. The shortcut is lines 72–74: when
the total is empty (the first merge) the new tuples *become* the total by a
swap and the function returns `true`, telling `EpochStore` to use the total
as the delta (`use_total_for_delta`, `temp_store.rs:252`) rather than
copy it. A **meet-aggregation** store (`MeetAggrStore`, `temp_store.rs:92`) does the
same with a lattice join in the occupied arm — the way Cozo runs recursive
`min`/`max`/`count` aggregations, the Datalog-level analogue of Ascent's
`lattice` columns.

### Step 4 — magic sets: specializing recursion to the query

> **In:** a query with a bound argument, e.g. `?[y] := reach[1, y]`.
> **Out:** what the rewrite does to the recursion, and the measured size of
> the win.

A recursive rule evaluated bottom-up computes the **whole** relation even if
the query asks for one row of it. A **magic-set rewrite** (Bancilhon et al.,
1986) specializes the program to the query's *binding pattern*: for each
relation it creates an **adorned** copy marked with which arguments are
bound (`bf` = first bound, second free), and an auxiliary "magic" relation
holding exactly the bound values that can ever be asked for, threaded
through the body so each atom is only evaluated on bindings that arrive
from the query. This threading is **sideways information passing** (SIP):
what one atom learns is passed to the next — the same idea as a semi-join
(topic 47), in Datalog clothes; Cozo's intermediate **supplementary**
relations (`MagicSymbol::Sup`, `magic.rs:105`) are the semi-join results
carried between body atoms.

```rust
// cozo-core/src/query/magic.rs:55-65 — the rewrite entry point (cozo 481af05)
55      pub(crate) fn magic_sets_rewrite(self, tx: &SessionTx<'_>) -> Result<StratifiedMagicProgram> {
56          let mut exempt_rules = BTreeSet::from([Symbol::new(PROG_ENTRY, SourceSpan(0, 0))]);
57          let mut collected = vec![];
58          for prog in self.0 {
59              prog.exempt_aggr_rules_for_magic_sets(&mut exempt_rules);
60              let down_stream_rules = prog.get_downstream_rules();
61              let adorned = prog.adorn(&exempt_rules, tx)?;
62              collected.push(adorned.magic_rewrite());
63              exempt_rules.extend(down_stream_rules);
64          }
```

Line 56 exempts the entry rule from rewriting; 59 exempts rules involved in
aggregation (a rewrite is unsound through them); 61 **adorns**; 62
rewrites. A program can switch it off (`:disable_magic_rewrite true`,
`magic.rs:653` is a test that uses it).

The effect is measurable without Cozo, because for linear TC the
rewritten program *is* a one-column recursion seeded by the query constant:

```
?- tc(0, y).     magic-set specialisation:
reach(y) :- arc(0, y).      reach(y) :- reach(z), arc(z, y).
```

Lane 1 runs it (`eval::reach_from`) beside the full closure for each
dataset — see the README's lane-1 table for the exact counts: the
goal-directed program derives only the rows reachable from vertex 0 instead
of every row of the closure.

### Step 5 — fixed rules: an algorithm as a body atom

> **In:** a query that calls a built-in algorithm. **Out:** how a non-Datalog
> computation sits inside the fixpoint loop.

`CompiledRuleSet::Fixed` (`eval.rs:170`) is a rule set whose "body" is a
built-in algorithm — the directory `fixed_rule/algos/` holds BFS, DFS,
PageRank, Louvain, Dijkstra, Yen, triangles and about a dozen more. The
engine hands the algorithm its input relations (`FixedRulePayload`, line 173)
and takes back a store; in later epochs the case is a one-liner: "no need to
do anything, algos are only calculated once" (line 251). An algorithm is
therefore a **non-recursive** stratum member that produces a relation other
rules can join with. This is the design FalkorDB's procedures
(`CALL algo.BFS(...) YIELD nodes`) also follow: the traversal is an opaque
operator, and the query language joins its output. It means a Datalog engine
need not be competitive with a dedicated kernel on BFS — it only needs to
call one.

## How to read the source (with the concepts in hand)

1. `cozo-core/src/query/eval.rs:65-111` — `stratified_magic_evaluate`; then
   113-300 — the epoch loop; then 305-505 (initial) and 507-690
   (incremental, both the plain and the meet-aggregate versions).
2. `cozo-core/src/data/program.rs:905-925` and `query/compile.rs:78-92` —
   where `One`/`Many` come from.
3. `cozo-core/src/runtime/temp_store.rs:24-90, 249-330` — stores and merge.
4. `cozo-core/src/query/magic.rs:55-200` — adornment and rewrite; read with
   the Bancilhon–Maier–Sagiv–Ullman magic-sets paper beside it.

## Where each step lives in this topic

| step | file / lane | what to look at |
|---|---|---|
| 2 | lane 1, `tc nonlinear naive` row | Cozo's complete run: 5,882,220 vs 2,143,923 (power-law) |
| 1, 3 | lane 2, `src/sorted.rs` (stub) | ordered stores, delta merged by walking both |
| 4 | lane 1, `?- tc(0,y)` rows | goal-directed derivations vs the full closure |

## Questions (answer in notes.md)

1. A rule `a(x,y) :- b(x,z), c(z,y).` with `b` and `c` both in the stratum
   and both changed. Which of Step 2's cases applies and how many passes
   does the engine make?
2. Same rule with `b` and `c` the same relation. Which case, and why is the
   engine's choice safe but slower?
3. Why does Cozo's `merge_in` return `true` when its total is empty, and what
   would break if it copied instead of swapping?
4. Rewrite `reach(y) :- reach(z), arc(z,y)` for the query `?- reach(5)`
   (membership test instead of enumeration). What does the magic relation
   hold and how would the evaluation stop early?

## Done when

Answer each before unfolding it.

- [ ] You can state the three cases of Cozo's per-rule semi-naive step.

  <details><summary>Answer</summary>

  (a) no relation the rule reads changed → skip; (b) changed relations each
  appear once → evaluate with that relation's delta, one pass per changed
  relation; (c) a changed relation appears more than once
  (`ContainedRuleMultiplicity::Many`) → run the rule completely over the
  totals and discard re-derived tuples with `prev_store.exists`
  (`eval.rs:520-542`).

  </details>

- [ ] You can say what that costs on nonlinear TC and where the number comes from.

  <details><summary>Answer</summary>

  Case (c) is naive evaluation for that rule, so its derivation count equals
  lane 1's naive row: 5,882,220 on the power-law graph, against 2,143,923 for
  the disjoint delta split Soufflé and Ascent use — 2.74× more. Linear TC
  (one occurrence) takes case (b) and matches lane 1's semi-naive row.

  </details>

- [ ] You can explain what a magic-set rewrite does and why it is a semi-join.

  <details><summary>Answer</summary>

  It adorns each relation with the binding pattern the query induces and adds
  a magic relation holding the bound values that can be asked, passing
  bindings atom to atom (sideways information passing) so each atom is
  evaluated only for values that can reach the query. The supplementary
  relations carried between atoms are semi-join results — "keep only rows
  with a partner" — the same operation as topic 47's reduction. For linear
  TC the rewrite leaves a unary recursion seeded by the query constant,
  which lane 1 measures.

  </details>

- [ ] You can say how a graph algorithm sits inside a Datalog query in Cozo.

  <details><summary>Answer</summary>

  As a `Fixed` rule set: the engine passes the algorithm its input relations,
  stores the result, and never re-evaluates it in later epochs
  (`eval.rs:170-173, 251`), so rules in later strata join with the output
  like any relation.

  </details>

## References

- `cozodb/cozo` at `481af05` (v0.7.6): `cozo-core/src/query/eval.rs`,
  `compile.rs`, `magic.rs`, `stratify.rs`; `src/data/program.rs`;
  `src/runtime/temp_store.rs`; `src/fixed_rule/algos/`.
- Bancilhon, Maier, Sagiv, Ullman, **"Magic Sets and Other Strange Ways to
  Implement Logic Programs"**, PODS 1986 — named, not read for this chapter.
- Topic 47's [README](../47-semijoin-reduction/README.md) for the semi-join
  view, and [reading-souffle.md](reading-souffle.md) /
  [reading-ascent.md](reading-ascent.md) for the finer delta split Cozo does
  not make.
