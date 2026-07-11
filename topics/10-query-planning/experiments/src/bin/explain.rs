//! EXPLAIN for the toy planner — provided, works once planner.rs is done.
//!
//!   cargo run --bin explain
//!
//! Prints naive / pushed-down / reordered plans with cardinality estimates
//! for three sample queries. Then load the same schema into DuckDB
//! (`.mode duckbox` + EXPLAIN) and compare join orders — note every
//! disagreement in notes.md and figure out whose estimate caused it.

use planner_experiments::planner::*;

fn indent(n: usize) -> String {
    "  ".repeat(n)
}

fn expr_str(e: &Expr) -> String {
    match e {
        Expr::ColEqLit { table, column, value } => format!("{table}.{column} = {value}"),
        Expr::ColEqCol { left, right } => {
            format!("{}.{} = {}.{}", left.0, left.1, right.0, right.1)
        }
    }
}

fn print_plan(p: &Plan, catalog: &Catalog, depth: usize) {
    let card = estimate(p, catalog);
    let pre = indent(depth);
    match p {
        Plan::Scan { table, pushed } => {
            let filters = if pushed.is_empty() {
                String::new()
            } else {
                format!(
                    " [{}]",
                    pushed.iter().map(expr_str).collect::<Vec<_>>().join(" AND ")
                )
            };
            println!("{pre}Scan {table}{filters}  (est {card:.0})");
        }
        Plan::Filter { pred, input } => {
            println!("{pre}Filter {}  (est {card:.0})", expr_str(pred));
            print_plan(input, catalog, depth + 1);
        }
        Plan::Join { left, right, on } => {
            let on_str = match on {
                Some(e) => format!("HashJoin {}", expr_str(e)),
                None => "CrossJoin".to_string(),
            };
            println!("{pre}{on_str}  (est {card:.0})");
            print_plan(left, catalog, depth + 1);
            print_plan(right, catalog, depth + 1);
        }
        Plan::Project { columns, input } => {
            println!("{pre}Project {}  (est {card:.0})", columns.join(", "));
            print_plan(input, catalog, depth + 1);
        }
    }
}

fn explain(sql: &str, catalog: &Catalog) {
    println!("=== {sql}");
    let naive = parse_and_plan(sql).expect("parse");
    println!("--- naive");
    print_plan(&naive, catalog, 0);

    let pushed = push_down(naive);
    println!("--- after push_down");
    print_plan(&pushed, catalog, 0);

    let reordered = reorder_joins(pushed, catalog);
    println!("--- after reorder_joins");
    print_plan(&reordered, catalog, 0);
    println!();
}

fn main() {
    let mut catalog = Catalog::new();
    catalog.insert(
        "users".into(),
        table(10_000, &[("id", 10_000), ("city", 100), ("age", 50)]),
    );
    catalog.insert(
        "orders".into(),
        table(
            1_000_000,
            &[("id", 1_000_000), ("user_id", 10_000), ("status", 5)],
        ),
    );
    catalog.insert(
        "items".into(),
        table(5_000_000, &[("order_id", 1_000_000), ("sku", 20_000)]),
    );

    explain(
        "SELECT users.city FROM users, orders \
         WHERE users.city = 7 AND users.id = orders.user_id",
        &catalog,
    );

    explain(
        "SELECT users.id FROM users, orders, items \
         WHERE users.id = orders.user_id AND orders.id = items.order_id \
         AND orders.status = 2",
        &catalog,
    );

    // The interesting one: does the selective users filter make
    // {users, orders} cheaper than {orders, items}? Predict first.
    explain(
        "SELECT items.sku FROM items, orders, users \
         WHERE orders.id = items.order_id AND users.id = orders.user_id \
         AND users.city = 7 AND users.age = 30",
        &catalog,
    );
}
