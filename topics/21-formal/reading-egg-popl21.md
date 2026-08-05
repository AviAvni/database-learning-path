# egg: equality saturation with deferred rebuilding

The paper that fixes this topic's measured failure — using this topic's exact
expression. egg's POPL 2021 paper opens §2.2 by applying `(𝑎 × 2)/2 → (𝑎 ≪ 1)/2`
and observing that "applying strength reduction at this point prevents us from
canceling out 2/2", which is character-for-character what our `hand.rs` lane
does. This chapter builds the e-graph from its three parts — a union-find, a
map from ids to e-classes, and a hashcons — states the two invariants that
distinguish it from a plain union-find, then gets to the contribution:
**deferred rebuilding**, letting congruence go stale on purpose and repairing it
in one batch. Then it says exactly what egg's headline speedup was measured
against, because that number is quoted wrongly more often than not.

Every code anchor below is `egraphs-good/egg` at the commit this repo pins,
**`f94c346`** (`resources/codebases.md` pin table), quoted with the line numbers
the code occupies at that commit. Every paper claim names the section, figure or
definition it came from in the POPL 2021 paper (arXiv:2004.03082).

## The problem in one sentence

Our hand-ordered rewriter answers `(a*2)/2` with `(a << 1) / 2` and stops at
cost 5 after **one** rule firing, because rewriting *destructively* means the
best local move — strength reduction — deletes the `*2` that `x/x → 1` needed;
the repair is to stop deleting, which needs a data structure that holds every
equivalent form at once and can restore its congruence invariant fast enough
that holding them is affordable.

## The concepts, step by step

### Step 1 — union-find: what egg's actually is, not what the textbook says

> **In:** ids that become equal over time, one pair at a time.
> **Out:** a canonical id per equivalence class, and an honest cost for `find`
> — which is *not* the O(α) you were taught, because egg's union-find is not
> the textbook one.

A **union-find** (disjoint-set) structure maintains a partition of ids under
two operations: `find(x)` returns the partition's canonical representative, and
`union(x, y)` merges two partitions. It is the standard answer to "these two
things just became equal; remember that, cheaply, forever."

egg's entire union-find is `src/unionfind.rs`, **93 lines** — of which the
implementation is lines 1–51 and the rest is a `#[cfg(test)]` module. Here it
is, all of it:

```rust
// egg src/unionfind.rs, lines 30-50 — the whole implementation, verbatim
    30      pub fn find(&self, mut current: Id) -> Id {
    31          while current != self.parent(current) {
    32              current = self.parent(current)
    33          }
    34          current
    35      }
    36
    37      pub fn find_mut(&mut self, mut current: Id) -> Id {
    38          while current != self.parent(current) {
    39              let grandparent = self.parent(self.parent(current));
    40              *self.parent_mut(current) = grandparent;
    41              current = grandparent;
    42          }
    43          current
    44      }
    45
    46      /// Given two leader ids, unions the two eclasses making root1 the leader.
    47      pub fn union(&mut self, root1: Id, root2: Id) -> Id {
    48          *self.parent_mut(root2) = root1;
    49          root1
    50      }
```

Three things the code says that the textbook does not:

- `find` (30–35) does **no path compression at all**. It walks to the root and
  leaves the tree exactly as it found it. Only `find_mut` (37–44) compresses,
  and it does **path halving** — each visited node is re-pointed at its
  *grandparent*, not at the root. Halving is one pointer write per step instead
  of a second pass; it gets the same asymptotic bound as full compression, but
  "re-points ids directly at the root" is not what line 40 does.
- `union` (47–50) is **unconditional**: `root1` always wins. There is no
  union-by-rank and no rank array in the struct.
- The balancing decision therefore lives one level up. `EGraph::perform_union`
  picks which root survives by **parent-list length**, not by tree height:

```rust
// egg src/egraph.rs, lines 1170-1175 and 1182 — the leader choice, elided between
  1170          // make sure class2 has fewer parents
  1171          let class1_parents = self.classes[&id1].parents.len();
  1172          let class2_parents = self.classes[&id2].parents.len();
  1173          if class1_parents < class2_parents {
  1174              core::mem::swap(&mut id1, &mut id2);
  1175          }
  1182          self.unionfind.union(id1, id2);
```

So the honest cost statement is: `find` is a pointer walk whose length is
bounded by how unbalanced the forest got; `find_mut` halves paths as it goes,
so repeated canonicalization of the same ids is cheap; and the heuristic that
keeps trees shallow optimises for *fewer parents to re-canonicalize later*
(Step 6's cost), not for tree height. The textbook O(α(n)) amortized bound
needs union-by-rank **and** compression on every find; egg has neither exactly.
It is fast for the reason the comment on line 1170 gives, and that reason is
about congruence work, not about the union-find.

### Step 2 — the e-graph: three maps, and what "represents" means

> **In:** the union-find of Step 1.
> **Out:** a structure that stores a *set of terms*, and a precise definition
> of which terms it stores — which you will need in Step 3 to count them.

The paper's Definition 2.1 is a tuple `(U, M, H)`:

- **U**, the union-find over e-class ids (Step 1).
- **M**, the **e-class map**, from e-class id to **e-class** — a set of e-nodes.
- **H**, the **hashcons**, a map from e-node to e-class id. (The paper's
  footnote 3: the name evokes memoization, "since both avoid creating new
  duplicates of existing objects." This is topic 8's hash table doing topic 8's
  job.)

An **e-node** is a function symbol paired with a list of **children e-class
ids** — not child subterms. `*` with children `[c3, c7]`, where `c3` and `c7`
are whole classes. The paper's footnote 4 flags what is unusual here: "making
e-classes but not e-nodes identifiable is unique to our definition" — an
e-class has an identity, an e-node does not.

In egg the three maps are struct fields, and their doc comments already tell
you the whole staleness story of Step 6:

```rust
// egg src/egraph.rs, lines 62-88 — EGraph's fields, non-essential attributes elided
    62      /// Stores each enode's `Id`, not the `Id` of the eclass.
    63      /// Enodes in the memo are canonicalized at each rebuild, but after rebuilding new
    64      /// unions can cause them to become out of date.
    66      memo: HashMap<L, Id>,
    67      /// Nodes which need to be processed for rebuilding. The `Id` is the `Id` of the enode,
    68      /// not the canonical id of the eclass.
    69      pending: Vec<Id>,
    70      analysis_pending: UniqueQueue<Id>,
    78      pub(crate) classes: HashMap<Id, EClass<L, N::Data>>,
    81      classes_by_op: HashMap<L::Discriminant, HashSet<Id>>,
    82      /// Whether or not reading operation are allowed on this e-graph.
    83      /// Mutating operations will set this to `false`, and
    84      /// [`EGraph::rebuild`] will set it to true.
    88      pub clean: bool,
```

`memo` is H, `classes` is M, `unionfind` is U, and `clean` (88) is the flag that
says whether the invariants currently hold. Note line 69: `pending` holds
**e-node ids, not e-class ids** — a detail that matters when you read
`process_unions` and expect the paper's pseudocode.

**Representation** (Definition 2.3) is recursive: an e-node `f(a₁, a₂, …)`
represents the term `f(t₁, t₂, …)` when `M[aᵢ]` represents `tᵢ`; an e-class
represents a term if any of its e-nodes do; the e-graph represents a term if
any of its e-classes do. The consequence is the compression: because children
are *classes*, one `/` e-node with children `[c_mul, c_two]` represents `(a*2)/2`
and `(a<<1)/2` simultaneously, as soon as `a*2` and `a<<1` are in `c_mul`.

### Step 3 — count the terms an e-graph represents

> **In:** Definition 2.3 from Step 2, and the paper's Figure 2, which is
> literally our trap expression.
> **Out:** a number, computed — the reason "exponentially many terms in linear
> space" is not a slogan.

Take the paper's Figure 2a: the e-graph containing just `(a×2)/2`. Four
e-classes, one e-node each: `{a}`, `{2}`, `{*}` with children `[{a},{2}]`,
`{/}` with children `[{*},{2}]`. Terms represented: **1**.

Now count with the multiplicative rule that Definition 2.3 gives you: an
e-class represents `Σ over its e-nodes` of `Π over that e-node's children` of
(terms the child class represents).

**Figure 2b**, after `x×2 → x≪1`: the `*` class now holds two e-nodes, `*` and
`<<`, and a new class `{1}` appears. Class `{a}` = 1 term, `{2}` = 1, `{1}` = 1.
The mul class = `(*: 1×1) + (<<: 1×1)` = **2**. The root `/` class =
`/: 2 × 1` = **2** terms — `(a*2)/2` and `(a<<1)/2` — from **5** e-nodes.

**Figure 2c**, after `(x×y)/z → x×(y/z)`: the root class gains a `*` e-node
whose children are `{a}` and a new `/` class holding `2/2`. Root =
`(/: 2×1) + (*: 1 × 1)` = **3** terms from 7 e-nodes.

**Figure 2d**, after `x/x → 1` and `1×x → x`: the paper's own caption says it —
"The resulting e-graph has a cycle, representing infinitely many expressions:
`a`, `a×1`, `a×1×1`, and so on." The class containing `a` now also contains a
`*` e-node one of whose children *is that same class*. Count with the rule
above and the sum diverges. Nine e-nodes; **infinitely many** terms.

That is the whole bargain in four pictures: **1 → 2 → 3 → ∞ terms, from 4 → 5 →
7 → 9 e-nodes.** And the answer we want, `a`, is now in the same e-class as the
input, so extraction (Step 8) finds it at cost 1 — against our hand rewriter's
measured cost 5.

### Step 4 — the two invariants, stated exactly

> **In:** the e-graph of Step 2.
> **Out:** the two properties `rebuild()` exists to restore, in the paper's own
> terms — you cannot reason about "letting them go stale" without them.

**Congruence** (paper Definition 2.6): the equivalence over e-nodes must be
closed under congruence, `(≡node) = (≅*)`. Concretely: if `x ≡ y` then
`f(x) ≡ f(y)`. The paper adds the corollary people forget — "since identical
e-nodes are trivially congruent, this implies that an e-node must be uniquely
contained in a single e-class." **Deduplication is a consequence of congruence**,
not a separate rule.

**The hashcons invariant** (Definition 2.7): `H` maps all *canonical* e-nodes to
their e-class ids —

    e-node n ∈ M[a]  ⟺  H[canonicalize(n)] = find(a)

where `canonicalize(f(a₁,a₂,…)) = f(find(a₁), find(a₂), …)` (Definition 2.2).
Its purpose is one line: when the invariant holds, `lookup(n) = H[canonicalize(n)]`
answers "is there already an e-class with an e-node congruent to `n`?" in one
hash probe.

The cascade these force is the expensive part. After `union(a, b)`, every parent
e-node of the merged classes has a stale child id; re-canonicalizing it may make
it collide in `H` with another e-node, which means *those two are congruent*, so
their classes must be unioned too, which invalidates *their* parents — repeat to
fixpoint. This upward cascade is **congruence closure**, and it is precisely the
invariant egg chooses to let go stale.

### Step 5 — equality saturation, and the trap it repairs

> **In:** an e-graph plus a set of rewrite rules.
> **Out:** the read/write/rebuild loop, and the explicit connection to this
> topic's measured `hand.rs` failure.

**Equality saturation** replaces ordered destructive rewriting with: seed an
e-graph with the input; **e-match** every rule's left-hand side against the whole
e-graph, collecting `(σ, c)` pairs where class `c` represents `ℓ[σ]`; apply each
by `merge(c, add(r[σ]))` — adding the right-hand side and *unioning* it with the
match, never deleting; repeat until **saturated** (no rule adds a node or
performs a merge) or a budget trips; then run an **extractor** to pick the
cheapest represented term.

The paper's §2.2 names our failure for us:

> "Consider applying a simple strength reduction rewrite: `(𝑎 × 2)/2 → (𝑎 ≪ 1)/2`.
> The new term carries no information about the initial term. Applying strength
> reduction at this point prevents us from canceling out `2/2`. In the compilers
> community, this classically tricky question of when to apply which rewrite is
> called the **phase ordering problem**."

That is `topics/21-formal/experiments/src/hand.rs` in one paragraph. Our
rewriter's rule R2 (`x*2 → x<<1`) is tried before R4 (`(x*y)/z → x*(y/z)`), it
fires once, and R4 can never match again because the `*` node is gone. Measured
result, from this topic's `notes.md` baseline: output `(a << 1) / 2`, **cost 5,
1 rule firing**. An e-graph keeps `a*2` *and* `a<<1` in one class, so R4 still
matches, and the chain in Step 3's Figure 2c–2d runs to `a`, cost 1.

egg's loop is one function, and its three phases are visible as three timers:

```rust
// egg src/run.rs, lines 556-595 inside Runner::run_one — read, write, rebuild
   556          result = result.and_then(|_| {
   557              matches = self
   558                  .scheduler
   559                  .search_rewrites(i, &self.egraph, rules, &self.limits)?;
   568          let search_time = start_time.elapsed().as_secs_f64();
   573          result = result.and_then(|_| {
   574              rules.iter().zip(matches).try_for_each(|(rw, ms)| {
   578                  let actually_matched = self.scheduler.apply_rewrite(i, &mut self.egraph, rw, ms);
   587                  self.check_limits()
   588              })
   589          });
   591          let apply_time = apply_time.elapsed().as_secs_f64();
   594          let rebuild_time = Instant::now();
   595          let n_rebuilds = self.egraph.rebuild();
```

Search (556–566) is read-only over the *whole* e-graph — every rule sees the
same snapshot, so no rule can preempt another. Apply (573–589) only mutates.
`rebuild()` is called **once** (595), after all rules have applied. That
separation is the paper's Figure 5b, and Step 6 is why it is allowed.

### Step 6 — deferred rebuilding: the contribution

> **In:** the invariants of Step 4 and the phase-split loop of Step 5.
> **Out:** why batching congruence repair is asymptotically better, worked on
> concrete numbers, and where the deferral happens in egg's source.

Traditional congruence closure restores congruence after **every** merge. egg
defers: `merge` records the work and returns; `rebuild()` does it all at once.
The deferral is one line —

```rust
// egg src/egraph.rs, line 1159 and line 1190, inside perform_union
  1159          self.clean = false;
  1190          self.pending.extend(class2.parents.iter().copied());
```

— and the batch drain is `process_unions`:

```rust
// egg src/egraph.rs, lines 1346-1358 — the pending drain (analysis loop elided)
  1346      fn process_unions(&mut self) -> usize {
  1347          let mut n_unions = 0;
  1348
  1349          while !self.pending.is_empty() || !self.analysis_pending.is_empty() {
  1350              while let Some(class) = self.pending.pop() {
  1351                  let mut node = self.nodes[usize::from(class)].clone();
  1352                  node.update_children(|id| self.find_mut(id));
  1353                  if let Some(memo_class) = self.memo.insert(node, class) {
  1354                      let did_something =
  1355                          self.perform_union(memo_class, class, Some(Justification::Congruence));
  1356                      n_unions += did_something as usize;
  1357                  }
  1358              }
```

Read line 1353 carefully, because it is the whole mechanism: re-canonicalize the
node (1352), re-insert into the hashcons, and **a returned old value means two
e-nodes now hash to the same key** — they are congruent, so union them (1355),
which pushes *their* parents back onto `pending`, which is why 1349 is a loop.
The recursion of Step 4's cascade is a worklist here.

`rebuild()` is the public entry point and does exactly two things plus logging:

```rust
// egg src/egraph.rs, lines 1416-1444 — rebuild, logging elided
  1416      pub fn rebuild(&mut self) -> usize {
  1422          let n_unions = self.process_unions();
  1423          let trimmed_nodes = self.rebuild_classes();
  1443          debug_assert!(self.check_memo());
  1444          self.clean = true;
```

**Work the asymptotics on numbers.** The paper's §3.2.1 gives two workloads.
Take the second: `w` terms each nested under `d` function symbols,
`f₁(f₂(…f_d(x₁)))` … `f₁(f₂(…f_d(x_w)))`, and a workload of `w−1` merges that
merge all the `x`s together.

- *Eager.* Each `merge(xᵢ, xⱼ)` needs `O(d)` `repair` calls, one per layer of
  `f`s. Over `w−1` merges: `O(wd)`.
- *Deferred.* All `w−1` merges happen first. The `x`s are now one e-class `c_x`,
  so the **deduplicated** worklist has exactly one element. Repairing `c_x`
  merges the `f_d` layer into one class; the worklist deduplicates to one
  element again; repeat per layer. Total: `O(d)`.

With `w = 100`, `d = 10`: eager does on the order of `100 × 10 = 1000` repair
calls; deferred does on the order of `10`. **A factor of `w` — the width —
disappears**, and it disappears because the worklist deduplicates. The paper's
first workload gives the same shape for hashcons updates: `O(n²)` eager against
`O(n)` deferred.

This is the same move as topic 20's delta-matrix `wait` and topic 4's LSM
memtable flush: make the mutation O(1) by batching the expensive invariant
restoration, and pay once per batch instead of once per mutation. The price is
that between rebuilds the hashcons is *stale* — `memo` holds non-canonical keys
(the doc comment at `egraph.rs:63-64` says so) — which is safe only because
Step 5's phase split guarantees nobody reads the e-graph during the write phase.

The paper's footnote 5 is worth the honesty: Z3's e-graph already separated read
and write phases "as an implementation detail"; egg is "the first algorithm to
take advantage of this by deferring invariant maintenance."

### Step 7 — what the 88× actually measures

> **In:** the deferred algorithm of Step 6.
> **Out:** the paper's number, with its baseline, its benchmark and its
> machine — because "egg is 88× faster" is false as usually stated.

The figure is §3.4 and **Figure 6**. Read it precisely:

- **The baseline is egg itself**, modified so that `rebuild` is invoked after
  every merge. It is *not* another tool, not Z3, not a prior eqsat engine. The
  paper is measuring one algorithmic change inside one codebase.
- **The benchmark is egg's own test suite** — the `math` (computer algebra) and
  `lambda` (untyped-λ partial evaluator) test sets, **32 tests**. Eight of the
  32 hit the iteration limit of 100; the rest saturated.
- **Two numbers, not one.** Aggregated as a **geometric mean over the 32 tests**:
  **88×** on *congruence closure alone*, and **21×** on the whole equality
  saturation algorithm. Quoting 88× as the end-to-end speedup overstates it by
  roughly 4×.
- **The machine** is a 2020 MacBook Pro, 2 GHz quad-core Intel Core i5, 16 GB.

Two supporting figures matter more than the headline. **Figure 7** shows the
speedup is *asymptotic* — it grows with the cumulative number of rewrites
applied, which is what Step 6's `O(wd) → O(d)` predicts and a constant-factor
win would not. **Figure 8** correlates time spent in congruence maintenance with
the number of `repair` calls: Spearman **r = 0.98, p = 3.6e-47**. That is the
evidence that the count Step 6 reasoned about is the thing that costs time.

One divergence to hold while reading the source: the paper's Figure 4 pseudocode
has a `repair(eclass)` method and a worklist of e-classes. Current egg has
neither — `process_unions` (`egraph.rs:1346`) works from a `Vec<Id>` of **e-node**
ids (`egraph.rs:67-69`) and there is no method named `repair` in the tree. The
algorithm is the same; the names are not.

### Step 8 — e-matching is where the time goes when it is not congruence

> **In:** the read phase of Step 5.
> **Out:** the cost model for finding matches, and the index that keeps it from
> being quadratic.

egg compiles each pattern to a tiny virtual machine. Four instructions, not
three:

```rust
// egg src/machine.rs, lines 24-29 — the complete instruction set
    24  enum Instruction<L> {
    25      Bind { node: L, i: Reg, out: Reg },
    26      Compare { i: Reg, j: Reg },
    27      Lookup { term: Vec<ENodeOrReg<L>>, i: Reg },
    28      Scan { out: Reg },
    29  }
```

`Bind` walks into an e-class's matching e-nodes and pushes their children into
registers; `Compare` checks two registers canonicalize to the same class (this
is how a pattern like `x/x` enforces that both `x`s are the *same* class);
`Lookup` looks a whole ground subterm up in the hashcons in one probe; `Scan`
is the expensive one:

```rust
// egg src/machine.rs, lines 66-74 — Scan iterates every e-class in the e-graph
    66                  Instruction::Scan { out } => {
    67                      let remaining_instructions = instructions.as_slice();
    68                      for class in egraph.classes() {
    69                          self.reg.truncate(out.0 as usize);
    70                          self.reg.push(class.id);
    71                          self.run(egraph, remaining_instructions, subst, yield_fn)?
    72                      }
    73                      return Ok(());
    74                  }
```

`Scan` is `O(number of e-classes)` **per invocation**, and it recurses into the
rest of the program for each. That is why `classes_by_op` (`egraph.rs:81`) exists:
it indexes e-classes by the discriminant of the operators they contain, and the
pattern searcher consults it before falling back to a scan —

```rust
// egg src/pattern.rs, lines 300-304 — the op index short-circuits the scan
   300      fn search_with_limit(&self, egraph: &EGraph<L, A>, limit: usize) -> Vec<SearchMatches<L>> {
   304              if let Some(ids) = egraph.classes_for_op(&key) {
```

Work it: a pattern rooted at `/` in an e-graph with 10,000 e-classes of which 40
contain a `/` e-node costs 40 starting points through the index instead of
10,000 through `Scan` — **250× fewer** entries into the rest of the program. A
pattern whose root is a bare variable has no discriminant to index on and must
scan.

The budget that stops the search from running forever:

```rust
// egg src/run.rs, lines 343-345 — RunnerLimits defaults
   343              iter_limit: 30,
   344              node_limit: 10_000,
   345              time_limit: Duration::from_secs(5),
```

`check_limits` (`run.rs:170`) tests them in the order time (176), nodes (181),
iterations (185), and the loop terminates with a `StopReason` (`run.rs:237`):
`Saturated`, `IterationLimit`, `NodeLimit`, `TimeLimit`, `Other`. Saturation is
best-effort — a *search budget*, exactly like topic 10's join-order DP cutoff.
A run that stops at `NodeLimit` has not proved anything; it has run out of money.

### Step 9 — e-class analyses: a semilattice riding along with each class

> **In:** an e-graph whose classes merge unpredictably.
> **Out:** the interface for attaching derived facts, and the algebraic
> condition that makes it well-defined under merging.

An **e-class analysis** attaches a value `d_c` to every e-class. Paper §4.1
gives three operations: `make(n)` produces the value for a new e-node,
`join(d₁, d₂)` combines the values of two classes being merged, and `modify(c)`
may optionally mutate the class. The domain and `join` must form a
**join-semilattice** — `join` associative, commutative and idempotent — because
merges happen in an order the analysis author does not control, and the result
must not depend on that order.

The analysis invariant is `∀c. d_c = ⨅_{n∈c} make(n)` and `modify(c) = c`.

In egg, `analysis_pending` (`egraph.rs:70`) is a `UniqueQueue` — deduplicating,
like the congruence worklist — and the second loop of `process_unions`
(`egraph.rs:1360-1371`) drains it, calling `N::remake`, `analysis.merge`, and
`N::modify` on any class whose data actually changed.

The canonical instance is constant folding: the value is `Option<i64>`, `join`
is "agree or panic", and `modify` adds the literal e-node to the class when the
value becomes `Some`. Our `eqsat.rs` stub sidesteps this — `(/ 2 2)` folds via
the `div-same` rewrite instead — but the M21 planner stage is the interesting
version: carry **cardinality estimates** as the analysis and topic 10's
`estimate()` becomes a lattice value that merges when two plan alternatives are
proved equivalent.

### Step 10 — extraction is where the guarantees stop

> **In:** a saturated (or budget-stopped) e-graph.
> **Out:** one term, and a clear statement of which cost functions this can and
> cannot optimise.

```rust
// egg src/extract.rs, lines 157-166 — AstSize, the default cost function
   157  pub struct AstSize;
   164          enode.fold(1, |sum, id| sum.saturating_add(costs(id)))
```

`Extractor::find_best` (`extract.rs:225`) reads a table built by `find_costs`
(`extract.rs:254`), which is a **fixpoint**: repeatedly recompute each class's
best cost from its e-nodes' children's current best costs until nothing changes.

This is optimal for a **local, tree-shaped** cost function — one where a term's
cost is a function of its node and its children's costs, and shared subterms are
paid for once per use. `AstSize` is exactly that: `1 + Σ children`. It is
**not** optimal under a DAG cost, where a subterm used twice should be priced
once; the paper's §4.3 notes that extraction with a local cost function can
itself be phrased as an e-class analysis, and points at other work (Wang et al.
2020; Wu et al. 2019) for the harder objectives. egg ships an ILP-based
extractor in `src/lp_extract.rs` for that case.

The planner analogy is the reason this topic sits where it does: greedy
extraction is picking the cheapest subplan per group in a memo, which is what a
Cascades optimizer does. An e-graph *is* a Cascades memo discovered
independently — with congruence, which Cascades lacks, and without physical
properties and enforcers, which Cascades has (question 5).

## How to read the paper (with the concepts in hand)

Willsey et al., *egg: Fast and Extensible Equality Saturation*, POPL 2021,
arXiv:2004.03082.

- **§2.1** — Definitions 2.1–2.7. Read Definition 2.6 and 2.7 slowly; they are
  Step 4 and every later argument depends on them.
- **§2.2** — one page, and it contains our trap verbatim plus Figure 2, the
  four-panel walkthrough Step 3 counted. Read Figure 2's caption; the phrase
  "representing infinitely many expressions" is the point of the whole topic.
- **§3** — the contribution. §3.2.1's two worked examples are Step 6; §3.2.2
  proves termination by a lexicographic decrease of `(|I|, |W|)`; §3.4 with
  Figures 6–8 is Step 7. Check Figure 6's caption for what the baseline is
  before you quote the number.
- **§4** — e-class analyses (Step 9). §4.3's extraction paragraph is Step 10.
- **§5** — the implementation. Note the paper's own size claim: egg is "~5000
  lines of Rust, including code, tests, and documentation."
- **§6** — three case studies (6.1 Herbie, 6.2 Spores, 6.3 Szalinski). Skim
  unless you want evidence that the library carried real work.

## Where each step lives in the code

Pinned at `egraphs-good/egg@f94c346`. Sizes are that commit's.

| file:line | step | what |
|---|---|---|
| `unionfind.rs:30` | 1 | `find` — walks to root, **no** compression |
| `unionfind.rs:37` | 1 | `find_mut` — path **halving** (grandparent, line 40) |
| `unionfind.rs:47` | 1 | `union` — unconditional, `root1` wins. 93-line file, 51 lines of impl |
| `egraph.rs:66` | 2 | `memo: HashMap<L, Id>` — the hashcons H; stale between rebuilds (63-64) |
| `egraph.rs:69` | 2, 6 | `pending: Vec<Id>` — **e-node** ids, not class ids |
| `egraph.rs:78` | 2 | `classes` — the e-class map M |
| `egraph.rs:88` | 4 | `clean` — do the invariants currently hold? |
| `egraph.rs:970` | 2 | `EGraph::add` — canonicalize children, memo lookup-or-insert |
| `egraph.rs:1147` | 5 | `EGraph::union` — the public merge |
| `egraph.rs:1170-1175` | 1 | the leader choice: fewer parents wins |
| `egraph.rs:1190` | 6 | `pending.extend(class2.parents…)` — the deferral, one line |
| `egraph.rs:1346` | 6 | `process_unions` — drain, re-canonicalize, memo collision ⇒ congruence |
| `egraph.rs:1416` | 6 | `rebuild` — `process_unions` + `rebuild_classes`, then `clean = true` |
| `egraph.rs:184` | 7 | `total_size()` = `memo.len()`, the node count `node_limit` watches |
| `machine.rs:24-28` | 8 | `Bind`, `Compare`, `Lookup`, `Scan` — the complete instruction set |
| `machine.rs:66-74` | 8 | `Scan` — `for class in egraph.classes()`, the quadratic risk |
| `pattern.rs:300-304` | 8 | `classes_for_op` short-circuit |
| `run.rs:525` | 5 | `run_one` — search (556), apply (573), one `rebuild` (595) |
| `run.rs:343-345` | 8 | defaults: 30 iterations, 10,000 nodes, 5 s |
| `run.rs:237` | 8 | `StopReason` |
| `extract.rs:157` | 10 | `AstSize` — `1 + Σ children` |
| `extract.rs:254` | 10 | `find_costs` — the fixpoint |

Navigation advice: read `unionfind.rs` fully (it is 93 lines and you have
already seen 21 of them above), then `egraph.rs` by the anchors, then `run.rs`'s
`run_one`, then `extract.rs`. Skip `explain.rs` on the first pass — at 1962
lines it is the largest file in the tree and it is orthogonal to everything
here.

## Questions (answer in notes.md)

1. Trace `(a*2)/2` by hand through iteration 1 of a saturating run: which
   `merge` calls happen, in which e-class do `(/ 2 2)` and `1` meet, and how
   many terms does the root class represent after each of Figure 2's panels?
   (Step 3 did b–d; do 2a and check.)
2. `process_unions` (`egraph.rs:1349`) loops until `pending` is empty. Give a
   concrete four-e-node example where one repair creates a *new* hashcons
   collision, so a single pass would leave congruence broken.
3. Take an e-graph with 10,000 e-classes, 40 of which contain a `/` e-node.
   Compute the starting points for a `/`-rooted pattern with and without
   `classes_by_op`. Now do it for a pattern rooted at a bare pattern variable —
   what changes, and why?
4. Associativity plus commutativity on `+` alone, applied to a depth-8 sum:
   estimate e-node growth per iteration, then predict which of the three
   `RunnerLimits` (30 iterations, 10,000 nodes, 5 s) trips first. Then measure
   it in the stub.
5. Cascades memo against e-graph: name one thing Cascades has that egg lacks
   (start with physical properties and enforcers) and one thing egg has that
   Cascades lacks (start with congruence). Which of the two would our M21
   planner need first?
6. Step 7 says the 88× baseline is egg-with-eager-rebuild on egg's own 32-test
   suite. Name one way that choice of baseline flatters the result, and one way
   it is *more* honest than comparing against a different tool.

## Done when

Answer each before unfolding it.

- [ ] You can state both e-graph invariants precisely, and say which one implies that an e-node lives in exactly one e-class.

  <details><summary>Answer</summary>

  **Congruence** (Definition 2.6): the equivalence over e-nodes is closed under
  congruence, `(≡node) = (≅*)` — if `x ≡ y` then `f(x) ≡ f(y)`. **Hashcons**
  (Definition 2.7): `n ∈ M[a] ⟺ H[canonicalize(n)] = find(a)`, where
  `canonicalize(f(a₁,…)) = f(find(a₁),…)`.

  Uniqueness follows from **congruence**, and the paper spells out why:
  identical e-nodes are trivially congruent, so if congruence holds they must be
  in the same e-class. Deduplication is not an extra rule.

  </details>

- [ ] You can count the terms an e-graph represents, and produce the number for each panel of the paper's Figure 2.

  <details><summary>Answer</summary>

  The rule from Definition 2.3: a class represents `Σ over its e-nodes` of
  `Π over that e-node's children` of the child class's count.

  Figure 2a: 4 e-nodes, **1** term. 2b (after `x×2 → x≪1`): the mul class holds
  `*` and `<<`, each contributing `1×1`, so the root `/` class is `2×1` = **2**
  terms from 5 e-nodes. 2c (after `(x×y)/z → x×(y/z)`): the root gains a `*`
  e-node, `(/: 2×1) + (*: 1×1)` = **3** terms from 7 e-nodes. 2d (after
  `x/x → 1`, `1×x → x`): the class holding `a` now contains a `*` e-node with
  itself as a child — a cycle — so the sum diverges: **infinitely many** terms
  from 9 e-nodes. The paper's Figure 2d caption says exactly this.

  </details>

- [ ] You can explain deferred rebuilding, and show on numbers why it is asymptotically better rather than a constant-factor win.

  <details><summary>Answer</summary>

  `perform_union` sets `clean = false` (`egraph.rs:1159`) and pushes the merged
  class's parents onto `pending` (`egraph.rs:1190`), then returns. Repair happens
  later, in `process_unions` (`egraph.rs:1346`), where each pending e-node is
  re-canonicalized (1352) and re-inserted into `memo` (1353); a returned old
  value means two e-nodes became congruent, so they are unioned (1355) — which
  refills `pending`, hence the loop at 1349.

  Paper §3.2.1's second workload: `w` terms nested `d` deep, `w−1` merges of the
  leaves. Eager: `O(d)` repairs per merge, `O(wd)` total. Deferred: all merges
  land first, the deduplicated worklist holds one class per *layer*, so `O(d)`
  total. At `w = 100, d = 10` that is ~1000 repairs against ~10. The saved
  factor is `w`, the *width* — it grows with the workload, which is why
  Figure 7 shows the speedup growing with cumulative rewrites instead of
  flattening.

  The correctness condition is Step 5's phase split: the hashcons is stale
  between rebuilds (`egraph.rs:63-64`), and that is safe only because search is
  read-only and apply is write-only, with one `rebuild()` between them
  (`run.rs:595`).

  </details>

- [ ] You can quote egg's headline speedup with its baseline, benchmark and caveat, and say what the *end-to-end* number is.

  <details><summary>Answer</summary>

  §3.4, Figure 6: a **geometric mean over 32 tests** of egg's own `math` and
  `lambda` test suites, on a 2020 MacBook Pro (2 GHz quad-core i5, 16 GB).
  **88× on congruence closure**, **21× on the whole equality-saturation
  algorithm** — the end-to-end figure is 21×, not 88×. The baseline is **egg
  itself, modified to call `rebuild` after every merge**, not a competing tool.
  Eight of the 32 tests hit the iteration limit of 100 rather than saturating.

  Figure 7 shows the speedup is asymptotic in cumulative rewrites; Figure 8
  correlates congruence time with `repair` call count at Spearman r = 0.98,
  p = 3.6e-47.

  </details>

- [ ] You can name the four e-matching instructions and compute what the `classes_by_op` index saves.

  <details><summary>Answer</summary>

  `Bind`, `Compare`, `Lookup`, `Scan` (`machine.rs:24-28`). `Bind` enters an
  e-class's matching e-nodes; `Compare` (75-78) checks two registers have the
  same canonical class, which is how `x/x` requires the *same* `x`; `Lookup`
  probes the hashcons for a whole ground subterm; `Scan` (66-74) iterates
  **every** e-class via `for class in egraph.classes()`.

  With 10,000 e-classes of which 40 hold a `/` e-node, `classes_for_op`
  (`pattern.rs:304`) gives 40 starting points instead of 10,000 — 250× fewer
  entries into the rest of the program. A pattern rooted at a bare variable has
  no operator discriminant, so the index cannot help and `Scan` runs.

  </details>

- [ ] You can say what an e-class analysis requires algebraically, and why that requirement exists.

  <details><summary>Answer</summary>

  §4.1: `make(n)`, `join(d₁,d₂)`, `modify(c)`, with the domain and `join`
  forming a **join-semilattice** — associative, commutative, idempotent. The
  reason is that merges arrive in an order the analysis author does not control
  and can repeat (a class may be merged many times before a rebuild), so the
  accumulated value must be independent of order and of repetition. The
  invariant is `d_c = ⨅_{n∈c} make(n)` and `modify(c) = c`. egg drains
  `analysis_pending` — a `UniqueQueue` (`egraph.rs:70`) — in the second loop of
  `process_unions` (1360-1371).

  </details>

- [ ] You can say which cost functions extraction optimises correctly and which it does not.

  <details><summary>Answer</summary>

  `find_costs` (`extract.rs:254`) is a fixpoint over per-class best cost, and
  `find_best` (`extract.rs:225`) reads the resulting table. This is optimal for a
  **local tree cost** — cost of a node is a function of the node and its
  children's costs, sharing paid per use. `AstSize` (`extract.rs:157`, cost at
  164) is `1 + Σ children`, exactly that shape.

  It is **not** optimal for a DAG cost, where a subterm referenced twice should
  be charged once; nor for any objective that is not decomposable per class
  (e.g. a global register-pressure or code-size budget). §4.3 notes extraction
  can be expressed as an e-class analysis and cites other work for the harder
  objectives; egg ships `src/lp_extract.rs` for an ILP formulation.

  </details>

- [ ] You can connect the paper's §2.2 to this topic's measured lane, in both directions.

  <details><summary>Answer</summary>

  §2.2's sentence — applying `(𝑎 × 2)/2 → (𝑎 ≪ 1)/2` "prevents us from canceling
  out `2/2`" — is the specification of our failure. `hand.rs` tries R2
  (`x*2 → x<<1`) before R4 (`(x*y)/z → x*(y/z)`), R2 fires, and R4 can never
  match again. Measured: `(a << 1) / 2`, **cost 5, 1 rule firing**.

  In the other direction, the e-graph run of Figure 2b–2d shows why saturation
  escapes it: `a*2` and `a<<1` sit in one e-class, so R4 still matches the `*`
  e-node, `2/2` folds to `1`, `1×a` folds to `a`, and `a` ends up in the root
  class at cost 1. The whole difference is that `merge` adds an alternative
  where `hand.rs` performs a replacement.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the associativity-plus-commutativity growth estimate and which `RunnerLimit` you predicted.

  <details><summary>Answer</summary>

  The prediction worth writing down before measuring: commutativity alone at
  most doubles the e-nodes at each `+`; associativity on a depth-8 right-nested
  sum generates the distinct parenthesisations, which is Catalan-like growth in
  the number of leaves. With defaults of 30 iterations, 10,000 nodes and 5 s
  (`run.rs:343-345`), `NodeLimit` is the plausible first trip for a depth-8 sum,
  because node count grows per-iteration while iterations are capped at 30 and
  the per-iteration work is still small enough to stay inside 5 s early on.
  Whatever you predict, record it before running — the point of the worksheet in
  `notes.md` is the gap between the prediction and the measurement, and a run
  that stops at `NodeLimit` has proved nothing about saturation.

  </details>

## References

**Papers**
- Max Willsey, Chandrakana Nandi, Yisu Remy Wang, Oliver Flatt, Zachary Tatlock,
  Pavel Panchekha — *egg: Fast and Extensible Equality Saturation*, POPL 2021
  ([arXiv:2004.03082](https://arxiv.org/abs/2004.03082)). §2.1 Definitions
  2.1–2.7; §2.2 the phase-ordering paragraph and Figure 2; §3 rebuilding, with
  §3.2.1's two worked workloads and §3.2.2's termination proof; §3.4 and
  Figures 6–8 the measurement; §4 e-class analyses; §4.3 extraction.
- Greg Nelson — *Techniques for Program Verification*, Stanford PhD thesis,
  1980, Chapter 7 — the upward-merging congruence closure that §3.2.2's proof
  reduces to.
- Leonardo de Moura, Nikolaj Bjørner — *Efficient E-Matching for SMT Solvers*,
  CADE 2007 — the e-matching procedure `machine.rs` implements. Read alongside
  `reading-z3-tacas08.md`.

**Code** — `egraphs-good/egg` at `f94c346`

| File | Lines | What |
|------|-------|------|
| `src/unionfind.rs` | 93 | U. Impl is lines 1–51; read it whole |
| `src/egraph.rs` | 1511 | M, H, `add`, `union`, `process_unions`, `rebuild` |
| `src/machine.rs` | 345 | the e-matching VM |
| `src/pattern.rs` | 536 | patterns, and the `classes_for_op` short-circuit |
| `src/run.rs` | 994 | `Runner`, the three-phase loop, limits, `StopReason` |
| `src/extract.rs` | 315 | `Extractor`, `CostFunction`, `AstSize` |
| `src/rewrite.rs` | 703 | `Rewrite`, `Searcher`, `Applier` |
| `src/language.rs` | 1001 | `Language`, `define_language!` — what `eqsat.rs` uses |
| `src/explain.rs` | 1962 | proof production. Skip on the first pass |

**In this topic**
- `experiments/src/hand.rs` — the ordered rewriter whose rule order is the trap;
  the test `the_ordering_trap` asserts the cost-5 answer.
- `experiments/src/eqsat.rs` — the stub you fill in, with the rewrite list in
  its module docs.
- `notes.md` — the measured baseline these guides quote.
