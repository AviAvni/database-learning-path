# Reading egglog: the e-graph that is a database engine

The two previous chapters are papers. This one is a codebase, and the
reason to read it is that the papers understate what happened. PLDI'23
describes "approximately 4,200 lines of Rust"; the repository today is a
workspace whose largest crate is called **`core-relations`** and which
contains a table with a clustered sort column, a hash index, a query
planner with hypertree decomposition, a join executor with two
strategies, and a union-find. If you had been handed those files with
the names changed you would call it a small analytical database.

That is the point worth taking from this chapter. **Congruence closure,
e-matching and rebuilding are not implemented as e-graph algorithms in
egglog. They are a schema constraint, a query, and a rule.** The
engineering underneath is ordinary database engineering, and it is
ordinary database engineering that made it fast.

Anchors are `egraphs-good/egglog` at the commit `resources/codebases.md`
pins (`e264c37a`), quoted with the line numbers they occupy there.
Read the [POPL'22](reading-relational-ematching.md) and
[PLDI'23](reading-egglog-pldi23.md) chapters first: this one assumes
generic join, delta rules and `:merge`.

## The problem in one sentence

Everything the two papers propose has to survive contact with a
representation — where do the tuples live, how do you find the ones a
variable is bound to, how do you say "only the new ones", and what
happens to all of it when a union invalidates half the ids — and
egglog's answers to those four questions are, respectively, a sorted row
buffer, a hash index over columns, a binary search on a timestamp
column, and a rule.

## The concepts, step by step

### Step 1 — the map of the workspace

> **In:** the two papers. **Out:** which crate answers which question,
> so the rest of the chapter has somewhere to put each file.

```
   egglog/
     src/                  the language: parser, typechecker, sorts,
                           extraction, proofs. This is "egglog" as a user
                           meets it.
     egglog-bridge/        the glue that turns egglog's semantics —
                           functions, :merge, rebuilding — into rules and
                           tables for the engine below.
     core-relations/       the engine. tables, indexes, query, free_join
                           (plan + execute), offsets, actions.
     union-find/           two union-finds, single-threaded and concurrent.
     egglog-ast/, concurrency/, numeric-id/   supporting crates.
```

The split is the thesis in directory form. `core-relations` knows
nothing about e-graphs — its table module says so out loud (Step 2) —
and `egglog-bridge` is where "this is an e-graph" is expressed, in terms
the engine already had.

### Step 2 — the table, field by field

> **In:** Step 1's map. **Out:** the physical representation every later
> step manipulates, and the deliberate omission at the top of the file.

```rust
// core-relations/src/table/mod.rs, lines 1-5 — the module's opening claim
     1  //! A generic table implementation supporting sorted writes.
     2  //!
     3  //! The primary difference between this table and the `Function` implementation
     4  //! in egglog is that high level concepts like "timestamp" and "merge function"
     5  //! are abstracted away from the core functionality of the table.
```

Read that twice. The engine's table does not know what a timestamp
means; it knows it may be asked to keep rows sorted by some column. It
does not know what congruence is; it knows it may be handed a merge
function. Every e-graph concept enters as a *parameter*.

```rust
// core-relations/src/table/mod.rs, lines 136-152 — the whole table.
// The line to look at is 143: one nominated column decides the physical order.
   136  pub struct SortedWritesTable {
   137      generation: Generation,
   138      data: Rows,
   139      hash: ShardedHashTable<TableEntry>,
   140
   141      n_keys: usize,
   142      n_columns: usize,
   143      sort_by: Option<ColumnId>,
   144      offsets: Vec<(Value, RowId)>,
   145
   146      pending_state: Arc<PendingState>,
   147      merge: Arc<MergeFn>,
   148      to_rebuild: Vec<ColumnId>,
   149      rebuild_index: Index<ColumnIndex>,
   150      // Used to manage incremental rebuilds.
   151      subset_tracker: SubsetTracker,
   152  }
```

Field by field, in database vocabulary:

- `data: Rows` — a row buffer. Tuples, contiguously.
- `hash` — a hash index from key columns to row id, **sharded** for
  parallel insert. `n_keys` says how many leading columns are the key:
  this is the functional dependency of PLDI'23 §3.2, declared.
- `sort_by` + `offsets` — the rows are kept in order of one nominated
  column, and `offsets` records where each distinct value of it starts.
  A **clustered index**, in other words, and Step 3 is what it is for.
- `merge` — the `:merge` expression, as a function pointer. Congruence
  is a value in this field.
- `to_rebuild`, `rebuild_index`, `subset_tracker` — which columns hold
  ids that a union can displace, and enough state to repair only the
  affected rows.
- `generation` — bumped when the table changes shape enough to
  invalidate cached indexes and subsets. Cache invalidation, given a
  name.

### Step 3 — "only the new tuples" is a range scan

> **In:** Step 2's `sort_by` column. **Out:** the mechanism behind
> PLDI'23's semi-naive evaluation, which turns out to be an index seek.

The previous chapter left semi-naive evaluation as an expansion into
delta rules. Here is what a delta rule *is*:

```rust
// core-relations/src/query.rs, lines 252-256 — doc comment on
// add_rule_from_cached_plan (the fn itself is at :257)
   252      /// The primary use-case is seminaive evaluation: an egglog rule is compiled
   253      /// once into a [`CachedPlan`] and then added to a fresh [`RuleSet`] each
   254      /// iteration with timestamp constraints (e.g. `GeConst` on the focus atom)
   255      /// that select only new tuples. If no new tuples exist for an atom, the
   256      /// `None` return allows the caller to skip that variant entirely.
```

Three separate ideas in five lines. The plan is compiled **once** and
reused every iteration, so delta rules cost no planning. The delta is
expressed as a **constraint on a column** rather than as a separate
relation. And when an atom has no new tuples, the whole rule variant is
dropped before it runs — the `None` return.

And the constraint itself is not a filter. Because the timestamp is the
`sort_by` column, `GeConst` becomes a binary search returning a
contiguous range of rows:

```rust
// core-relations/src/table/mod.rs, lines 497-510 — inside fast_subset (:445).
// Line 499 is the argument: a delta is found, not scanned for.
   497              Constraint::GeConst { col, val } => {
   498                  if col == &sort_by {
   499                      match self.binary_search_sort_val(*val) {
   500                          Ok((found, _)) => {
   501                              Some(Subset::Dense(OffsetRange::new(found, self.data.next_row())))
   502                          }
   503                          Err(next) => {
   504                              Some(Subset::Dense(OffsetRange::new(next, self.data.next_row())))
   505                          }
   506                      }
   507                  } else {
   508                      None
   509                  }
   510              }
```

`else { None }` is the honest part: on any other column, `fast_subset`
declines and the caller falls back to filtering. This is exactly the
difference between a clustered and an unclustered index, and egglog gets
the clustered one for the only column where it is worth having.

Work the cost. Lane 2 of this topic's bench has 60,000 tuples and a
delta of 24. A filter costs 60,000 comparisons; a binary search over
~20,000 distinct timestamps costs about `log₂(20,000) ≈ 14` probes and
returns an offset range. The saving is not the constant factor on the
scan — it is that the delta rule never touches the old rows at all.

### Step 4 — a `Subset` is how "which rows" is represented

> **In:** the `Subset::Dense` returned in Step 3. **Out:** the one type
> that carries intermediate results through the join, and why it has two
> shapes.

```rust
// core-relations/src/offsets/mod.rs, lines 333-338
   333  /// Either or an offset range or a sorted offset vector.
   334  #[derive(Debug, Hash, PartialEq, Eq)]
   335  pub enum Subset {
   336      Dense(OffsetRange),
   337      Sparse(Pooled<SortedOffsetVector>),
   338  }
```

Every intermediate in the executor is a set of row ids, and it is stored
either as a **range** — two integers, when the rows happen to be
contiguous, which is exactly what a timestamp seek produces — or as a
**sorted vector** of row ids.

Both halves of that choice are cashed in by `Subset::intersect`
(`offsets/mod.rs:402`), which is four cases and no hashing anywhere:

```
   dense  ∩ dense    max of the starts, min of the ends — two comparisons   :404-411
   dense  ∩ sparse   two binary searches, then a subslice                   :413-426
   sparse ∩ dense    the same, compacted in place                           :432-441
   sparse ∩ sparse   two-pointer MERGE when the sides are within 4x         :447-467
                     of each other …
                     … and GALLOPING into the longer side when they are      :468-516
                     not — the comment at :470 gives the reason,
                     "O(other_len * log(cur_len / other_len)) vs
                      O(cur_len) for retain"
```

That is topic 23's postings-list intersection menu — merge for similar
lengths, galloping for skewed ones — chosen by the same size ratio, in a
join engine instead of a search engine. Topic 26's readers will
recognise the storage half as roaring's array-vs-bitmap container
decision, taken per intermediate rather than per block. And `Pooled<…>`
means the sorted vector comes from a pool: allocation on the join path
is treated as a cost worth eliminating, which is the same lesson this
topic's own `gj` learned in `notes.md`.

### Step 5 — a query, and the plan it is compiled to once

> **In:** Steps 2–4. **Out:** where the conjunctive query of POPL'22
> lives in this codebase, and the two-phase compilation it goes through.

`core-relations/src/query.rs` builds a `Query` from **atoms** — a table
plus a list of variables or constants — and hands it to
`plan_query` (`free_join/plan.rs:1183`), which returns a `Plan`. The
module comment is the best short description of a modern join planner
you will find in a source file, so read it whole; here is its skeleton:

```rust
// core-relations/src/free_join/plan.rs, lines 3-23 — the two phases.
// Line 13 names the algorithm the first phase is: variable elimination.
     3  //! At a high level, the query planner has two phases: **(hyper)tree decomposition** and **join planning for each bag**.
     4  //! Both phases are very subtle, and heuristics are heavily used for good performance.
     5  //!
     6  //! # (Hyper)tree Decomposition
     7  //!
     8  //! A conjunctive query can be viewed as a hypergraph where variables are vertices and atoms (relations) are hyperedges.
     9  //! The idea of tree decomposition is to break this hypergraph into a tree of overlapping subqueries called *bags*,
    10  //! each of which is cheaper to evaluate independently. This is the classical idea behind tree decomposition and the
    11  //! Yannakakis algorithm.
    12  //!
    13  //! The decomposition proceeds via *variable elimination*: we iteratively pick a variable `v` and eliminate the neighborhood
    14  //! `N(v)` (which also includes `v`) from the hypergraph, and add back a hyperedge consisting of `N(v) - {v}`, until
    15  //! there are no variables left. Each elimination step gives us a bag. A min-fill heuristic
    16  //! (`next_var_to_eliminate`) guides the order of elimination to keep bags small. After all variables are eliminated,
    17  //! redundant bags are pruned: bags subsumed by another (all their variables are covered) are merged, and "ears"
    18  //! are merged into their parent.
    // ... 19–20: topologically sort the bags, split message vs private vars ...
    21  //! The materialized result of each bag has its output keyed on the *message variables* it shares with
    22  //! its parent, and the parent uses that materialization to prune its own search space.
    23  //!
```

Vocabulary, defined here because the comment assumes it:

- The **query hypergraph** is the one from the POPL'22 chapter, Step 8:
  a vertex per variable, a hyperedge per atom.
- A **tree decomposition** covers that hypergraph with **bags** — sets
  of variables — arranged in a tree, such that every atom fits inside
  some bag and every variable's bags form a connected subtree. An
  **acyclic** query is one with a decomposition whose bags are single
  atoms.
- **Yannakakis' algorithm** evaluates an acyclic query in time linear in
  input plus output by passing *semijoin messages* up and down that
  tree, so that no bag ever materialises a tuple that cannot survive.
- **Variable elimination** builds a decomposition greedily: repeatedly
  remove a variable and connect its neighbours. The **min-fill**
  heuristic picks the variable that adds the fewest new connections —
  the standard heuristic from probabilistic graphical models, here
  scoring on atom occurrences and column cardinality estimates
  (`plan.rs:420` `next_var_to_eliminate`, whose body at `:441-452`
  counts occurrences and consults a size estimate).
- **Message variables** are the ones a bag shares with its parent — the
  join keys of the semijoin, and the columns the bag's materialised
  result is keyed on.

That is topic 10's optimizer, in a system with no SQL in it. And note
what the comment says about the fallback: "When the query hypergraph is
a single connected component with no beneficial decomposition, the
planner falls back to a `SinglePlan` with no materialization steps."
A planner that knows when not to plan.

### Step 6 — two join strategies, and the space between them

> **In:** the bags of Step 5. **Out:** what actually runs inside one
> bag, and where generic join sits relative to a hash join.

```rust
// core-relations/src/free_join/plan.rs, lines 32-41 — the strategies.
// Line 38 is the sentence to keep: one plan space, two familiar corners.
    32  //! - **Generic Join** (`PlanStrategy::Gj`): The classic worst-case optimal join algorithm. Each stage picks one variable
    33  //!   and intersects the columns of atoms that correspond to this variable (`JoinStage::Intersect`).
    34  //!
    35  //! - **Free Join** (`PlanStrategy::PureSize` / `PlanStrategy::MinCover`): From Remy's paper. The planning algorithm
    36  //!   does the following: Each stage it selects a *cover* — a (sub)atom whose columns span the variables being bound in that step — and
    37  //!   uses it to probe all other atoms that share those variables (`JoinStage::FusedIntersect`). When the cover is an
    38  //!   entire atom and there is only one relation to probe, this degenerates to a hash join; when covers are single-column
    39  //!   scans it ~ recovers generic join*.
    40  //!
    41  //!   *: although this is not worst-case optimal because it does not necessarily picks the smallest side to scan.
```

`Gj` is the algorithm this topic's `relational.rs` implements.
`MinCover` is the [Free Join](reading-free-join.md) plan space, and the
footnote at line 41 is the honest caveat: the production default is not
worst-case optimal, because picking a cover is not the same as always
scanning the smallest side. Worst-case optimality is a property the
engine will trade for speed, deliberately, and says so in a comment.

### Step 7 — the executor, doing the thing the bound requires

> **In:** a `JoinStage::Intersect` from Step 6. **Out:** the production
> version of `relational.rs`'s "iterate the smallest, probe the rest".

```rust
// core-relations/src/free_join/execute.rs, lines 1460-1469 — the two-scan
// intersection. Line 1465 is the whole O(min |R_j.x|) requirement.
  1460                  [a, b] => {
  1461                      let a_prober = self.get_column_index(atoms, binding_info, a.atom, a.column);
  1462                      let b_prober = self.get_column_index(atoms, binding_info, b.atom, b.column);
  1463
  1464                      let ((smaller, smaller_scan), (larger, larger_scan)) =
  1465                          if a_prober.len() < b_prober.len() {
  1466                              ((&a_prober, a), (&b_prober, b))
  1467                          } else {
  1468                              ((&b_prober, b), (&a_prober, a))
  1469                          };
```

Compare our own, which is the same decision written for two to eight
atoms instead of specialised at two:

```rust
// relational.rs, lines 199-203 (this topic's crate)
   199      // Intersect smallest-first, which is what buys the O(min |R_j.x|) bound.
   200      let lead = *part[..n_part]
   201          .iter()
   202          .min_by_key(|&&i| cur[i].kids.len())
   203          .expect("non-empty");
```

Two details in the surrounding production code that our version does not
have, and that are worth knowing exist. First, a size threshold: at
`execute.rs:1431` a subset of 16 rows or fewer is refined directly,
while a larger one goes through `get_cached_trie_node` (`:1439`) — the
trie node is *built lazily and cached*, so the index is materialised
only where it pays. Second, results are accumulated into `FrameUpdates`
and drained in **chunks** (`:1453`), which is what lets the join run in
parallel — vectorised execution, in topic 11's sense, arriving for the
same reason it arrived there.

### Step 8 — rebuilding is a rule

> **In:** everything above. **Out:** the answer to "where is congruence
> closure implemented", which is: nowhere, on purpose.

Topic 21 read egg's `rebuild` — a hand-written worklist that repairs the
congruence invariant. Look for its equivalent here and you find a
*rule builder*:

```rust
// egglog-bridge/src/lib.rs, lines 951-957 — inside incremental_rebuild_rule (:945).
// The comment on 954 is the whole design.
   951          let subsume = self.funcs[table].can_subsume;
   952          let table_id = self.funcs[table].table;
   953          let uf_table = self.uf_table;
   954          // Two atoms, one binding a whole tuple, one binding a displaced column
   955          let mut rb = self.new_rule(&format!("incremental rebuild {table:?}, {col:?}"), true);
   956          rb.set_plan_strategy(PlanStrategy::MinCover);
   957          let mut vars = Vec::<QueryEntry>::with_capacity(schema.len());
```

Rebuilding is compiled, per table and per id-typed column
(`incremental_rebuild_rules`, `:932`), into a two-atom query: find a
tuple, join it against the union-find table on a column whose id has
been displaced, and write the canonical version back. It is planned by
the same planner (`MinCover` is chosen for it explicitly) and executed
by the same executor. There is a `nonincremental_rebuild` (`:994`)
alongside it for the case where scanning everything is cheaper, and
`EGraph::rebuild` (`:722`) picks between them — including a short-circuit
at `:703` that skips the full rebuild when no unions happened.

This is what "unifying Datalog and equality saturation" cashes out to.
Every optimisation the query engine gains — a better plan, a lazily
built index, chunked parallel execution — is inherited by congruence
closure, because congruence closure is a query.

### Step 9 — the union-find that declines the textbook

> **In:** the union-find referenced by Step 8's rule. **Out:** a
> deliberate asymptotic sacrifice, and the reason for it.

```rust
// union-find/src/lib.rs, lines 6-12 — the crate's own justification
     6  //! Both structures are fairly rudimentary and are customized to be used in an
     7  //! egraph-related setting. In particular, they do "union by min id", which is a
     8  //! strategy that _does not_ guarantee the same asymptotic complexity as the
     9  //! main techniques in the literature (e.g. union by rank). Union by min is a
    10  //! heuristic introduced to reduce the number of ids perturbed during congruence
    11  //! closure. There's likely more to do in this area but for now it seems to work
    12  //! well enough. It doesn't hurt that it's also simpler to implement.
```

**Union by rank** attaches the shorter tree under the taller one, which
with path compression gives the near-constant `O(α(n))` amortised bound
every textbook quotes. **Union by min id** instead always keeps the
numerically smaller id as the representative — giving up that bound on
purpose.

Why is it worth giving up? Because in this system a union's real cost is
not the union-find operation, it is everything downstream: every row
whose id stops being canonical has to be rewritten by Step 8's rebuild
rule. Keeping the smaller id stable means an id that has been canonical
for a long time — and therefore appears in many rows — tends to stay
canonical. **The union-find is optimised for the rebuild it triggers,
not for itself.**

Topic 21 found egg making the same kind of choice for the same kind of
reason (its `find(&self)` does not path-compress, because the read path
takes `&self`). Two independent implementations, two textbook
optimisations declined, both because the union-find is embedded in
something bigger.

## How to read the source (with the concepts in hand)

A two-hour path, top-down, that stays on the database side:

1. `core-relations/src/free_join/plan.rs`, **lines 1-46**. The module
   comment. Everything else is easier afterwards.
2. `core-relations/src/table/mod.rs`, **lines 1-5 and 136-172**. The
   doc, the struct, and the `Clone` impl right after it — which is a
   good check on your reading, because what it chooses *not* to clone
   (the indexes) tells you what is derived state.
3. `fast_subset` (`table/mod.rs:445-512`) in full: five constraint kinds,
   one of which is fast and only on the sorted column.
4. `core-relations/src/offsets/mod.rs:333` and the `Offsets` trait above
   it — 20 lines, and every intermediate in the engine is one of these.
   Then `intersect` at `:402-519`, which is a compact tour of set
   intersection strategies.
5. `core-relations/src/query.rs`, the `RuleSetBuilder` API, ending at
   `add_rule_from_cached_plan` (`:257`).
6. `free_join/execute.rs:1418-1560`, the `Intersect` stage at one, two
   and many scans. Skim the parallel machinery; read the size threshold
   at `:1431`.
7. `egglog-bridge/src/lib.rs:932-1050`, the rebuild rules, then `:722`
   for how they are driven.
8. `union-find/src/lib.rs` entire — 104 lines.

Then, for contrast, re-read this topic's `relational.rs` (250 lines) and
list what it is missing. That list is the difference between an
algorithm and an engine.

## Where each step lives in the code

| step | file:line |
|---|---|
| 2, the table | `core-relations/src/table/mod.rs:1-5`, `:136-152` |
| 3, delta as a seek | `core-relations/src/query.rs:252-256`, `table/mod.rs:445` `fast_subset`, `:497-510` `GeConst`, `:983` binary search |
| 4, intermediates | `core-relations/src/offsets/mod.rs:333-338`, `:402-519` `intersect` |
| 5, the planner | `free_join/plan.rs:1-46` (doc), `:1183` `plan_query`, `:420` `next_var_to_eliminate` |
| 6, strategies | `free_join/plan.rs:32-41`, `:134-158` `JoinStage` |
| 7, the executor | `free_join/execute.rs:1418` `Intersect`, `:1431` size threshold, `:1460-1469` smaller-first, `:1453` chunking |
| 8, rebuilding | `egglog-bridge/src/lib.rs:932`, `:945`, `:994`, `:722`, `:703` |
| 9, union-find | `union-find/src/lib.rs:1-12`, `:55` `union` |
| this topic's toy | `relational.rs:78` `index_atom`, `:116` `plan`, `:170` `gj` |

## Questions (answer in notes.md)

1. `SortedWritesTable` has one `sort_by` column. If you wanted a second
   sort order — say, to make a different atom's delta a range scan too —
   what would you have to add, and what would it cost on the write path?
   Compare with a second clustered index in postgres (topic 3).
2. `fast_subset` returns `None` for a `GeConst` on any column other than
   `sort_by`. Trace what the caller does with that `None`
   (`free_join/mod.rs:805` `split_fast_slow`) and describe the fallback
   in database terms.
3. The min-fill heuristic (`plan.rs:420`) scores variables by occurrence
   count and column cardinality. Our `relational.rs::plan` scores by
   atom count then relation size. Construct a query where the two
   orderings differ and say which is better and why.
4. Step 6's footnote says the Free Join strategies are not worst-case
   optimal. Write the query and database where `MinCover` does
   asymptotically more work than `Gj`, and then argue why the default is
   still the right default.
5. Rebuilding as a rule (Step 8) means congruence closure is planned.
   What would a *bad* plan for the incremental rebuild rule look like,
   and what in the schema stops the planner from choosing it? (Hint:
   `n_keys`, and PLDI'23 §3.2.)
6. Union by min id trades the `O(α(n))` bound for fewer perturbed ids.
   Design the measurement that would decide whether the trade pays on
   the Figure 2 e-graph, and predict the result before running it.

## Done when

Answer each before unfolding it.

- [ ] You can say what `core-relations` deliberately does not know, and
      why that matters.
  <details><summary>Answer</summary>

  It does not know what a timestamp is or what a merge function means —
  `table/mod.rs:1-5` says both are "abstracted away from the core
  functionality of the table". A timestamp is just the column named in
  `sort_by`; congruence is just a value in the `merge` field. That
  separation is what lets the same engine run Datalog, lattice analyses
  and equality saturation, and it is why `egglog-bridge` exists: it is
  the only crate that knows the parameters mean "e-graph".
  </details>

- [ ] You can explain how a delta rule finds its tuples without scanning.
  <details><summary>Answer</summary>

  The rule's plan is compiled once and re-added each iteration with a
  `GeConst` constraint on the focus atom's timestamp column
  (`query.rs:252-256`). Because that column is the table's `sort_by`
  column, `fast_subset` answers the constraint with a binary search
  (`table/mod.rs:497-510`, `:983`) and returns a `Subset::Dense` — a
  contiguous offset range. It is an index seek on a clustered index. On
  any other column `fast_subset` returns `None` and the caller filters
  instead.
  </details>

- [ ] You can name the planner's two phases and what each produces.
  <details><summary>Answer</summary>

  Phase one is **hypertree decomposition** by variable elimination with
  a min-fill heuristic (`plan.rs:3-18`, `:420`): it breaks the query
  hypergraph into a tree of *bags*, prunes subsumed bags and ears, and
  chooses each bag's *message variables* — the ones shared with its
  parent, which its materialised result is keyed on. That is Yannakakis'
  semijoin idea. Phase two plans the join *inside* each bag as a list of
  `JoinStage`s, using either `Gj` (generic join, one variable per stage)
  or Free Join (`PureSize`/`MinCover`, a cover per stage). A `JoinHeader`
  applies constant constraints before the loop starts.
  </details>

- [ ] You can point at the line in production code that implements
      generic join's `O(min |R_j.x|)` requirement.
  <details><summary>Answer</summary>

  `execute.rs:1464-1469`: with two scans, the executor compares
  `a_prober.len()` and `b_prober.len()` and iterates the smaller,
  probing the larger. That is the same decision as `relational.rs:200`'s
  `min_by_key`, specialised to the two-atom case. Around it are two
  things the toy lacks: a 16-row threshold below which a subset is
  refined directly instead of indexed (`:1431`, `:1439`), and chunked
  draining of results so the stage can run in parallel (`:1453`).
  </details>

- [ ] You can say where congruence closure is implemented in egglog.
  <details><summary>Answer</summary>

  It is not, as an algorithm. `egglog-bridge/src/lib.rs:945`
  `incremental_rebuild_rule` *compiles a rule*: two atoms, one binding a
  whole tuple and one binding a displaced column
  (comment at `:954`), planned with `PlanStrategy::MinCover` and
  executed by the ordinary join executor. `:932` builds one such rule
  per id-typed column, `:994` provides a non-incremental variant, and
  `:722`/`:703` choose between them and skip entirely when no unions
  happened. Congruence inherits every improvement the query engine gets.
  </details>

- [ ] You can explain why egglog's union-find gives up the textbook
      bound.
  <details><summary>Answer</summary>

  It unions **by min id** rather than by rank (`union-find/src/lib.rs:6-12`),
  which the crate admits "does not guarantee the same asymptotic
  complexity". The reason is that the expensive consequence of a union
  is not the union — it is the rebuild it triggers, which must rewrite
  every row whose id stopped being canonical. Keeping the smaller id
  canonical keeps long-lived ids stable and so perturbs fewer rows. The
  union-find is tuned for its caller. egg declines a different textbook
  optimisation (path compression on the `&self` path) for a
  structurally similar reason.
  </details>

## References

- `egraphs-good/egglog` at the pinned commit (see the pin table at the
  end of [resources/codebases.md](../../resources/codebases.md)).
  The crates read here: `core-relations` (table, offsets, query,
  free_join), `egglog-bridge`, `union-find`.
- Zhang et al., **"Better Together: Unifying Datalog and Equality
  Saturation"**, PLDI 2023, arXiv:2304.04332 — §5.1 describes the
  components this chapter reads, at the size they were in 2023.
- Wang, Willsey, Suciu, **"Free Join: Unifying Worst-Case Optimal and
  Traditional Joins"**, SIGMOD 2023 — the `PureSize`/`MinCover`
  strategies. Next chapter: [reading-free-join.md](reading-free-join.md).
- Mihalis Yannakakis, **"Algorithms for Acyclic Database Schemes"**,
  VLDB 1981 — the semijoin program the decomposition phase is aiming at.
- Topic 21's [egg chapter](../21-formal/reading-egg-popl21.md) for the
  hand-written `rebuild` this engine replaces with a rule.
