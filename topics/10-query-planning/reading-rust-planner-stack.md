# The Rust planner stack: Pratt parsing, rule traits, lazy frames

Three codebases, three Rust-shaped answers: sqlparser-rs (the parser you'll use
directly in the experiments), DataFusion's rules-as-a-trait optimizer, and
polars' rewrites-only lazy frames. M10's Cypher planner will face every design
choice DataFusion made. Before the code, this chapter builds the five ideas
these codebases embody — recursive-descent parsing, Pratt expression parsing,
rules as a trait, fixpoint driving, and rewrites-only optimization — then maps
each to its file:line. Read for the shapes, not the SQL details.

**Every `file:line` below was read at these pins:**
`apache/datafusion-sqlparser-rs@aeb616f`, `apache/datafusion@1e77af8`,
`pola-rs/polars@f8bcc3d`. Re-verify with
`python3 tools/pinned-source.py show <repo> <path> -r A:B` rather than trusting
the numbers — these are fast-moving crates. **Topic 10 has no measured lane**;
nothing here is a timing taken on this machine, and none of it appears in
`FINDINGS.md`. The counts below (grammar rules, file proportions, precedence
levels) are exact figures read out of the pinned source.

## The problem in one sentence

Between "a string of SQL" and "a plan the executor can run" sit three design
decisions — how to parse expressions without one grammar nonterminal per
precedence level (sqlparser-rs's table has 16 distinct levels), how to organize
dozens of rewrite rules so they stay testable, and when you can skip cost-based
planning entirely — and each of these three codebases answers one of them well.

## The concepts, step by step

### Step 1 — parsing: text to AST, by hand

> **In:** a SQL string and a `Dialect`.
> **Out:** a `Vec<Statement>` — an AST — or a `ParserError` with a position.
> This is `Parser::parse_sql`'s entire contract.

A **parser** turns the query string into an **AST** (abstract syntax tree — a
tree of typed nodes mirroring the query's structure: a `Select` node holding a
list of expression nodes, a `From` node, and so on). Note what an AST is *not*:
it is not yet a **logical plan**, which is an expression in **relational
algebra** (select, project, join, aggregate) with resolved column references.
Binding the AST against a catalog to produce a logical plan is a separate stage,
which is why sqlparser-rs can be a standalone crate at all.

The two ways to build a parser: feed a grammar to a **parser generator** (a
tool that emits parser code from grammar rules — postgres uses Bison,
`src/backend/parser/gram.y`), or write a **recursive-descent** parser by hand —
one function per grammar construct, each consuming tokens and calling the
functions for its sub-constructs.

sqlparser-rs is hand-written recursive descent, and this is the norm, not the
exception. Hand-written parsers give precise, human error messages ("expected ON
after JOIN near line 3"), and parse errors are a database's single most
user-facing surface. The entry chain is three functions deep:

```
   Parser::parse_sql(dialect, sql)     src/parser/mod.rs:582
     → parse_statements()              src/parser/mod.rs:531   -> Vec<Statement>
        → parse_statement()            src/parser/mod.rs:626   -> Statement
```

Two design details worth stealing. First, a `Dialect` trait is threaded through
every decision point — one AST, many SQLs — including the precedence table
itself (Step 2). Second, recursion is bounded: `DEFAULT_REMAINING_DEPTH = 50`
(`src/parser/mod.rs:213`, installed at `:417`), decremented by a guard at the
top of `parse_subexpr` (`:1431`). A query with 51 levels of nested parentheses
gets a clean `ParserError` instead of a stack overflow — the correct answer to
"what does your parser do on hostile input".

The `src/ast/` types are the de-facto Rust standard; DataFusion consumes them
directly.

Why it matters: this is the layer you will actually write for M10, and the
tokenizer/AST split is the cheapest structural decision in the whole stack.

### Step 2 — Pratt parsing: precedence climbing in one loop

> **In:** a token stream positioned at the start of an expression, plus a
> minimum binding precedence (`0` at the top, from `prec_unknown`).
> **Out:** one `Expr` tree, correctly parenthesized by precedence and
> associativity, with the token stream positioned just past it.

Expressions are the part of a grammar where recursive descent gets ugly:
`a + b * c > d AND e` must parse as `((a + (b*c)) > d) AND e`, and encoding
"`*` binds tighter than `+`" in a classical layered grammar takes **one
nonterminal per precedence level**.

**Count sqlparser-rs's levels.** The precedence table is a single match at
`src/dialect/mod.rs:981-1002`:

```
   Period       100      Between      20
   DoubleColon   50      Eq           20
   AtTz          41      Like         19
   MulDivModOp   40      Is           17
   PlusMinus     30      PgOther      16
   Xor           24      UnaryNot     15
   Ampersand     23      And          10
   Caret         22      Or            5
   Pipe          21      (unknown)     0    prec_unknown, mod.rs:1005-1007
   Colon         21

   18 named variants, 16 distinct numeric levels
```

A layered grammar needs 16 nonterminals plus a primary-expression rule — 17
productions, each of which must be edited and re-layered when you add an
operator. **Pratt parsing** (also called precedence climbing) replaces all 17
with one loop plus that table: parse a prefix (literal, identifier, unary op,
parenthesized expression), then repeatedly ask "does the next token bind
tighter than the precedence I was called with?" — if yes, consume it and
recurse.

```rust
// apache/datafusion-sqlparser-rs@aeb616f — src/parser/mod.rs
// (elided; the real body also handles compound field access and COLLATE)
1430      pub fn parse_subexpr(&mut self, precedence: u8) -> Result<Expr, ParserError> {
1431          let _guard = self.recursion_counter.try_decrease()?;
1433          let mut expr = self.parse_prefix()?;
    // ... 1435-1445: parse_compound_expr, then optional COLLATE ...
1448          loop {
1449              let next_precedence = self.get_next_precedence()?;
1452              if precedence >= next_precedence {
1453                  break;
1454              }
    // ... 1456-1460: the Period operator is left to compound field access ...
1462              expr = self.parse_infix(expr, next_precedence)?;
1463          }
1464          Ok(expr)
1465      }
```

and the recursion is inside `parse_infix`, at the plain binary-operator arm:

```rust
// apache/datafusion-sqlparser-rs@aeb616f — src/parser/mod.rs
4049                  Ok(Expr::BinaryOp {
4050                      left: Box::new(expr),
4051                      op,
4052                      right: Box::new(self.parse_subexpr(precedence)?),
4053                  })
```

**Work the trace**, with the real numbers from the table above. `parse_expr`
(`:1404-1406`) enters at `prec_unknown() = 0`:

```
   parse_subexpr(0)          prefix -> a
     next '+' = 30;  0 >= 30? no   -> parse_infix(a, 30)
       parse_subexpr(30)     prefix -> b
         next '*' = 40; 30 >= 40? no -> parse_infix(b, 40)
           parse_subexpr(40) prefix -> c
             next '>' = 20; 40 >= 20? YES -> break, return c
         -> (b * c)
         next '>' = 20; 30 >= 20? YES -> break, return (b * c)
     -> (a + (b * c))
     next '>' = 20;  0 >= 20? no   -> parse_infix(., 20)
       parse_subexpr(20)     prefix -> d
         next AND = 10; 20 >= 10? YES -> break, return d
     -> ((a + (b * c)) > d)
     next AND = 10;  0 >= 10? no   -> parse_infix(., 10)
       parse_subexpr(10)     prefix -> e
         next EOF = 0;  10 >= 0?  YES -> break, return e
     -> (((a + (b * c)) > d) AND e)
     next EOF = 0;   0 >= 0?  YES  -> break
```

Every `break` above is line 1452-1453 firing, and every descent is line 4052.

**Now the associativity, which is the subtle part.** The comparison is `>=`,
not `>`. Take `a - b - c`, both operators at precedence 30:

```
   parse_subexpr(0)   prefix -> a
     next '-' = 30;  0 >= 30? no -> parse_infix(a, 30)
       parse_subexpr(30)  prefix -> b
         next '-' = 30; 30 >= 30? YES -> break     <-- equality stops the recursion
     -> (a - b)
     next '-' = 30;  0 >= 30? no -> parse_infix((a-b), 30) -> c
     -> ((a - b) - c)              LEFT-associative
```

Equal precedence terminates the inner call, so the operator is left-associative.
Recurse at `precedence - 1` instead and the same operator becomes
right-associative. That single comparison is the whole associativity mechanism —
one character of code per associativity class.

Why it matters: steal this verbatim for Cypher expressions in M10. You write
one loop, one `parse_prefix`, and a table; adding an operator is one match arm,
not a grammar re-layering.

### Step 3 — rewrite rules as a trait: one file, one rule, one test suite

> **In:** a `LogicalPlan` and an `OptimizerConfig`.
> **Out:** a `Transformed<LogicalPlan>` — the (possibly rewritten) plan plus a
> flag recording whether anything changed. That is the entire per-rule
> contract; everything else is the driver's job (Step 4).

An optimizer is a pile of **rewrite rules**. Two kinds, and the distinction
matters in Step 5: a **transformation rule** turns a logical plan into an
equivalent logical plan (push a filter down, eliminate a cross join), while an
**implementation rule** turns a logical operator into a physical one (a join
becomes a hash join). Everything in this step is a transformation rule.

The organizational question is how to keep 30+ of them from becoming one giant
pass. DataFusion's answer: every rule implements one trait
(`datafusion/optimizer/src/optimizer.rs:83`):

```rust
// apache/datafusion@1e77af8 — datafusion/optimizer/src/optimizer.rs
  83  pub trait OptimizerRule: Debug {
  85      fn name(&self) -> &str;
    // ... 86-90: doc comment ...
  91      fn apply_order(&self) -> Option<ApplyOrder> {
  92          None
  93      }
    // ... 94-134: doc comments ...
 135      fn rewrite(
    //     &self, plan: LogicalPlan, config: &dyn OptimizerConfig,
    //     ) -> Result<Transformed<LogicalPlan>>
```

Three parts, and the guide-level summaries usually mention only the middle one.

- **`name`** (`:85`) — used for logging and, critically, for the error context
  when a rule fails (`:671`, `:718`). You always learn *which* rule broke the
  plan.
- **`apply_order`** (`:91`) — `Some(ApplyOrder::TopDown)`,
  `Some(ApplyOrder::BottomUp)` or `None` (`:265-270`). This is real ordering
  machinery: a rule declares how it wants the plan walked, and the driver
  performs the traversal on its behalf (`:625-662`). `None` means "I recurse
  myself".
- **`rewrite`** (`:135`) — returns `Transformed<LogicalPlan>`, a wrapper
  carrying a `transformed: bool`.

The payoff is structural: one file per rule, each unit-testable in isolation.
And "each file's bottom half is its tests" is if anything an understatement —
measured at this pin:

```
   push_down_filter.rs      4399 lines, #[cfg(test)] at :1424  -> 68% tests
   eliminate_cross_join.rs  1558 lines, #[cfg(test)] at  :490  -> 69% tests
```

The rest of the menu is the same rewrite set DuckDB's pipeline runs
(`reading-duckdb-optimizer.md`): `extract_equijoin_predicate.rs`
(`impl OptimizerRule` at `:51`), `decorrelate_predicate_subquery.rs` (`:56`).

The cost of the design: rules cannot see each other, so all cooperation has to
happen through the driver.

Why it matters: it is the cheapest way to make an optimizer contributable by
people who do not understand the whole optimizer — which is exactly the
position you are in when you start M10.

### Step 4 — the fixpoint driver: repeat until the plan repeats

> **In:** the initial `LogicalPlan` and the ordered `Vec<Arc<dyn OptimizerRule>>`.
> **Out:** the final plan, after at most `max_passes` full sweeps, plus an
> invariant check that the output schema still matches the input's.

Given independent rules, who decides the order and when to stop? DataFusion's
`Optimizer::optimize` (`optimizer.rs:581`) runs *all* rules in sequence
(`:615`), then repeats the whole sequence. A **fixpoint loop** iterates until
the output stops changing. The interesting question is how it detects that.

```rust
// apache/datafusion@1e77af8 — datafusion/optimizer/src/optimizer.rs
 598          let mut previous_plans = HashSet::with_capacity(16);
 599          previous_plans.insert(LogicalPlanSignature::new(&new_plan));
    // ... 601-603: stash the starting schema, i = 0 ...
 604          while i < options.optimizer.max_passes {
    // ... 615-723: for rule in &self.rules { ... apply it ... }
 726              // HashSet::insert returns, whether the value was newly inserted.
 727              let plan_is_fresh =
 728                  previous_plans.insert(LogicalPlanSignature::new(&new_plan));
 729              if !plan_is_fresh {
 730                  // plan did not change, so no need to continue trying to optimize
 731                  debug!("optimizer pass {i} did not make changes");
 732                  break;
 733              }
 734              i += 1;
 735          }
```

**This is the detail most summaries of DataFusion get wrong, so read lines
598 and 727 carefully.** The loop does *not* terminate on the `Transformed`
flag. It maintains a `HashSet` of the **signature of every plan it has ever
seen** and stops when a pass produces a plan it has seen before. The
`transformed` boolean is consumed only for logging (`:692-700`).

That is strictly stronger than a change flag, because it also catches
**cycles**: if rule A rewrites P→Q and rule B rewrites Q→P, every pass reports
"changed" forever, but the signature set sees P on pass 2 and breaks. A
`LogicalPlanSignature` is a pair — `node_number` and a `plan_hash` from
`DefaultHasher` (`datafusion/optimizer/src/plan_signature.rs:31-33`, built at
`:62-69`, node count at `:74`) — so it is a hash comparison, not a deep plan
walk.

`max_passes` defaults to **3** (`datafusion/common/src/config.rs:1559`,
`default = 3`), overridable via `with_max_passes` (`optimizer.rs:226`).

Compare the three engines in this course, all verified at their pins:

```
 driver style          detection of "done"                    bound
 ────────────────────  ─────────────────────────────────────  ──────────────
 DataFusion            plan-signature set                     max_passes = 3
   optimizer.rs:604      optimizer.rs:598, :727-733           config.rs:1559
 polars                boolean `changed` flag                 none — loops
   stack_opt.rs:34       stack_opt.rs:23, :36, :45              until stable
 DuckDB                n/a — no fixpoint at all               each pass runs
   optimizer.cpp:178     hand-ordered list of 39 calls          exactly once
```

The trade, restated honestly now that the mechanism is right:

```
 fixpoint of all rules (DataFusion)     once, in order (DuckDB)
 ──────────────────────────────────    ─────────────────────────
 no global ordering to hand-tune        order encodes expert knowledge
 catches rule-enables-rule chains       misses them unless ordered right
 pays repeated plan traversals          one traversal per pass
 needs a termination oracle             terminates by construction
 rules should be idempotent-ish         rules may assume predecessors ran
```

Note that DataFusion has *not* escaped ordering entirely: the rule vector is
still an ordered list (`Optimizer::new`, `:280`; `with_rules`, `:325`), and
`apply_order` (Step 3) hand-picks a traversal direction per rule. What the
fixpoint buys is tolerance of a *slightly wrong* order, not freedom from order.

Why it matters: if you build M10 rule-by-rule, this is the decision that
determines whether adding rule 31 can break rules 1-30.

### Step 5 — rewrites-only optimization: what polars gets away with

> **In:** a lazy `IR` plan built by dataframe method calls, plus `OptFlags`.
> **Out:** an optimized `IR` — with no join reordering, because the join order
> was in the input.

polars is a dataframe library with a real query optimizer hiding inside:
`.lazy()` builds a plan IR instead of executing eagerly, `.collect()` optimizes
then executes. Its optimizer module list
(`crates/polars-plan/src/plans/optimizer/mod.rs:8-37`) reads like a mini
DuckDB — `predicate_pushdown` (`:30`), `projection_pushdown` (`:31`),
`simplify_expr` (`:32`), `cse` (`:14`), `collapse_and_project` (`:11`),
`delay_rechunk` (`:8`), `cluster_with_columns` (`:10`), `slice_pushdown_lp`
(`:35`), `fused` (`:19`).

The top-level `optimize` (`:85`) is an explicit hand-ordered sequence gated on
`OptFlags` — `simplify_expr` (`:134`), `comm_subplan_elim` (`:142`),
`predicate_pushdown` (`:176`), `projection_pushdown` (`:208`), then
`simplify_expr` *again* (`:224`) — while the expression-level rules run under
`StackOptimizer::optimize_loop` (`stack_opt.rs:16`), a boolean-flag fixpoint
(`:23`, `:34`, `:36`, `:45`). So polars is both shapes at once: ordered
pipeline outside, fixpoint inside.

**What is MISSING is the lesson.** Grep the module list for join reordering and
you find exactly one join-named entry, `join_utils` (`:20`), which re-exports
`ExprOrigin` — a helper that classifies which side of a join an expression came
from. There is no cost model, no cardinality estimator, no join enumeration.

It can skip all of that because a dataframe program *is* an explicit plan — the
user already wrote the join order, method call by method call. `df.join(a).join(b)`
is not a declarative request that the system is free to reorder; it is an
instruction. Rewrites-only optimization is viable exactly when the API hands you
the order.

The M10 corollary: Cypher gives no such luck. A `MATCH` pattern names
*relationships*, not an order — `MATCH (a)-[:R]->(b)-[:S]->(c)` says nothing
about whether to start from `a`, `b` or `c`. Pattern → expansion order is a
genuine cost-based choice (anchor selection), so a FalkorDB planner cannot be
polars; it has to be at least Step 3 + Step 4, and probably Selinger
(`reading-postgres-optimizer.md`).

Why it matters: it tells you exactly which half of this topic you are allowed
to skip, and the test is a property of your *API*, not of your engine.

## Where each step lives in the code

| Step | Repo @ pin | File | Lines | What is there |
|---|---|---|---|---|
| 1 | sqlparser-rs @ `aeb616f` | `src/parser/mod.rs` | 582 | `parse_sql` — the entry point |
| 1 | sqlparser-rs | `src/parser/mod.rs` | 531 | `parse_statements` → `Vec<Statement>` |
| 1 | sqlparser-rs | `src/parser/mod.rs` | 626 | `parse_statement` — the recursive-descent root |
| 1 | sqlparser-rs | `src/parser/mod.rs` | 213, 417, 1431 | `DEFAULT_REMAINING_DEPTH = 50` and its guard |
| 2 | sqlparser-rs | `src/parser/mod.rs` | 1404-1406 | `parse_expr` — enters at `prec_unknown()` |
| 2 | sqlparser-rs | `src/parser/mod.rs` | **1430-1465** | `parse_subexpr` — the Pratt loop; break at :1452 |
| 2 | sqlparser-rs | `src/parser/mod.rs` | **4452** | `get_next_precedence` (definition; :1449 is the call) |
| 2 | sqlparser-rs | `src/parser/mod.rs` | 3833, 4049-4053 | `parse_infix`, and the binary-op recursion |
| 2 | sqlparser-rs | `src/dialect/mod.rs` | 981-1002, 1005 | `prec_value` — the whole precedence table |
| 3 | datafusion @ `1e77af8` | `datafusion/optimizer/src/optimizer.rs` | 83, 85, 91, 135 | `OptimizerRule`: `name`, `apply_order`, `rewrite` |
| 3 | datafusion | `datafusion/optimizer/src/optimizer.rs` | 265-270 | `ApplyOrder::{TopDown, BottomUp}` |
| 3 | datafusion | `datafusion/optimizer/src/push_down_filter.rs` | 761, 1424 | the rule, then 68% of the file in tests |
| 3 | datafusion | `datafusion/optimizer/src/eliminate_cross_join.rs` | 77, 490 | same shape, 69% tests |
| 4 | datafusion | `datafusion/optimizer/src/optimizer.rs` | 581, 604, 615 | `optimize`, the pass loop, the rule loop |
| 4 | datafusion | `datafusion/optimizer/src/optimizer.rs` | 598, 727-733 | the plan-signature set — the real termination test |
| 4 | datafusion | `datafusion/optimizer/src/plan_signature.rs` | 31-33, 62-69, 74 | `LogicalPlanSignature` = (node_number, plan_hash) |
| 4 | datafusion | `datafusion/common/src/config.rs` | 1559 | `max_passes, default = 3` |
| 5 | polars @ `f8bcc3d` | `crates/polars-plan/src/plans/optimizer/mod.rs` | 8-37 | the module list — read it as a menu |
| 5 | polars | `crates/polars-plan/src/plans/optimizer/mod.rs` | 85, 134, 142, 176, 208, 224 | `optimize` — the hand-ordered sequence |
| 5 | polars | `crates/polars-plan/src/plans/optimizer/stack_opt.rs` | 16, 23, 34, 36, 45 | `optimize_loop` — a boolean-flag fixpoint |

Reproduce any row with:

```
python3 tools/pinned-source.py show sqlparser-rs src/dialect/mod.rs -r 981:1007
```

## Questions for notes.md

1. Trace `a + b * c > d AND e` through `parse_subexpr` by hand with the real
   numbers (40/30/20/10/0) and check your tree against the trace in Step 2.
   Then write the Cypher expression subset you need for M10 and its precedence
   table — how many distinct levels?
2. `parse_subexpr` breaks on `precedence >= next_precedence`. Show that this
   makes `-` left-associative, then show what you would change to make `^`
   right-associative. How many characters is the diff?
3. DataFusion's plan-signature fixpoint vs DuckDB's once-in-order pipeline:
   which catches `filter → (rewrite exposes new filter) → filter` chains, what
   is the worst-case cost, and which one can loop forever if you get it wrong?
4. Why can polars skip join reordering but FalkorDB can't? Point at the exact
   place Cypher hides the join order decision, and name the polars module that
   *would* have to exist.
5. `push_down_filter.rs` is 68% tests. What does that ratio tell you about the
   real cost of adding rule number 31 to an optimizer, and how should that
   change your M10 plan?

## Takeaway

Three separable decisions, three verified answers. Expression parsing: one loop
plus a 16-level table (`parser/mod.rs:1430`, `dialect/mod.rs:981`) replaces 17
grammar nonterminals, and one `>=` decides associativity. Rule organization:
one trait with `name`/`apply_order`/`rewrite` (`optimizer.rs:83`) buys you
one-file-per-rule and 68%-tests-by-line. Fixpoint termination: not a change
flag but a set of plan signatures (`optimizer.rs:598`, `:727-733`), which is
what makes rule cycles terminate rather than spin. And the whole cost-based
half of this topic is skippable exactly when your API already specifies the
join order — which polars' does and Cypher's does not.

## Done when

Answer each before unfolding it.

- [ ] Parse `a + b * c > d AND e` on paper using sqlparser-rs's real precedence
      numbers. What are the numbers, and where does each recursion stop?
  <details><summary>Answer</summary>

  From `src/dialect/mod.rs:981-1002`: `*` = MulDivModOp = 40, `+` = PlusMinus =
  30, `>` = Eq = 20, `AND` = And = 10, and the top-level entry is
  `prec_unknown() = 0` (`:1005-1007`). `parse_subexpr(0)` takes `a`, sees `+`
  (30 > 0) and recurses at 30; that call takes `b`, sees `*` (40 > 30) and
  recurses at 40; that call takes `c`, sees `>` at 20 and **breaks because
  40 >= 20**, yielding `(b*c)`; back at 30, `>` at 20 breaks again, yielding
  `(a + (b*c))`. Then `>` is consumed at 0, `d` is parsed at 20 and stops at
  `AND` (20 >= 10), and finally `AND` is consumed and `e` parsed. Result:
  `(((a + (b * c)) > d) AND e)`. Every stop is line 1452-1453; every descent is
  line 4052.
  </details>

- [ ] Why is `a - b - c` parsed left-associatively, and what one change would
      make it right-associative?
  <details><summary>Answer</summary>

  Both `-` are at precedence 30, and the loop test is
  `if precedence >= next_precedence { break; }` (`:1452`). The inner
  `parse_subexpr(30)` sees the second `-` at 30, finds `30 >= 30` **true**, and
  breaks — so the inner call returns just `b` and the outer loop builds
  `(a - b)` before consuming the second `-`, giving `((a - b) - c)`. Equal
  precedence terminating the recursion *is* left-associativity. To make an
  operator right-associative you recurse at `precedence - 1` instead of
  `precedence` (line 4052), so the equal-precedence operator is `>` the
  threshold and gets absorbed by the inner call. One character of arithmetic
  per associativity class.
  </details>

- [ ] How many grammar productions does Pratt parsing save here, exactly?
  <details><summary>Answer</summary>

  sqlparser-rs's table (`dialect/mod.rs:981-1002`) has **18 named variants at
  16 distinct numeric levels** — 100, 50, 41, 40, 30, 24, 23, 22, 21, 20, 19,
  17, 16, 15, 10, 5 — plus `prec_unknown() = 0`. A classical layered expression
  grammar needs one nonterminal per distinct level plus a primary rule: **17
  productions**, every one of which has to be edited and re-layered to insert a
  new operator. Pratt replaces them with `parse_subexpr` (`:1430-1465`, ~36
  lines including the compound-expression and COLLATE handling) and the table.
  Adding an operator is one match arm.
  </details>

- [ ] What actually terminates DataFusion's optimizer loop? (It is not the
      `Transformed` flag.)
  <details><summary>Answer</summary>

  A `HashSet<LogicalPlanSignature>` of every plan seen so far. It is seeded
  before the loop (`optimizer.rs:598-599`) and re-inserted after each full pass
  (`:727-728`); `HashSet::insert` returning `false` means this plan was already
  seen, and the loop breaks (`:729-733`). The `transformed` boolean from
  `Transformed<LogicalPlan>` is only used for logging (`:692-700`). This matters
  because it makes **cycles** terminate: if rule A rewrites P→Q and rule B
  rewrites Q→P, every pass honestly reports "changed" forever, and a
  change-flag driver would spin to `max_passes` every single time. A
  `LogicalPlanSignature` is `(node_number, plan_hash)`
  (`plan_signature.rs:31-33`, `:62-69`), so the check is a hash lookup. The
  hard bound is `max_passes`, default **3** (`common/src/config.rs:1559`).
  </details>

- [ ] Does DataFusion's rule trait really eliminate ordering concerns?
  <details><summary>Answer</summary>

  No, it relocates them. Two mechanisms survive. First, the rule list is still
  an ordered `Vec` applied in sequence each pass (`optimizer.rs:615`, built by
  `Optimizer::new` at `:280` / `with_rules` at `:325`). Second, each rule
  declares an `apply_order` (`:91`) of `TopDown`, `BottomUp` or `None`
  (`:265-270`), and the driver performs that traversal on the rule's behalf
  (`:625-662`) — filter pushdown and projection pushdown genuinely want
  opposite directions. What the fixpoint buys is *tolerance of a slightly wrong
  order*, since a rule enabled by a later rule gets another chance next pass.
  It does not buy order-independence.
  </details>

- [ ] polars ships a full pushdown optimizer but no join reordering. Why is
      that not a bug?
  <details><summary>Answer</summary>

  Because a dataframe program is already an explicit plan. `df.join(a).join(b)`
  is an instruction, not a declarative request the system may reorder — the
  user chose the order when they wrote the method chain. The module list
  (`crates/polars-plan/src/plans/optimizer/mod.rs:8-37`) confirms the absence:
  the only join-named entry is `join_utils` (`:20`), an `ExprOrigin` helper,
  and there is no cost model or cardinality estimator anywhere in the
  directory. Rewrites-only optimization is viable exactly when the API supplies
  the order. Cypher does not: `MATCH (a)-[:R]->(b)-[:S]->(c)` names
  relationships, not a traversal order, so anchor selection and expansion order
  are genuine cost-based choices — which is why M10 needs Step 3 + Step 4 at
  minimum and probably Selinger's DP as well.
  </details>

## References

**Code**
- [sqlparser-rs](https://github.com/apache/datafusion-sqlparser-rs) @ `aeb616f`
  — `src/parser/mod.rs` (`parse_subexpr` at :1430 is the heart),
  `src/dialect/mod.rs:981-1002` (the precedence table), `src/ast/`. ~40 min.
- [datafusion](https://github.com/apache/datafusion) @ `1e77af8` —
  `datafusion/optimizer/src/optimizer.rs` (the trait at :83, the driver at
  :581), `datafusion/optimizer/src/plan_signature.rs`, then skim the
  one-file-per-rule menu. ~40 min.
- [polars](https://github.com/pola-rs/polars) @ `f8bcc3d` —
  `crates/polars-plan/src/plans/optimizer/mod.rs`: read the module list at
  :8-37 and the `optimize` sequence at :85 as much as the code; what's MISSING
  is the lesson. ~20 min.

**In this topic**
- `reading-duckdb-optimizer.md` — the same rewrite menu as an ordered,
  run-once pipeline, plus the cost-based join enumeration polars omits.
- `reading-postgres-optimizer.md` — what Step 5 says M10 cannot avoid.
- `reading-selinger-cascades.md` — where "transformation rule vs implementation
  rule" comes from, and the rule-driven optimizer generator DataFusion's trait
  is a distant descendant of.
