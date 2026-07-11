# The Rust planner stack: Pratt parsing, rule traits, lazy frames

Three codebases, three Rust-shaped answers: sqlparser-rs (the parser
you'll use directly in the experiments), DataFusion's rules-as-a-trait
optimizer, and polars' rewrites-only lazy frames. M10's Cypher planner
will face every design choice DataFusion made — read for the shapes,
not the SQL details.

## 1. sqlparser-rs — Pratt parsing (src/parser/mod.rs)

- Entry: `parse_sql` :582 → `parse_statements` :531 → `parse_statement`
  :626 — a hand-written recursive-descent parser (no parser generator;
  same choice as postgres' gram.y ≠, DuckDB's libpg_query, and most
  production systems that started generated and went manual for error
  messages).
- The heart: `parse_subexpr` :1428–1450 — **Pratt / precedence-climbing**
  expression parsing: parse a prefix, then loop "while
  `get_next_precedence` (:1449) > my precedence, consume infix". This is
  the 30-line answer to expression grammars that would take 40 grammar
  rules; steal it for Cypher expressions in M10.

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

- Note the `Dialect` trait plumbing — one AST, many SQLs; the AST types
  in `src/ast/` are the de-facto Rust standard (DataFusion consumes them
  directly).

## 2. DataFusion optimizer — rules as a trait (optimizer/src/optimizer.rs)

- `OptimizerRule` :83 — `rewrite(&self, plan, config) ->
  Transformed<LogicalPlan>` (:135): every pass is this trait; the
  `Transformed` wrapper tracks "did anything change".
- The driver (`optimize` :581): run ALL rules in order, REPEAT up to
  `max_passes` (:604, default 3) or until a full pass changes nothing —
  a fixpoint loop, where DuckDB runs each pass once in a hand-tuned
  order. Trade: no pass-ordering cleverness needed / passes must be
  idempotent-ish and you pay repeated traversals.
- Skim the rule files: `push_down_filter.rs`, `eliminate_cross_join.rs`,
  `extract_equijoin_predicate.rs`, `decorrelate_predicate_subquery.rs` —
  the same rewrite menu as DuckDB §2, one file per rule, unit-testable
  in isolation (each file's bottom half is tests — the payoff of
  rule-as-trait).

## 3. polars lazy frames (crates/polars-plan/src/plans/optimizer/)

- A DATAFRAME library with a query optimizer: `.lazy()` builds an IR
  plan; `.collect()` optimizes + executes. The dir reads like a mini
  DuckDB: `predicate_pushdown/`, `projection_pushdown/`,
  `simplify_expr/`, `cse/`, `collapse_and_project.rs`,
  `delay_rechunk.rs`.
- What's MISSING is the lesson: no cost-based join reordering to speak of
  — dataframe programs mostly encode the join order the user wrote.
  Rewrites-only optimization is viable when the API hands you an
  explicit plan. (M10 corollary: Cypher gives no such luck — MATCH
  patterns NEED cost-based anchor/expansion choice.)

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
