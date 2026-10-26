# DataFusion's dynamic filters: the hash table as the filter

DuckDB summarises its build side into something small — a range, a list, a
bitmap, a Bloom filter. DataFusion, at the commit this repo pins, mostly does
not summarise at all: for anything larger than a short IN-list it pushes a
reference to the **hash table itself** into the probe-side scan, wrapped in
an expression that asks "is this key's hash in the table?". That is the exact
semi-join of the Yannakakis chapter, run inside the scan, with no false
positives beyond 64-bit hash collisions. And the filter is routed through the
plan by *column ownership*, not by "probe child only", which makes the SQL
Server cascade expressible. DataFusion is also the Rust engine closest to
this curriculum's capstone, so its shape is the one to copy.

Every anchor is `apache/datafusion` at **`1e77af8`** (2026-07-10), read with
`tools/pinned-source.py`; the pin is the `datafusion` row of
[resources/codebases.md](../../resources/codebases.md).

## The problem in one sentence

How do you push a filter whose contents do not exist until a build finishes
into a scan that was planned before execution started — and once you can,
what should the filter *be*: a summary, or the hash table?

## The concepts, step by step

### Step 1 — a filter that is filled in later

> **In:** a physical plan. **Out:** the `DynamicFilterPhysicalExpr` — an
> expression with a placeholder now and a real predicate later — and how a
> scan learns that it changed.

A **physical expression** in DataFusion is a node that evaluates to a column
of values for a batch of rows. A `DynamicFilterPhysicalExpr` is one whose
inner expression can be swapped at run time
(`datafusion/physical-expr/src/expressions/dynamic_filters/mod.rs:56-84`).
A hash join creates it with a placeholder:

```rust
// datafusion/physical-plan/src/joins/hash_join/exec.rs:851–857 — create_dynamic_filter
851      fn create_dynamic_filter(on: &JoinOn) -> Arc<DynamicFilterPhysicalExpr> {
852          // Extract the right-side keys (probe side keys) from the `on` clauses
853          // Dynamic filter will be created from build side values (left side) and applied to probe side (right side)
854          let right_keys: Vec<_> = on.iter().map(|(_, r)| Arc::clone(r)).collect();
855          // Initialize with a placeholder expression (true) that will be updated when the hash table is built
856          Arc::new(DynamicFilterPhysicalExpr::new(right_keys, lit(true)))
857      }
```

The line to focus on is 856: until the build finishes the filter is
`lit(true)` and passes everything, so a scan that starts early is merely
unfiltered, never wrong. `update` (`mod.rs:246-274`) swaps in the real
expression and bumps a **generation** counter, and `wait_update`
(`mod.rs:300-307`) lets a consumer await a generation change. Left is build,
right is probe throughout DataFusion's hash join (`exec.rs:888-896`).

### Step 2 — when a join is allowed to make one

> **In:** a hash join. **Out:** the three refusals, one of which is the
> IN-predicate chapter's NULL rule.

`allow_join_dynamic_filter_pushdown` (`exec.rs:859-886`) refuses when the
probe side is not **preserved** — when the join type must emit unmatched
probe rows, as an outer join does (`:860-863`); when the config switch
`enable_join_dynamic_filter_pushdown` is off (default **true**,
`datafusion/common/src/config.rs:1420`); when NULLs compare equal, because
"a probe-side NULL key evaluates such predicates to NULL and would be pruned,
even though it can match a build-side NULL" (`:865-871`); and for one
partitioning mode whose partitions are not really hash-routed (`:873-883`).
The NULL refusal is [reading-in-predicates.md](reading-in-predicates.md)'s
point in one `if`: the filter is only sound where the join is a plain
semi-join on non-NULL-matching keys.

### Step 3 — routing filters by column ownership

> **In:** filters arriving at a join from above, plus its own new one.
> **Out:** which child each filter is sent to — and the difference from
> DuckDB.

```rust
// datafusion/physical-plan/src/joins/hash_join/exec.rs:1675–1712 — gather_filters_for_pushdown (end)
1675          let left_child = if left_preserved {
1676              ChildFilterDescription::from_child_with_allowed_indices(
1677                  &parent_filters,
1678                  left_allowed,
1679                  self.left(),
1680              )?
1681          } else {
1682              ChildFilterDescription::all_unsupported(&parent_filters)
1683          };
1684  
1685          let mut right_child = if right_preserved {
1686              ChildFilterDescription::from_child_with_allowed_indices(
1687                  &parent_filters,
1688                  right_allowed,
1689                  self.right(),
1690              )?
1691          } else {
1692              ChildFilterDescription::all_unsupported(&parent_filters)
1693          };
     // ... 1694–1700: comment on not re-creating an existing filter ...
1701          if phase == FilterPushdownPhase::Post
1702              && self.dynamic_filter.is_none()
1703              && self.allow_join_dynamic_filter_pushdown(config)
1704          {
1705              // Add actual dynamic filter to right side (probe side)
1706              let dynamic_filter = Self::create_dynamic_filter(&self.on);
1707              right_child = right_child.with_self_filter(dynamic_filter);
1708          }
1709  
1710          Ok(FilterDescription::new()
1711              .with_child(left_child)
1712              .with_child(right_child))
```

Two things happen. Lines 1701–1707: the join's *own* filter goes to the
right (probe) child. Lines 1675–1693: filters from *above* — including
dynamic filters other joins sent down — go to whichever child produces
their columns (`left_allowed` / `right_allowed` are built from the join's
output column sides at `:1623-1634`), as long as that side is preserved.
That includes the **left, build** child. In lane 1's bad order, the
rare_tag filter on t could therefore be routed into has_tag even though
has_tag is a build side — the step DuckDB's probe-spine walk cannot take
([reading-duckdb-join-filters.md](reading-duckdb-join-filters.md) Step 3).
Whether the filter is *populated* before that build scan reads it depends on
when the upper join's build finishes relative to the lower join's; the
`lit(true)` placeholder keeps it correct either way. This guide did not trace
the execution order, so it claims the routing, not the cascade's effect.

### Step 4 — what the filter becomes: an IN-list or the hash table

> **In:** a finished build. **Out:** the membership predicate, and the
> thresholds that pick its form.

```rust
// datafusion/physical-plan/src/joins/hash_join/exec.rs:2106–2129 — choosing the membership predicate
2106      let membership = if num_rows == 0 {
2107          PushdownStrategy::Empty
2108      } else {
2109          // If the build side is small enough we can use IN list pushdown.
2110          // If it's too big we fall back to pushing down a reference to the hash table.
2111          // See `PushdownStrategy` for more details.
2112          let estimated_size = left_values
2113              .iter()
2114              .map(|arr| arr.get_array_memory_size())
2115              .sum::<usize>();
2116          if left_values.is_empty()
2117              || left_values[0].is_empty()
2118              || estimated_size > config.optimizer.hash_join_inlist_pushdown_max_size
2119              || map.num_of_distinct_key()
2120                  > config
2121                      .optimizer
2122                      .hash_join_inlist_pushdown_max_distinct_values
2123          {
2124              PushdownStrategy::Map(Arc::clone(&map))
2125          } else if let Some(in_list_values) = build_struct_inlist_values(&left_values)? {
2126              PushdownStrategy::InList(in_list_values)
2127          } else {
2128              PushdownStrategy::Map(Arc::clone(&map))
2129          }
2130      };
```

The defaults are **128 KiB** (`config.rs:1608`) and **150 distinct values**
(`config.rs:1622`, "inspired by Trino's `max-filter-keys-per-column`"), per
partition. Worked on lane 1's bad order: the rare_tag build has 10 distinct
tags — an IN-list; has_tag after filtering has 1,000 rows with ~1,000
distinct posts — over 150, so the scan gets the hash table (line 2124). The
line to focus on is 2124: there is no Bloom filter in this path at all.

The `Map` form is evaluated by `HashTableLookupExpr::evaluate`
(`partitioned_hash_eval.rs:372-388`), which hashes the probe keys and calls
`contain_hashes` on the build's hash map — or, for a dense integer build,
`contain_keys` on an array map. It checks the 64-bit *hash*, so a false
positive needs a full 64-bit collision: with ~1,000 build keys, the chance a
given absent key collides is about 1,000 / 2⁶⁴ ≈ 5 × 10⁻¹⁷, against lane 1b's
0.5% for DuckDB's register-blocked filter at the same key count. The price is
the probe: a hash-table lookup per row instead of one cached word.

### Step 5 — bounds, and combining per-partition filters

> **In:** Step 4's membership predicate. **Out:** the final expression the
> scan evaluates.

Each build partition also reports min/max **bounds** per key column.
`create_bounds_predicate` (`shared_bounds.rs:147-185`) turns them into
`col >= min AND col <= max`, and `combine_membership_and_bounds`
(`:191-204`) ANDs bounds before membership, so the cheap range test runs
first and lets a Parquet scan skip row groups — RPT+'s cascade filter
(min–max, then membership) without the Bloom filter. With hash-partitioned
builds, `build_filter` (`:566-693`) emits a `CASE hash(key) % n WHEN i THEN
<partition i's filter>` so each probe row is checked only against the
partition it would join with. Each probe stream reports its build and waits
for every partition before probing (`stream.rs:549-579`, state
`WaitPartitionBoundsReport`), so the published filter is complete before
use.

### Step 6 — DataFusion beside the papers

> **In:** Steps 1–5. **Out:** what this design has and lacks, in the
> vocabulary of the three papers.

- **Exact, not approximate.** The `Map` filter is a semi-join with
  negligible false positives — Yannakakis' operator, not PT's Bloom filter.
  The PT paper's §1 objection (hash-table cost in the semi-join phase) is
  answered differently: the hash table is not built for the filter, it is
  the join's own, already built.
- **Forward only, one hop per join.** Each join filters its probe subtree;
  there is no backward pass and no separate phase, as in SQL Server (CIDR'26
  §3.2).
- **Routing can enter build sides** (Step 3) — the precondition for SQL
  Server's cascade, which DuckDB's walk lacks.
- **No run-time abandon rule** in this path: unlike DuckDB's
  selectivity-optional wrapper or RPT+'s dynamic pipeline, a dynamic filter
  that passes every row is still evaluated (this guide found no pause logic
  in `hash_join/`; it did not survey the scan side).

## How to read the source (with the concepts in hand)

1. `dynamic_filters/mod.rs:36-120` then `:230-310` — the filter object.
2. `hash_join/exec.rs:845-896` — creation and the refusals.
3. `exec.rs:1593-1744` — routing (`gather_filters_for_pushdown`) and
   installing (`handle_child_pushdown_result`).
4. `exec.rs:2095-2132` — membership choice; `config.rs:1595-1622` for the
   knobs' documentation.
5. `shared_bounds.rs:80-290`, `:566-693`; `partitioned_hash_eval.rs:255-388`.
6. `stream.rs:540-620` — the probe waiting on the build.

## Questions (answer in notes.md)

1. Write the DataFusion plan for lane 1's bad order and mark which filters
   Step 3 routes into which scans. Which of them would be populated before
   their scan starts?
2. `contain_hashes` checks hashes only. Under what key distribution could
   that be measurably worse than a check on keys?
3. A pushed `HashTableLookupExpr` costs one hash-table probe per scanned row.
   When is that *more* expensive than letting the join probe the same table
   later? (Hint: what if the filter passes 95% of rows?)
4. Port lane 3's `IntoLowerBuilds` mode to a DataFusion-shaped rule: a
   filter goes to any child that owns its column. Does it reproduce the
   60,504?

## Done when

Answer each before unfolding it.

- [ ] You can explain how a scan planned before execution can use a filter built during it.

  <details><summary>Answer</summary>

  The plan contains a `DynamicFilterPhysicalExpr` created with a `lit(true)`
  placeholder (`exec.rs:856`). When the build finishes, `update` swaps in the
  real predicate and bumps a generation counter (`mod.rs:246-274`); consumers
  can await it (`mod.rs:300-307`). A scan that runs before the update is
  unfiltered, never incorrect.

  </details>

- [ ] You can list when a hash join refuses to create a dynamic filter.

  <details><summary>Answer</summary>

  Probe side not preserved (outer-type joins), the
  `enable_join_dynamic_filter_pushdown` switch off (default true), NULLs
  comparing equal — a NULL probe key would be pruned though it could match —
  and Hive-style preserved file partitions under partitioned mode
  (`exec.rs:859-886`).

  </details>

- [ ] You can say how routing differs from DuckDB's and why that matters for the cascade.

  <details><summary>Answer</summary>

  DataFusion sends each parent filter to whichever child owns its columns, if
  that side is preserved — build (left) or probe (right) —
  (`exec.rs:1675-1693`), and its own filter to the probe side (`:1701-1707`).
  DuckDB's search follows only probe children and stops at a scan without the
  column. Reaching build sides is what lets a bitmap from an upper join filter
  a lower join's build — the SQL Server cascade that took lane 1's bad order
  from 3,020,210 to 60,504 intermediates in lane 3's model.

  </details>

- [ ] You can state the IN-list thresholds and what replaces the IN-list above them.

  <details><summary>Answer</summary>

  IN-list if the build's key arrays total ≤ 128 KiB and ≤ 150 distinct keys
  per partition (`config.rs:1608`, `:1622`); otherwise a reference to the
  build's hash map, evaluated by `HashTableLookupExpr` with `contain_hashes`
  (`exec.rs:2116-2129`, `partitioned_hash_eval.rs:372-388`). No Bloom filter.

  </details>

- [ ] You can compare the false-positive behaviour of the hash-table filter with DuckDB's Bloom filter.

  <details><summary>Answer</summary>

  The hash-table filter tests 64-bit hashes, so an absent key passes only on a
  full 64-bit collision — about n/2⁶⁴ per key, ≈ 5 × 10⁻¹⁷ for 1,000 keys.
  DuckDB's register-blocked filter at 1,000 keys and 16.4 bits/key measures
  0.548% in lane 1b. DataFusion pays a hash-table probe per scanned row
  instead of one word.

  </details>

## References

- `apache/datafusion` at `1e77af8`:
  `datafusion/physical-expr/src/expressions/dynamic_filters/mod.rs`,
  `datafusion/physical-plan/src/joins/hash_join/{exec.rs, shared_bounds.rs, partitioned_hash_eval.rs, stream.rs}`,
  `datafusion/common/src/config.rs`.
- The DataFusion blog post the source links (`mod.rs:62-64`), "Dynamic
  Filters: Passing Information Between Operators During Execution for 25x
  Faster Queries" (2025-09-10) — not read for this chapter; its 25× is not
  quoted here.
- Previous: [reading-duckdb-join-filters.md](reading-duckdb-join-filters.md).
  Back to the topic: [README.md](README.md).
