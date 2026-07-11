//! A tiny cost-based planner over sqlparser-rs ASTs.
//!
//! You implement four functions (bottom of file). The types and tests fix
//! the contract; `bin/explain.rs` pretty-prints your plans next to their
//! estimates so you can compare against DuckDB's EXPLAIN.
//!
//! Pipeline (mirrors the README):
//!
//! ```text
//!  SQL --parse_and_plan--> naive Plan (left-deep cross joins, Filter stack)
//!      --push_down-------> filters sunk into scans, cross joins -> eq joins
//!      --reorder_joins---> greedy smallest-estimated-pair-first join order
//!      --estimate--------> cardinality (the number everything hinges on)
//! ```
//!
//! Supported SQL subset (the tests never leave it):
//!   SELECT col, ... FROM t1, t2, ... WHERE a.x = 1 AND a.y = b.y AND ...
//! Literals are i64 only. Every column reference is `table.column`.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Catalog: the statistics the optimizer lies with
// ---------------------------------------------------------------------------

/// Per-table stats. `distinct[col]` = NDV (number of distinct values).
#[derive(Debug, Clone)]
pub struct TableStats {
    pub rows: u64,
    pub distinct: HashMap<String, u64>,
}

/// table name -> stats
pub type Catalog = HashMap<String, TableStats>;

pub fn table(rows: u64, cols: &[(&str, u64)]) -> TableStats {
    TableStats {
        rows,
        distinct: cols.iter().map(|(c, n)| (c.to_string(), *n)).collect(),
    }
}

// ---------------------------------------------------------------------------
// Expressions and plans
// ---------------------------------------------------------------------------

/// Only the two predicate shapes the planner reasons about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// `table.column = <int literal>` — a single-table filter.
    ColEqLit {
        table: String,
        column: String,
        value: i64,
    },
    /// `l_table.l_col = r_table.r_col` — a join predicate.
    ColEqCol {
        left: (String, String),
        right: (String, String),
    },
}

impl Expr {
    /// Tables this predicate touches (1 for ColEqLit, 2 for ColEqCol).
    pub fn tables(&self) -> Vec<&str> {
        match self {
            Expr::ColEqLit { table, .. } => vec![table],
            Expr::ColEqCol { left, right } => vec![&left.0, &right.0],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Scan {
        table: String,
        /// Filters pushed into the scan (empty until push_down runs).
        pushed: Vec<Expr>,
    },
    Filter {
        pred: Expr,
        input: Box<Plan>,
    },
    Join {
        left: Box<Plan>,
        right: Box<Plan>,
        /// None = cross join (naive plan); Some = equi-join after push_down.
        on: Option<Expr>,
    },
    Project {
        columns: Vec<String>,
        input: Box<Plan>,
    },
}

impl Plan {
    /// All base tables under this plan node.
    pub fn tables(&self) -> Vec<String> {
        match self {
            Plan::Scan { table, .. } => vec![table.clone()],
            Plan::Filter { input, .. } | Plan::Project { input, .. } => input.tables(),
            Plan::Join { left, right, .. } => {
                let mut t = left.tables();
                t.extend(right.tables());
                t
            }
        }
    }
}

#[derive(Debug)]
pub enum PlanError {
    Parse(String),
    Unsupported(String),
}

// ---------------------------------------------------------------------------
// The four functions you implement
// ---------------------------------------------------------------------------

/// Parse `sql` (via sqlparser's GenericDialect) and build the NAIVE plan:
///
/// - FROM t1, t2, t3   -> left-deep cross joins in written order:
///                        Join(Join(Scan t1, Scan t2), Scan t3), all on=None
/// - WHERE a AND b     -> a stack of Filter nodes above the joins
///                        (split conjuncts; each becomes one Filter)
/// - SELECT c1, c2     -> Project at the root (column names as written)
///
/// No optimization here — this is the "tree that mirrors the SQL" stage.
/// Return Unsupported for anything outside the subset (ORDER BY, subqueries,
/// non-i64 literals, expressions other than the two Expr shapes).
pub fn parse_and_plan(sql: &str) -> Result<Plan, PlanError> {
    let _ = sql;
    todo!("parse with sqlparser, build naive left-deep plan")
}

/// Rewrite pass 1: predicate pushdown.
///
/// - Every ColEqLit filter sinks into its table's Scan.pushed.
/// - Every ColEqCol filter sinks to the LOWEST Join whose two sides
///   together cover both tables, and becomes that join's `on`.
///   (If a join already has an `on`, keep the extra predicate as a Filter
///   directly above that join — one equi-condition per join is enough here.)
/// - After this pass, no Filter node with a ColEqLit predicate remains.
pub fn push_down(plan: Plan) -> Plan {
    let _ = plan;
    todo!("sink single-table filters into scans, turn cross joins into equi-joins")
}

/// Rewrite pass 2: greedy join reordering (DuckDB's fallback, simplified).
///
/// Collect the base relations (scans, with their pushed filters) and the
/// join predicates, then greedily build a tree: repeatedly pick the pair of
/// current subplans connected by a predicate whose JOIN OUTPUT has the
/// smallest `estimate`, join them, repeat. Avoid cross joins unless no
/// connected pair remains.
///
/// This is smallest-intermediate-result-first — compare with DuckDB's
/// SolveJoinOrderApproximately (plan_enumerator.cpp:398).
pub fn reorder_joins(plan: Plan, catalog: &Catalog) -> Plan {
    let _ = (plan, catalog);
    todo!("greedy smallest-estimated-pair-first")
}

/// Cardinality estimation — the three classical lies, verbatim:
///
/// - Scan            -> rows * product of pushed-filter selectivities
/// - ColEqLit sel    -> 1 / NDV(column)              (uniformity)
/// - Filter          -> estimate(input) * sel        (independence)
/// - Join with on    -> |L| * |R| / max(NDV_l, NDV_r) (containment)
/// - Join cross      -> |L| * |R|
/// - Project         -> estimate(input)
///
/// Missing NDV: use 10 (your DEFAULT_EQ_SEL moment — pick it, document it).
pub fn estimate(plan: &Plan, catalog: &Catalog) -> f64 {
    let _ = (plan, catalog);
    todo!("selectivity arithmetic")
}

// ---------------------------------------------------------------------------
// Tests: the contract
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let mut c = Catalog::new();
        c.insert("users".into(), table(10_000, &[("id", 10_000), ("city", 100), ("age", 50)]));
        c.insert("orders".into(), table(1_000_000, &[("id", 1_000_000), ("user_id", 10_000), ("status", 5)]));
        c.insert("items".into(), table(5_000_000, &[("order_id", 1_000_000), ("sku", 20_000)]));
        c
    }

    fn scan(t: &str) -> Plan {
        Plan::Scan { table: t.into(), pushed: vec![] }
    }

    // -- parse_and_plan -----------------------------------------------------

    #[test]
    fn naive_plan_mirrors_the_sql() {
        let plan = parse_and_plan(
            "SELECT users.city FROM users, orders \
             WHERE users.city = 7 AND users.id = orders.user_id",
        )
        .unwrap();

        let expected = Plan::Project {
            columns: vec!["users.city".into()],
            input: Box::new(Plan::Filter {
                pred: Expr::ColEqCol {
                    left: ("users".into(), "id".into()),
                    right: ("orders".into(), "user_id".into()),
                },
                input: Box::new(Plan::Filter {
                    pred: Expr::ColEqLit { table: "users".into(), column: "city".into(), value: 7 },
                    input: Box::new(Plan::Join {
                        left: Box::new(scan("users")),
                        right: Box::new(scan("orders")),
                        on: None,
                    }),
                }),
            }),
        };
        // Filter stack order (which conjunct ends up on top) is up to you —
        // accept either stacking.
        let alt = Plan::Project {
            columns: vec!["users.city".into()],
            input: Box::new(Plan::Filter {
                pred: Expr::ColEqLit { table: "users".into(), column: "city".into(), value: 7 },
                input: Box::new(Plan::Filter {
                    pred: Expr::ColEqCol {
                        left: ("users".into(), "id".into()),
                        right: ("orders".into(), "user_id".into()),
                    },
                    input: Box::new(Plan::Join {
                        left: Box::new(scan("users")),
                        right: Box::new(scan("orders")),
                        on: None,
                    }),
                }),
            }),
        };
        assert!(plan == expected || plan == alt, "got {plan:#?}");
    }

    #[test]
    fn from_order_gives_left_deep_cross_joins() {
        let plan = parse_and_plan("SELECT users.id FROM users, orders, items").unwrap();
        let Plan::Project { input, .. } = plan else { panic!("no project root") };
        let Plan::Join { left, right, on } = *input else { panic!("no top join") };
        assert_eq!(on, None);
        assert_eq!(right.tables(), vec!["items".to_string()]);
        assert_eq!(left.tables(), vec!["users".to_string(), "orders".to_string()]);
    }

    // -- push_down ----------------------------------------------------------

    fn count_lit_filters(p: &Plan) -> usize {
        match p {
            Plan::Scan { .. } => 0,
            Plan::Filter { pred, input } => {
                let here = matches!(pred, Expr::ColEqLit { .. }) as usize;
                here + count_lit_filters(input)
            }
            Plan::Join { left, right, .. } => count_lit_filters(left) + count_lit_filters(right),
            Plan::Project { input, .. } => count_lit_filters(input),
        }
    }

    fn find_scan<'a>(p: &'a Plan, t: &str) -> Option<&'a Plan> {
        match p {
            Plan::Scan { table, .. } if table == t => Some(p),
            Plan::Scan { .. } => None,
            Plan::Filter { input, .. } | Plan::Project { input, .. } => find_scan(input, t),
            Plan::Join { left, right, .. } => find_scan(left, t).or_else(|| find_scan(right, t)),
        }
    }

    #[test]
    fn pushdown_sinks_literal_filters_into_scans() {
        let plan = parse_and_plan(
            "SELECT users.id FROM users, orders \
             WHERE users.city = 7 AND orders.status = 2 AND users.id = orders.user_id",
        )
        .unwrap();
        let plan = push_down(plan);

        assert_eq!(count_lit_filters(&plan), 0, "literal filters must all sink");
        let Some(Plan::Scan { pushed, .. }) = find_scan(&plan, "users") else { panic!() };
        assert!(pushed.contains(&Expr::ColEqLit {
            table: "users".into(), column: "city".into(), value: 7
        }));
        let Some(Plan::Scan { pushed, .. }) = find_scan(&plan, "orders") else { panic!() };
        assert!(pushed.contains(&Expr::ColEqLit {
            table: "orders".into(), column: "status".into(), value: 2
        }));
    }

    #[test]
    fn pushdown_turns_cross_join_into_equi_join() {
        let plan = parse_and_plan(
            "SELECT users.id FROM users, orders WHERE users.id = orders.user_id",
        )
        .unwrap();
        let plan = push_down(plan);

        fn top_join(p: &Plan) -> &Plan {
            match p {
                Plan::Project { input, .. } | Plan::Filter { input, .. } => top_join(input),
                other => other,
            }
        }
        let Plan::Join { on, .. } = top_join(&plan) else { panic!("no join") };
        assert_eq!(
            *on,
            Some(Expr::ColEqCol {
                left: ("users".into(), "id".into()),
                right: ("orders".into(), "user_id".into()),
            })
        );
    }

    // -- estimate -----------------------------------------------------------

    #[test]
    fn estimate_uses_independence_and_containment() {
        let c = catalog();

        // scan with one pushed filter: 10_000 rows * (1/100) = 100
        let s = Plan::Scan {
            table: "users".into(),
            pushed: vec![Expr::ColEqLit { table: "users".into(), column: "city".into(), value: 7 }],
        };
        assert_eq!(estimate(&s, &c), 100.0);

        // two filters multiply (independence): 10_000 * 1/100 * 1/50 = 2
        let s2 = Plan::Scan {
            table: "users".into(),
            pushed: vec![
                Expr::ColEqLit { table: "users".into(), column: "city".into(), value: 7 },
                Expr::ColEqLit { table: "users".into(), column: "age".into(), value: 30 },
            ],
        };
        assert_eq!(estimate(&s2, &c), 2.0);

        // equi-join: |L|*|R| / max(ndv) = 10_000 * 1_000_000 / 10_000 = 1e6
        let j = Plan::Join {
            left: Box::new(scan("users")),
            right: Box::new(scan("orders")),
            on: Some(Expr::ColEqCol {
                left: ("users".into(), "id".into()),
                right: ("orders".into(), "user_id".into()),
            }),
        };
        assert_eq!(estimate(&j, &c), 1_000_000.0);

        // cross join: plain product
        let x = Plan::Join {
            left: Box::new(scan("users")),
            right: Box::new(scan("orders")),
            on: None,
        };
        assert_eq!(estimate(&x, &c), 10_000.0 * 1_000_000.0);
    }

    // -- reorder_joins ------------------------------------------------------

    /// The deepest join in a left-deep tree = the pair chosen FIRST.
    fn first_pair(p: &Plan) -> Vec<String> {
        match p {
            Plan::Join { left, .. } => {
                if matches!(**left, Plan::Join { .. })
                    || matches!(**left, Plan::Filter { ref input, .. } if matches!(**input, Plan::Join { .. }))
                {
                    first_pair(left)
                } else {
                    p.tables()
                }
            }
            Plan::Project { input, .. } | Plan::Filter { input, .. } => first_pair(input),
            Plan::Scan { .. } => vec![],
        }
    }

    #[test]
    fn join_order_flips_with_stats() {
        let sql = "SELECT a.x FROM a, b, c WHERE a.x = b.x AND b.y = c.y";

        // catalog 1: c is tiny -> greedy should join {b, c} first
        let mut c1 = Catalog::new();
        c1.insert("a".into(), table(1_000_000, &[("x", 1_000_000)]));
        c1.insert("b".into(), table(1_000, &[("x", 1_000), ("y", 1_000)]));
        c1.insert("c".into(), table(10, &[("y", 10)]));

        let plan = reorder_joins(push_down(parse_and_plan(sql).unwrap()), &c1);
        let mut pair = first_pair(&plan);
        pair.sort();
        assert_eq!(pair, vec!["b".to_string(), "c".to_string()]);

        // catalog 2: a is tiny instead -> {a, b} should win
        let mut c2 = Catalog::new();
        c2.insert("a".into(), table(10, &[("x", 10)]));
        c2.insert("b".into(), table(1_000, &[("x", 1_000), ("y", 1_000)]));
        c2.insert("c".into(), table(1_000_000, &[("y", 1_000_000)]));

        let plan = reorder_joins(push_down(parse_and_plan(sql).unwrap()), &c2);
        let mut pair = first_pair(&plan);
        pair.sort();
        assert_eq!(pair, vec!["a".to_string(), "b".to_string()]);
    }
}
