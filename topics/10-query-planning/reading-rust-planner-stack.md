# The Rust planner stack: Pratt parsing, rule traits, lazy frames

Three codebases, three Rust-shaped answers: sqlparser-rs (the parser
you'll use directly in the experiments), DataFusion's rules-as-a-trait
optimizer, and polars' rewrites-only lazy frames. M10's Cypher planner
will face every design choice DataFusion made. Before the code, this
chapter builds the five ideas these codebases embody — recursive-descent
parsing, Pratt expression parsing, rules as a trait, fixpoint driving,
and rewrites-only optimization — then maps each to its file:line. Read
for the shapes, not the SQL details.

## The problem in one sentence

Between "a string of SQL" and "a plan the executor can run" sit three
design decisions — how to parse expressions without 40 grammar rules,
how to organize dozens of rewrite rules so they stay testable, and when
you can skip cost-based planning entirely — and each of these three
codebases answers one of them well.

## The concepts, step by step

### Step 1 — parsing: text to AST, by hand

A **parser** turns the query string into an **AST** (abstract syntax
tree — a tree of typed nodes mirroring the query's structure: a
SELECT node holding a list of expression nodes, a FROM node, and so on).
The two ways to build one: feed a grammar to a **parser generator**
(a tool that emits parser code from grammar rules), or write a
**recursive-descent** parser by hand — one function per grammar
construct, each consuming tokens and calling the functions for its
sub-constructs. sqlparser-rs is hand-written recursive descent, and this
is the norm, not the exception: postgres's gram.y aside, DuckDB
(libpg_query) and most production systems that started generated went
manual — because hand-written parsers give precise, human error messages
("expected ON after JOIN near line 3"), and parse errors are a database's
single most user-facing surface. sqlparser-rs also threads a `Dialect`
trait through every decision point — one AST, many SQLs — and its
`src/ast/` types are the de-facto Rust standard (DataFusion consumes
them directly).

### Step 2 — Pratt parsing: precedence climbing in 30 lines

Expressions are the part of a grammar where recursive descent gets ugly:
`a + b * c > d AND e` must parse as `((a + (b*c)) > d) AND e`, and
encoding "* binds tighter than +" as grammar rules takes one rule per
precedence level — ~40 rules for SQL. **Pratt parsing** (also called
precedence climbing) replaces them all with one loop and a precedence
table: parse a prefix (literal, identifier, unary op, parenthesized
expression), then repeatedly ask "does the next token bind tighter than
the operator that called me?" — if yes, consume it and recurse with the
new precedence:

```rust
fn parse_subexpr(&mut self, min_prec: u8) -> Expr {
    let mut lhs = self.parse_prefix();          // literal, ident, unary, (…)
    loop {
        let prec = self.get_next_precedence();  // 0 if next isn't infix
        if prec <= min_prec { return lhs; }     // caller binds tighter: stop
        let op = self.next_token();
        let rhs = self.parse_subexpr(prec);     // recurse with MY precedence:
        lhs = Expr::binary(lhs, op, rhs);       // higher-prec ops bind first,
    }                                           // left-assoc falls out of <=
}
```

This is the 30-line answer to expression grammars that would take 40
grammar rules; steal it verbatim for Cypher expressions in M10 — you
only write the precedence table.

### Step 3 — rewrite rules as a trait: one file, one rule, one test suite

An optimizer is a pile of **rewrite rules** (plan transformations that
are always safe — pushdown, constant folding; see the DuckDB guide). The
organizational question is how to keep 30+ of them from becoming one
giant pass. DataFusion's answer: every rule is an implementation of one
trait —

```rust
trait OptimizerRule {
    fn rewrite(&self, plan: LogicalPlan, config: &dyn OptimizerConfig)
        -> Result<Transformed<LogicalPlan>>;
}
```

— where the `Transformed<LogicalPlan>` wrapper carries a flag recording
"did anything actually change". The payoff is structural: one file per
rule (`push_down_filter.rs`, `eliminate_cross_join.rs`,
`extract_equijoin_predicate.rs`, `decorrelate_predicate_subquery.rs` —
the same rewrite menu as DuckDB's pipeline), each unit-testable in
isolation — each file's bottom half *is* its tests. The cost: rules
can't see each other, so cooperation must happen through the driver.

### Step 4 — the fixpoint driver: repeat until nothing changes

Given independent rules, who decides the order? DataFusion's driver runs
ALL rules in sequence, then repeats the whole sequence up to `max_passes`
times (default 3) or until a full pass reports no change — a **fixpoint
loop** (iterate until the output stops changing). Compare DuckDB: each
pass runs exactly once, in a hand-tuned order (pullup deliberately before
pushdown). The trade:

```
 fixpoint of all rules (DataFusion)     once, in order (DuckDB)
 ──────────────────────────────────    ─────────────────────────
 no ordering cleverness needed          order encodes expert knowledge
 catches rule-enables-rule chains       misses them unless ordered right
 pays repeated plan traversals          one traversal per pass
 rules must be idempotent-ish           rules may assume predecessors ran
```

The `Transformed` flag is what makes the fixpoint terminate — a rule that
always reports "changed" spins the driver to max_passes every time
(question 4 below).

### Step 5 — rewrites-only optimization: what polars gets away with

polars is a dataframe library with a real query optimizer hiding inside:
`.lazy()` builds a plan IR instead of executing eagerly, `.collect()`
optimizes then executes. Its optimizer directory reads like a mini
DuckDB — `predicate_pushdown/`, `projection_pushdown/`,
`simplify_expr/`, `cse/`, `collapse_and_project.rs`,
`delay_rechunk.rs` — but what's MISSING is the lesson: no cost-based
join reordering to speak of. It can skip it because a dataframe program
*is* an explicit plan — the user already wrote the join order, method
call by method call. Rewrites-only optimization is viable exactly when
the API hands you the order. The M10 corollary: Cypher gives no such
luck — a MATCH pattern names *relationships*, not an order, so pattern →
expansion order is a genuine cost-based choice (anchor selection). A
FalkorDB planner cannot be polars.

## Where each step lives in the code

- **Step 1 — sqlparser-rs** (`src/parser/mod.rs`): entry `parse_sql`
  :582 → `parse_statements` :531 → `parse_statement` :626 — the
  hand-written recursive descent. The `Dialect` trait plumbing is
  threaded throughout; AST types in `src/ast/`.
- **Step 2 — the heart**: `parse_subexpr` :1428–1450 with
  `get_next_precedence` :1449 — match the Rust sketch above against the
  real thing.
- **Steps 3–4 — DataFusion** (`optimizer/src/optimizer.rs`):
  `OptimizerRule` :83, its `rewrite` returning `Transformed<LogicalPlan>`
  (:135); the driver `optimize` :581 with `max_passes` :604. Then skim
  the one-file-per-rule menu: `push_down_filter.rs`,
  `eliminate_cross_join.rs`, `extract_equijoin_predicate.rs`,
  `decorrelate_predicate_subquery.rs`.
- **Step 5 — polars** (`crates/polars-plan/src/plans/optimizer/`): read
  the directory listing as much as the code — `predicate_pushdown/`,
  `projection_pushdown/`, `simplify_expr/`, `cse/`,
  `collapse_and_project.rs`, `delay_rechunk.rs` — and note what isn't
  there.

## Questions for notes.md

1. Trace `a + b * c > d AND e` through parse_subexpr by hand (precedence
   table lookups included). Now write the Cypher expression subset you
   need for M10 and its precedence table.
2. DataFusion's fixpoint-of-all-rules vs DuckDB's once-in-order: which
   catches `filter → (rewrite exposes new filter) → filter` chains, and
   what's the worst-case cost?
3. Why can polars skip join reordering but FalkorDB can't? Where exactly
   does Cypher hide the join order decision (pattern → expansion order)?
4. The `Transformed` flag: why does a fixpoint driver need rules to
   report changes honestly — what breaks with a rule that always says
   "changed"?

## Done when

You can parse an expression with Pratt precedence on paper, and argue
rules-as-trait-with-fixpoint vs ordered-pass-pipeline for M10 (pick one,
justify in notes.md).

## References

**Code**
- [sqlparser-rs](https://github.com/apache/datafusion-sqlparser-rs) —
  `src/parser/mod.rs` (parse_subexpr is the heart), `src/ast/`
- [datafusion](https://github.com/apache/datafusion) —
  `optimizer/src/optimizer.rs` (OptimizerRule trait + fixpoint driver),
  then skim the one-file-per-rule menu
- [polars](https://github.com/pola-rs/polars) —
  `crates/polars-plan/src/plans/optimizer/` — read the directory listing
  as much as the code; what's MISSING is the lesson
