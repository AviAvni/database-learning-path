# TPC-H decoded: 22 queries, 28 choke points

TPC-H's 22 queries are not arbitrary — each stresses a named set of
engine capabilities ("choke points"), and Boncz, Neumann, and
Erling's TPCTC 2013 paper catalogs all 28 of them. Internalize the
map and a benchmark number stops being a score and becomes a
diagnosis. Read it WITH the queries open (DuckDB vendors them —
see References and
[reading-duckdb-tpch.md](reading-duckdb-tpch.md)).

## The choke-point taxonomy (condensed)

```
  CP1 aggregation      dominated by GROUP BY machinery
      CP1.1 ordered agg / CP1.2 small group-by keys (Q1!) /
      CP1.4 dependent group-by (Q18)
  CP2 joins            order (Q5,Q7-Q9), semijoin (Q4,Q21,Q22),
                       large vs selective probes
  CP3 locality         materialized views would help (Q14/Q15),
                       physical column order
  CP4 expressions      arithmetic-heavy (Q1 again), string match
                       LIKE '%green%' (Q9), date logic everywhere
  CP5 correlated subq  Q2, Q11, Q17, Q20-Q22
  CP6 parallelism      all of them, but skew hits Q9/Q18 hardest
```

## The three queries everyone profiles (and why)

| query | choke points | what it really measures |
|---|---|---|
| Q1 | CP1.2 + CP4 | expression evaluation + tiny-domain aggregation: ~4 groups, so the hash table is FREE and fused arithmetic dominates — our `q1_flat` stub makes this explicit |
| Q6 | CP4 + scan | pure selection: ~2% selectivity, SIMD-able predicates — DBMS "GB/s scanned" headline numbers are usually Q6 |
| Q9 | CP2 + CP4 (LIKE) + CP6 skew | 6-way join order + `%green%` string matching + per-nation skew — the query that punishes optimizers |

Q1's whole trick, made explicit (this is what our `q1_flat` stub
implements):

```rust
// Q1: "GROUP BY returnflag, linestatus" has ~6 groups TOTAL, so the
// hash table degenerates into a flat array — all that's left to
// measure is expression evaluation and fused accumulation.
fn q1_flat(c: &LineItemColumns) -> [Agg; 6] {
    let mut g = [Agg::default(); 6];
    for i in 0..c.len {
        if c.shipdate[i] > CUTOFF { continue; }
        let k = group_code(c.returnflag[i], c.linestatus[i]);  // 0..5
        let disc_price = c.extendedprice[i] * (1.0 - c.discount[i]);
        g[k].sum_qty        += c.quantity[i];
        g[k].sum_disc_price += disc_price;
        g[k].sum_charge     += disc_price * (1.0 + c.tax[i]);
        g[k].count          += 1;
    }
    g
}
```

## Hidden messages worth knowing

- **Uniformity is a lie you can exploit**: dbgen data is uniform and
  independent — cardinality estimation is EASY on TPC-H (contrast
  JOB, topic 10, built precisely because of this).
- **Q1's group count (4-6) makes hash-agg invisible** — a benchmark
  win on Q1 says nothing about high-cardinality GROUP BY; that's why
  ClickBench and TPC-DS exist.
- **Refresh functions (RF1/RF2) are always skipped** in informal
  runs — published "TPC-H" numbers are usually just the power test's
  22 SELECTs, i.e. read-only. Say "TPC-H-derived" (the spec police
  are real, and so is the Fair Benchmarking paper — topic 0 guide).
- **Scale factor changes the winner**: SF1 fits in cache, SF100
  doesn't — engine rankings flip between them (topic 0's ladder).

## Questions (answer in notes.md)

1. Map Q1/Q6/Q9 onto FalkorDB-relevant analogues: which Cypher query
   shapes hit the same choke points (small-domain agg, scan+filter,
   join-order + skew)?
2. Our dbgen-lite is uniform AND independent like the real dbgen.
   Which columns would need correlation to break `q1_flat`'s
   perfect-group-code trick?
3. Why does Q6's ~2% selectivity favor branchy evaluation while 50%
   would favor branchless (topic 17's crater)? Predict the measured
   crossover for `q6_branchless`.
4. Choke point CP3 (materialization): which of the 22 queries would
   an incremental-view engine (topic 27 preview) answer in O(1)?
5. TPC-H says nothing about updates. What does TPC-C's NewOrder mix
   test that no TPC-H query can (see reading-oltpbench-tpcc.md)?

## References

**Papers**
- Boncz, Neumann, Erling — "TPC-H Analyzed: Hidden Messages and
  Lessons Learned from an Influential Benchmark" (TPCTC 2013) — the
  choke-point catalog; read with the queries open

**Code**
- [duckdb](https://github.com/duckdb/duckdb)
  `extension/tpch/dbgen/queries/q01.sql … q22.sql` — the 22
  queries; reference answers in `dbgen/answers/`
