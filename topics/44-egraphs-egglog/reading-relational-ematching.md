# Relational e-matching: the pattern is a query, the e-graph is the database

Topic 21's guide to egg ends on a number: e-matching, not congruence
closure, is where equality saturation spends its time. This paper —
Zhang, Wang, Willsey and Tatlock, **"Relational E-matching"**, POPL 2022
(arXiv:2108.02290) — is the one that reads that sentence as a database
problem and answers it with an algorithm from our literature. Its whole
argument fits in one substitution: an e-graph is a set of tables, a
pattern is a conjunctive query, and *the equality constraint that
backtracking checks last is a join key*.

This chapter builds every term it needs — e-matching, substitution,
linear pattern, conjunctive query, atom, fractional edge cover, the AGM
bound, generic join — from nothing, then applies the paper's two
theorems to the numbers this topic's own bench prints.

Every paper claim below names the section, figure, table or theorem it
came from. Code anchors are this topic's crate
(`topics/44-egraphs-egglog/experiments/src/…`) and `egraphs-good/egg` at
the commit `resources/codebases.md` pins.

## The problem in one sentence

Matching the pattern `f(a, g(a))` against an e-graph that holds N `f`
e-nodes and N `g` e-nodes takes a backtracking matcher N² + N + 1 steps
to produce N answers, because it can only check that the two `a`s agree
*after* it has built each candidate — and a join algorithm checks that
first, for the same reason a database never computes a cross product
and filters it.

## The concepts, step by step

### Step 1 — e-matching, stated precisely

> **In:** an e-graph, as topic 21's guide built it. **Out:** the exact
> statement of what e-matching returns, and the two words the rest of
> this chapter leans on — *substitution* and *root*.

A quick restatement of the structure, because the definitions have to be
exact for the counting to mean anything (paper Definitions 4 and 5):

- An **e-node** is a pair `(f, [i₁…i_k])`: a function symbol and a list
  of e-class ids. `f(i₁…i_k)` is shorthand for it.
- An **e-class** is a set of e-nodes, identified by one or more ids. All
  the e-nodes in one e-class are asserted to be equal.
- A **union-find** stores which ids are equal; `find(i)` returns the
  class's **canonical** id — the single id that stands for it.
- A **term** is what you write on paper: `f(3, g(3))`. An e-class
  **represents** a term if some e-node in it does, recursively
  (Definition 6).

A **pattern** is a term with **pattern variables** in it — `f(a, g(a))`,
where `a` may stand for any e-class. A pattern with no variable
occurring twice is called **linear**; `f(a, g(b))` is linear and
`f(a, g(a))` is not. That distinction will turn out to be the entire
performance story.

An **e-matching substitution** σ maps every variable in the pattern to
an e-class (Definition 7). E-matching (Definition 8) returns the set of
pairs `(σ, r)` such that every term in `σ(p)` is represented in e-class
`r`; `r` is called the **root** of the match. So a match is *not* a
term — it is an assignment of e-classes to variables, plus the class
the matched terms live in. That is what makes the output small even
when the term set is enormous.

### Step 2 — the e-graph that makes it hard

> **In:** the definitions of Step 1. **Out:** the concrete e-graph both
> the paper and this topic's bench measure on, with its two sizes — how
> big it is, and how many terms it stands for.

Paper Figure 2. Fix N. The e-graph has:

```
   e-class  1 … N     one constant e-node each:  1, 2, … N
   e-class  i_g       N e-nodes:  g(1), g(2), … g(N)
   e-class  i_f       N e-nodes:  f(1,i_g), f(2,i_g), … f(N,i_g)
```

That is **3N e-nodes** in **N + 2 e-classes**. Now count the terms it
represents. Every `f` e-node's second child is `i_g`, and `i_g`
represents N different terms, so each of the N `f` e-nodes stands for N
terms:

```
   constants          N
   g-terms            N          g(1) … g(N)
   f-terms            N × N      f(i, g(j)) for every i, j
   ────────────────────────
   total              N² + 2N
```

At N = 1600 that is 4,800 e-nodes representing 2,563,200 terms. **This
is the property that makes e-graphs worth having and e-matching hard:
the structure is linear, the thing it denotes is quadratic.** Any
algorithm that enumerates terms has already lost; the question is
whether an algorithm that walks the structure can avoid enumerating them
implicitly.

`gen.rs::Fig2` builds exactly this graph, and lane 1's `e-nodes` column
is 3N in every row — the check that it did.

### Step 3 — two kinds of constraint, and the one backtracking defers

> **In:** the Figure 2 e-graph of Step 2 and the pattern `f(a, g(a))`.
> **Out:** the paper's classification of what a pattern demands, which
> is the diagnosis the whole paper rests on (§2.1).

Matching `f(a, g(a))` demands three things of a candidate term `t`
(paper §2.1 lists them in this order):

1. `t`'s symbol is `f`;
2. `t`'s second child's symbol is `g`;
3. `t`'s first child is equivalent to the child of `t`'s second child.

The paper splits these into two kinds:

- **Structural constraints** come from the *shape* of the pattern —
  which symbol sits where. Constraints 1 and 2.
- **Equality constraints** come from a variable occurring more than
  once: the positions it occupies must be the same e-class. Constraint
  3. A pattern with none of these is **linear**.

Now the diagnosis, in the paper's words (§2.1): "Backtracking search
exploits the structural constraints first and defers checking the
equality constraints to the end." A top-down walk cannot do otherwise.
It reaches the `f` e-node, takes the first child as a candidate binding
for `a`, then must walk into the second child's e-class to find a `g`
before it has anything to compare against. By then the candidate exists.

```
   pattern f(a, g(a))            what the walk must do
   ──────────────────            ─────────────────────
        f                        pick an f e-node        ← structural, usable now
       / \                       bind a := its 1st child ← nothing to check yet
      a   g                      find a g e-node         ← structural, usable now
          |                      compare its child to a  ← equality, only now
          a
```

### Step 4 — count the walk, on paper and on the machine

> **In:** the diagnosis of Step 3, the e-graph of Step 2. **Out:** a
> closed form for the work backtracking does, checked against this
> topic's measured `bt visits` column.

Paper §2.1 gives the visiting order:

```
        f(1, g(1)) → … → f(1, g(N))
    ↩→  f(2, g(1)) → … → f(2, g(N))
    ↩→  f(N, g(1)) → … → f(N, g(N))
```

and concludes: "Despite there being only N matches, backtracking search
runs in time O(N²)."

Our implementation is egg's four-instruction VM rather than the
declarative algorithm, so the constant is visible. `backtrack.rs`
compiles `f(a, g(a))` to `Scan(f)`, `Bind f`, `Bind g`, `Compare`, and
this is the `Bind` case, where the cost lives:

```rust
// backtrack.rs, lines 151-173 — Bind steps over every e-node of the right
// symbol; Compare cannot run until both registers are filled. The line to
// watch is 158, the loop, and 170, the check that comes too late.
   151         Ins::Bind {
   152             class,
   153             op,
   154             out: base,
   155             arity,
   156         } => {
   157             let c = regs[*class];
   158             for n in g.nodes(c) {
   159                 if n.op != *op || n.children.len() != *arity {
   160                     continue;
   161                 }
   162                 visits.set(visits.get() + 1);
   163                 for (i, &ch) in n.children.iter().enumerate() {
   164                     regs[base + i] = ch;
   165                 }
   166                 exec(g, rest, roots, regs, visits, out);
   167             }
   168         }
   169         Ins::Compare { a, b } => {
   170             if g.find(regs[*a]) == g.find(regs[*b]) {
   171                 exec(g, rest, roots, regs, visits, out);
   172             }
   173         }
```

Count it. `Scan` visits the one e-class containing an `f` e-node — the
op index (`egraph.rs::classes_with_op`, egg's `classes_by_op`) means it
is 1 and not N + 2. The outer `Bind f` steps over N e-nodes. For each,
the inner `Bind g` steps over N. So:

```
   visits = 1  +  N  +  N × N   =  N² + N + 1
```

N = 100 → 1 + 100 + 10,000 = **10,101**, which is exactly what lane 1
prints. N = 1600 → 2,561,601, also exact. The formula is not an
estimate; it is the number, and being able to predict a counter to the
unit is how you know the harness is measuring the algorithm rather than
the allocator.

### Step 5 — conjunctive queries, in the vocabulary a pattern needs

> **In:** nothing from the previous steps — this is the database half of
> the vocabulary, defined from scratch. **Out:** the five words Step 7
> will use to restate a pattern: relation, atom, body, head, join
> variable.

A **relation** `R` of arity k is a set of tuples of k values. A
**database** is a set of relations.

A **conjunctive query** (paper §2.2) is a query built only from select,
project and join — no union, no difference, no aggregation. It is
written:

```
   Q(x₁ … x_k)  ←  R₁(x₁,₁ … x₁,ₖ₁), … , Rₙ(xₙ,₁ … xₙ,ₖₙ)
```

- Each `Rᵢ(…)` is an **atom**: a relation name with a variable in each
  column position.
- Everything right of the arrow is the **body**; `Q(…)` is the **head**.
- A variable in the head is **free** — it comes back in the answer. A
  variable only in the body is **bound**: existentially quantified,
  projected away.
- A variable appearing in two atoms is what a database person calls a
  **join variable**, and answering the query means finding assignments
  that make every atom simultaneously present in the database.

There is no separate notion of "shape constraint" and "equality
constraint" here. Every constraint is the same kind of thing: a variable
that two atoms have to agree on. That is the whole trick, and Step 7 is
where the pattern acquires this form.

### Step 6 — the e-graph as a database, and the dependency hiding in it

> **In:** the e-graph of Step 2 and the relational vocabulary of Step 5.
> **Out:** the database the rest of the chapter queries, plus the reason
> nested patterns do not need an extra join.

Paper §3.1. For every function symbol `f` of arity k, make a relation
`R_f` of arity k+1: the first column is the e-class id **containing**
the e-node, the remaining k are its children. Every id is canonicalised
through `find` first.

```
   I = { R_f ← (find(i), find(j₁) … find(j_k))  |  M[i] = f(j₁ … j_k) }
```

Figure 2 becomes (paper Figure 7):

```
   R_f: | id  | arg1 | arg2 |        R_g: | id  | arg1 |
        | i_f |  1   | i_g  |             | i_g |  1   |
        | i_f |  2   | i_g  |             | i_g |  2   |
        |  …  |  …   |  …   |             |  …  |  …   |
        | i_f |  N   | i_g  |             | i_g |  N   |
```

`relational.rs::to_database` (line 29) is that formula, one loop long.
Two properties of the translation matter later:

- **It is linear.** One tuple per e-node, so building the database costs
  O(|E|) — "subsumed by the time complexity of most non-trivial
  e-matching patterns" (§3.1). The paper is explicit that this is
  affordable *because* e-matching happens in big batches between
  rebuilds; §6.4 lists the frequently-updated case as future work, and
  egglog is what that future work turned into.
- **Every id in it is canonical.** This is why compilation can join
  nested patterns directly on the auxiliary variable instead of adding a
  join against the equivalence relation (§3.2): for canonical ids,
  `i ≡ j` is just `i = j`.

And one that the paper flags for optimisation (§4.3): an e-graph never
contains two e-nodes with the same symbol and children, so in `R_f` the
children columns **functionally determine** the id column — a
functional dependency, in exactly the sense a schema means it. A query
planner that knows it can skip work; §4.3 is about doing so.

### Step 7 — unnesting: from pattern to conjunctive query

> **In:** the pattern of Step 3, the vocabulary of Step 5, the database
> of Step 6. **Out:** the query the join algorithm will answer, with the
> equality constraint now indistinguishable from the structural one.

Paper Figure 8 defines two functions. `Aux` returns a variable standing
for a subpattern, plus the atoms that constrain it:

```
   Aux(f(p₁ … p_k)) = v ~ R_f(v, v₁ … v_k), A₁ … A_k    where Aux(pᵢ) = vᵢ ~ Aᵢ,
                                                        and v is FRESH
   Aux(x)           = x ~ ∅                             for a pattern variable x

   Compile(p)       = Q(root, v₁ … v_k) ← atoms         where Aux(p) = root ~ atoms
```

Run it on `f(a, g(a))`, innermost decisions first:

1. `Aux(f(a, g(a)))` mints a fresh variable — call it `root` — and will
   emit `R_f(root, ?, ?)`.
2. Its first argument is the variable `a`: `Aux(a) = a ~ ∅`. No atom.
3. Its second argument is `g(a)`: mint a fresh `x`, emit `R_g(x, a)`.
4. So the body is `R_f(root, a, x), R_g(x, a)`, and the head keeps the
   root and the pattern's own variables.

```
   Q(root, a)  ←  R_f(root, a, x),  R_g(x, a)
                          │  │            │  │
                          │  └────────────┼──┘   a: the EQUALITY constraint
                          └───────────────┘      x: the STRUCTURAL constraint
```

Both are now the same syntactic object — a variable in two atoms —
which is the sentence the paper is built on (§3): "the relational
perspective sees no difference between the two kinds of constraint."

Our `pattern.rs::compile` is this, and its unit test asserts the exact
rendering (auxiliaries are printed `?0`, `?1`):

```rust
// pattern.rs, lines 171-176 — the test that pins Figure 8's output
   171         let q = compile(&[papp(f, vec![pvar("a"), papp(gg, vec![pvar("a")])])]);
   172         assert_eq!(q.atoms.len(), 2, "one atom per non-variable subpattern");
   173         let rendered = q.render(&|s| g.sym_name(s).to_string());
   174         assert_eq!(rendered, "Q(?0, a) <- R_f(?0, a, ?1), R_g(?1, a)");
   175         // `a` occurs in both atoms: the equality constraint became a join.
   176         assert_eq!(q.atoms_with(q.head[1]).len(), 2);
```

One consequence the paper draws in §1 and we exploit in lane 3:
**multi-patterns are free.** Matching several patterns that share
variables — which backtracking needs special machinery for (de Moura and
Bjørner 2007) — is just a body with more atoms.

### Step 8 — the AGM bound, worked on the triangle

> **In:** the query vocabulary of Step 5. **Out:** the number that
> bounds any conjunctive query's output, and therefore the target any
> join algorithm should hit. Nothing here is about e-graphs; Step 10
> applies it to one.

How big can a conjunctive query's answer be? Three answers of increasing
sharpness, on the **triangle query** (paper §2.3) with
`|R| = |S| = |T| = M`:

```
   Q(x, y, z) ← R(x, y), S(y, z), T(z, x)
```

1. **Cartesian product**: `|R|·|S|·|T| = M³`. Ignores the query.
2. **Any two atoms**: the answer cannot exceed `|R|·|S| = M²`, since a
   triangle is in particular an `R`-`S` pair sharing `y`.
3. **The AGM bound**: `M^1.5`, and it is *tight* — there exists a
   database achieving it (Atserias, Grohe, Marx 2008).

The third needs two definitions. The **query hypergraph** has a vertex
per variable and a hyperedge per atom; the triangle's is a triangle. A
**fractional edge cover** assigns a weight `w_i ∈ [0,1]` to each atom so
that, for every variable, the weights of the atoms containing it sum to
at least 1. The **AGM bound** is then

```
   min over covers of   Π_i |R_i|^{w_i}
```

Work it. In the triangle each variable is in exactly two atoms, so
`w = (½, ½, ½)` is a cover: for `x`, the atoms `R` and `T` contribute
½ + ½ = 1. ✓. The bound is

```
   M^½ · M^½ · M^½ = M^{3/2}
```

At M = 10,000: the cross product says 10¹², two atoms say 10⁸, AGM says
10⁶. Six orders of magnitude between the naive bound and the true one —
and a binary-join plan that materialises `R ⋈ S` first *builds* the 10⁸
before filtering it down. This is the gap worst-case optimal joins
exist to close.

### Step 9 — generic join, traced on the Figure 2 database

> **In:** the AGM bound of Step 8, the database of Step 6, the query of
> Step 7. **Out:** the algorithm, its two implementation requirements,
> and an exact probe count to compare against Step 4's N² + N + 1.

**Generic join** (paper Algorithm 1, from Ngo et al. 2014) is
variable-at-a-time, not relation-at-a-time. Given an ordering of the
query's variables, it picks the next variable `x`, computes `D_x` as the
intersection of the values every atom containing `x` allows for it, and
recurses on each survivor with `x` replaced by that value. When no
variables remain, the accumulated assignment is an answer.

Two requirements make the AGM bound hold (§2.3):

- the intersection must run in `O(min_j |R_j.x|)` — iterate the
  *smallest* participating set and probe the others, never the largest;
- a residual relation — `R(v, y)` for a fixed `v` — must be reachable in
  constant time. A **trie** gives both (Figure 5): a tree whose every
  node is a map from a value to a subtrie, with the columns ordered to
  agree with the variable ordering, so fixing a variable is one lookup.

Here is the inner loop, doing exactly that:

```rust
// relational.rs, lines 199-227 — smallest-first intersection. Line 200 picks
// the lead by map size (the O(min) requirement); line 220 is the probe into
// every other participating atom.
   199     // Intersect smallest-first, which is what buys the O(min |R_j.x|) bound.
   200     let lead = *part[..n_part]
   201         .iter()
   202         .min_by_key(|&&i| cur[i].kids.len())
   203         .expect("non-empty");
   204     let lead_trie: &'a Trie = cur[lead];
   // ... 205-214: copy the participating cursors into fixed-size scratch ...
   215     for (&v, sub) in &lead_trie.kids {
   216         probes.set(probes.get() + 1);
   217         let mut ok = true;
   218         for k in 0..n_others {
   219             probes.set(probes.get() + 1);
   220             match others[k].1.kids.get(&v) {
   221                 Some(child) => next[k] = (others[k].0, child),
   222                 None => {
   223                     ok = false;
   224                     break;
   225                 }
   226             }
   227         }
```

Now trace it on `Q(root, a) ← R_f(root, a, x), R_g(x, a)` with the
ordering `[a, x, root]` (which is what `relational.rs::plan` picks:
`a` and `x` are each in two atoms, ties broken by relation size then
variable id):

```
   level a:     D_a = R_f.arg1 ∩ R_g.arg1 = {1 … N}
                cost: N keys in the lead trie + N probes into the other  = 2N
   level x:     for each a = i:  R_f(_, i, x).x = {i_g}  ∩  R_g(x, i).x = {i_g}
                cost: 1 key + 1 probe, N times                           = 2N
   level root:  for each (i, i_g):  R_f(root, i, i_g).root = {i_f}
                only one atom participates — no intersection             = N
   ─────────────────────────────────────────────────────────────────────────
   total probes                                                          = 5N
```

N = 100 → **500**, N = 1600 → **8,000**: precisely lane 1's `gj probes`
column. Compare Step 4: 10,101 and 2,561,601. Same answers, same
machine, same accounting.

Notice what happened at the very first level. Intersecting `R_f.arg1`
with `R_g.arg1` *is* enforcing the equality constraint, before a single
candidate has been constructed. The paper's Figure 6 draws exactly this
contrast — backtracking with N² boxes and ✓/✗ marks, a hash join with N.

### Step 10 — the two theorems, applied to this topic's own lanes

> **In:** Step 8's AGM bound and Step 9's algorithm. **Out:** the paper's
> complexity results, and a check that they predict both of lane 1's
> tables — including the one where generic join loses.

First, the objection that has to be cleared. E-matching is NP-complete
(Kozen 1977), so how can anything be optimal? Because the hardness is in
the wrong parameter. Databases distinguish **query complexity** — in the
size of the query — from **data complexity** — in the size of the
database, with the query held fixed (§1). Kozen's result is about the
pattern's size. Patterns are three or four nodes; e-graphs are millions.
Hold the pattern fixed and the problem is polynomial in the e-graph.

**Theorem 9** (§3.4): relational e-matching is worst-case optimal — fix
a pattern `p`, and it runs in `O(max_E |M(p, E)|)`, the largest output
any e-graph of that size could produce.

**Theorem 10** is the one with teeth, because it is stated in the
*actual* output size rather than the worst case. For a pattern that
compiles to `Q(X) ← R₁(X₁) … R_m(X_m)`:

```
   time  =  O( √( |Q(I)| × Π_i |R_i| ) )   ≤   O( √( |Q(I)| × N^m ) )
```

The proof is worth reading (§3.4) because it is short and it explains
the shape: add an atom `C` covering the variables that appear in only
one atom; then *every* variable is in at least two atoms, so `w = ½`
everywhere is a cover, and the AGM bound of the padded query is the
square root above.

Now apply it to lane 1, where `m = 2` and `|R_f| = |R_g| = N`:

```
   lane 1a, f(a, g(a)):   |Q(I)| = N        bound = √(N · N²) = N^1.5
                          N = 1600  →  1600^1.5 = 64,000
                          measured probes: 8,000            ✓ under the bound

   lane 1b, f(a, g(b)):   |Q(I)| = N²       bound = √(N² · N²) = N²
                          N = 1600  →  2,560,000
                          measured probes: 2,561,603        ✓ AT the bound
```

The theorem predicts both rows, including the disappointing one. When
the output is quadratic, an optimal algorithm still does quadratic work
— optimality is a promise about waste, not about speed. `f(a, g(b))` is
linear, has no equality constraint, and every candidate is an answer, so
there is no waste to eliminate and generic join's more expensive
instruction (a hash lookup, ~18 ns, against a pointer walk, ~4 ns) makes
it **1.8× slower** in wall clock. That is the correct result and the
paper reports its own version of it (Step 11).

### Step 11 — what the evaluation actually claims

> **In:** the algorithm and its bounds. **Out:** an honest reading of
> §5, including the column most summaries drop.

The setup (§5.1): relational e-matching implemented *inside* egg — about
80 lines to compile patterns to conjunctive queries, plus a separate
generic-join library "in fewer than 500 lines", against egg's existing
matcher of "about 500 lines … interconnected to various other parts of
egg". Two suites, `math` and `lambda`; e-graphs grown by saturation and
stopped at four sizes; each approach run 10 times, minimum taken; single
threaded, 4.6 GHz, 32 GB.

The headline is real: "GJ can be over 6 orders of magnitude faster"
(§5.2). Table 1's `math` suite at 217,396 e-nodes, with index building
excluded, reports a best ratio of **8,575,830.58** and a median of
**80.84**.

The column to keep is **Worst**, in the same row: **0.76**. And in the
smallest `math` configuration with index building charged, 0.03 — a
pattern on which relational e-matching was 33× *slower*. §5.2 explains
it in one sentence: "Speedup tends to be greater when the output size is
smaller. A large output indicates the e-graph is densely populated with
terms matching the given pattern, therefore backtracking search wastes
little time on unmatched terms."

Two more honest details, both of which shaped what came next:

- The `+`/`−` rows are with and without **index building time**. Tries
  must be built before generic join can run, and §5.2 says the cost
  "sometimes offset[s] the gains". Amortising that is exercise 4 here —
  and it is the reason egglog stopped keeping a separate e-graph to copy
  from at all.
- The **Total** column (cumulative speedup over all patterns) does not
  grow with e-graph size, even while the best and median ratios do,
  because total time is dominated by simple linear patterns like
  `(+ (+ a b) c)` that return enormous result sets — the case where
  there is nothing to win.

## How to read the paper (with the concepts in hand)

Straight through; it is 22 pages and the middle is the good part.

1. **§1**, for the framing and the 60–90% number.
2. **§2.1** with our Step 3 open — the constraint taxonomy is the
   diagnosis, and everything after is treatment.
3. **§2.2–2.3** can be skimmed if Step 5 and Step 8 landed; do read the
   generic join walkthrough (Algorithm 2), which is our Step 9 written
   as nested loops.
4. **§3.1–3.2** are short and are the translation; §3.3 shows the
   generated program for our exact query.
5. **§3.4** — read the proof of Theorem 10. It is the only place the
   `√` shape is explained.
6. **§4** is the practical section: variable ordering (§4.1), functional
   dependencies (§4.3). Read §4.3 next to Step 6's dependency remark.
7. **§5** with Step 11 open, and look at Figure 9's log-scale spread
   rather than the headline.
8. **§6.4** is one paragraph and it is the seed of egglog: what to do
   when the e-graph changes constantly instead of in batches.

## Where each step lives in the code

| step | this crate | egg (pinned) |
|---|---|---|
| 1–2, the structure | `egraph.rs`, `gen.rs::Fig2` | `egraph.rs:970` add, `:1147` union, `:1416` rebuild |
| 3–4, the walk | `backtrack.rs:29` `Ins`, `:151` Bind, `:169` Compare | `machine.rs:24-29`, `:66-74` Scan |
| the op index | `egraph.rs::classes_with_op` | `egraph.rs:81` `classes_by_op`, `pattern.rs:300-304` |
| 6, e-graph → tables | `relational.rs:29` `to_database` | — |
| 7, Figure 8 | `pattern.rs::compile`, test at `:171` | — |
| 9, tries + generic join | `relational.rs:78` `index_atom`, `:116` `plan`, `:170` `gj` | — |
| 10, the two lanes | `bin/ematch_bench.rs::lane1` | — |

## Questions (answer in notes.md)

1. Lane 1a's `bt visits` is `N² + N + 1` and `gj probes` is `5N`, yet
   the measured speedup at N = 1600 is 21.7×, not 320×. Account for the
   difference in nanoseconds per unit, using the `index µs` column, and
   say which of the two constants you could actually reduce.
2. Theorem 10 gives `√(|Q(I)| × Π|Rᵢ|)`. Compute it for the triangle
   multi-pattern of lane 3 at V = 1600, E = 8000 (three atoms, all
   `R_e`), and compare with the measured 79,416 probes. Is the bound
   loose here, and why?
3. `relational.rs::plan` orders variables most-constrained-first. Any
   ordering is worst-case optimal — so construct a pattern and an
   e-graph where the reverse ordering does dramatically more probes, and
   explain what the good ordering knew.
4. §4.3 says the children columns functionally determine the id column.
   Name one step of the trace in Step 9 that this makes redundant, and
   estimate what fraction of that lane's probes it would remove.
5. The paper builds the database from scratch on every match (§3.1) and
   calls the cost "subsumed". At what ratio of e-graph size to match
   count does that stop being true? Use lane 2's numbers (60,000 tuples,
   24 new) to argue the case that motivated egglog.
6. Backtracking checks the equality constraint as early as it can — our
   `Compare` is emitted at the second occurrence, not at the end. Show
   that this does *not* change the asymptotics on Figure 2, and describe
   a pattern shape where it does.

## Done when

Answer each before unfolding it.

- [ ] You can state, without looking, what an e-matching substitution is
      and why the output is small even though the term set is quadratic.
  <details><summary>Answer</summary>

  A substitution σ maps each **pattern variable to an e-class**, and a
  match is the pair `(σ, r)` where `r` is the root e-class holding the
  matched terms (Definition 7, Definition 8). It is not a term. On
  Figure 2 at N = 1600 the e-graph represents N² + 2N = 2,563,200 terms,
  and `f(a, g(a))` has exactly N = 1600 matches — one per value of `a` —
  all sharing the root `i_f`. The output is indexed by *classes*, so it
  is bounded by the structure, not by what the structure denotes.
  </details>

- [ ] You can classify a pattern's constraints and predict from that
      alone whether relational e-matching will help.
  <details><summary>Answer</summary>

  Structural constraints come from the pattern's shape (which symbol
  where); equality constraints come from a variable occurring more than
  once. A pattern with no repeated variable is **linear**. Backtracking
  exploits structural constraints immediately and can only check
  equality constraints after building a candidate (§2.1), so the win is
  proportional to the candidates that the equality constraints kill.
  `f(a, g(a))`: N answers out of N² candidates — 21.7× measured.
  `f(a, g(b))`: linear, N² answers out of N² candidates, nothing to
  kill — measured **0.56×**, i.e. 1.8× slower. Predict the ratio of
  candidates to answers and you have predicted the outcome.
  </details>

- [ ] You can derive `bt visits = N² + N + 1` and `gj probes = 5N` from
      the algorithms rather than from the table.
  <details><summary>Answer</summary>

  Backtracking: one `Scan` over the single e-class holding an `f`
  e-node (the op index makes it 1, not N + 2), then `Bind f` over N
  e-nodes, then `Bind g` over N e-nodes for each of those — 1 + N + N².
  Generic join with ordering `[a, x, root]`: level `a` intersects two
  N-key tries (N keys iterated + N probes = 2N); level `x` intersects
  two singletons N times (2N); level `root` has one participating atom
  with one key, N times (N). Total 5N. At N = 100 that is 10,101 and
  500, which is what lane 1 prints.
  </details>

- [ ] You can compute the AGM bound of the triangle query and say what a
      binary plan does instead.
  <details><summary>Answer</summary>

  Every variable of `Q(x,y,z) ← R(x,y), S(y,z), T(z,x)` sits in exactly
  two atoms, so `w = (½,½,½)` is a fractional edge cover — each variable
  gets ½ + ½ = 1. The AGM bound is `Π|Rᵢ|^{wᵢ} = M^½·M^½·M^½ = M^1.5`,
  and it is tight. A binary plan must pick two atoms to join first; `R ⋈
  S` on `y` can reach `M²` tuples before `T` ever filters it. At M =
  10,000: M^1.5 = 10⁶ against an intermediate of 10⁸.
  </details>

- [ ] You can apply Theorem 10 to both of lane 1's tables and get the
      measured numbers.
  <details><summary>Answer</summary>

  `time = O(√(|Q(I)| × Π_i |R_i|))` with m = 2 atoms of N tuples each.
  Lane 1a: |Q(I)| = N, so the bound is `√(N·N²) = N^1.5` — at N = 1600,
  64,000, and 8,000 probes were measured, comfortably inside. Lane 1b:
  |Q(I)| = N², so the bound is `√(N²·N²) = N²` — 2,560,000 at N = 1600,
  and 2,561,603 probes were measured, i.e. *at* the bound. Optimality
  bounds waste, not time: with a quadratic answer, quadratic work is
  optimal and the constant decides the winner.
  </details>

- [ ] You can say what Table 1's `Worst` column means and why it is not
      an embarrassment.
  <details><summary>Answer</summary>

  `Worst` is the smallest EM/GJ ratio over the patterns in that
  configuration — 0.76 for `math` at 217,396 e-nodes without index
  building, and 0.03 for `math` at 8,205 with it, meaning generic join
  was 33× slower on some pattern. §5.2: speedup tracks how much
  backtracking wastes, and a densely matched pattern wastes nothing. It
  is the same result as this topic's lane 1b, reproduced independently,
  and it is why the useful question is "what fraction of the candidate
  space is discarded" rather than "which algorithm is faster".
  </details>

- [ ] You can explain why NP-completeness does not contradict the
      optimality claims.
  <details><summary>Answer</summary>

  Kozen's NP-completeness is stated over the size of the **pattern**;
  the paper's bounds are **data complexity**, in the size of the
  e-graph with the pattern held fixed (§1). In practice patterns have a
  handful of nodes and e-graphs have millions, so the fixed-pattern
  regime is the real one. The exponent hidden in the constant is the
  pattern's — Theorem 10's `N^m` has the atom count `m` in it.
  </details>

## References

- Yihong Zhang, Yisu Remy Wang, Max Willsey, Zachary Tatlock,
  **"Relational E-matching"**, POPL 2022, arXiv:2108.02290. Figure 2
  (the e-graph), §2.1 (the constraint taxonomy), Figure 3 (backtracking),
  §2.3 (AGM, generic join), §3.1 (e-graph → database), Figure 8
  (unnesting), §3.4 (Theorems 9 and 10), §4.3 (functional dependencies),
  §5 (evaluation, Table 1).
- Max Willsey et al., **"egg: Fast and Extensible Equality
  Saturation"**, POPL 2021, arXiv:2004.03082 — the e-graph and the
  60–90% measurement. Read
  [topic 21's guide](../21-formal/reading-egg-popl21.md) first.
- Hung Q. Ngo, Christopher Ré, Atri Rudra, **"Skew Strikes Back: New
  Developments in the Theory of Join Algorithms"**, SIGMOD Record 2013 —
  generic join and the AGM bound, if you want the database-side
  treatment.
- Albert Atserias, Martin Grohe, Dániel Marx, **"Size Bounds and Query
  Plans for Relational Joins"**, FOCS 2008 — the AGM bound itself.
- Next in this topic: [reading-egglog-pldi23.md](reading-egglog-pldi23.md),
  which starts from §6.4's open problem.
