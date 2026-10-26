# DataFusion's dynamic filters: the hash table as the filter

DuckDB summarises its build side into something small — a range, a list, a
bitmap, a Bloom filter. DataFusion, for anything larger than a short IN-list,
does not summarise the rows at all: it pushes a reference to the **hash table
itself** into the probe-side scan, wrapped in an expression that asks "is this
key's hash in the table?" (and, for a dense integer key, "is this key in the
array map?"). That is the exact semi-join of the Yannakakis chapter, run inside
the scan, with no false positives beyond 64-bit hash collisions. And the filter
is routed through the plan by *column ownership* and across equi-join keys, not
by "probe child only", which makes the SQL Server cascade expressible.
DataFusion is also the Rust engine closest to this curriculum's capstone, so its
shape is the one to copy.

Every anchor is `apache/datafusion` on `main` at **`350ca7650`**
(2026-10-26), read with `tools/pinned-source.py`; the revision is the
`datafusion` row of [resources/codebases.md](../../resources/codebases.md).

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
(`datafusion/physical-expr/src/expressions/dynamic_filters/mod.rs:61-98`).
A hash join creates it with a placeholder:

```rust
// datafusion/physical-plan/src/joins/hash_join/exec.rs:962–968 — create_dynamic_filter
962      fn create_dynamic_filter(on: &JoinOn) -> Arc<DynamicFilterPhysicalExpr> {
963          // Extract the right-side keys (probe side keys) from the `on` clauses
964          // Dynamic filter will be created from build side values (left side) and applied to probe side (right side)
965          let right_keys: Vec<_> = on.iter().map(|(_, r)| Arc::clone(r)).collect();
966          // Initialize with a placeholder expression (true) that will be updated when the hash table is built
967          Arc::new(DynamicFilterPhysicalExpr::new(right_keys, lit(true)))
968      }
```

The line to focus on is 967: until the build finishes the filter is
`lit(true)` and passes everything, so a scan that starts early is merely
unfiltered, never wrong. `update` (`mod.rs:317-345`) swaps in the real
expression and bumps a **generation** counter, and `wait_update`
(`mod.rs:393-400`) lets a consumer await a generation change. Left is build,
right is probe throughout DataFusion's hash join (the doc comment at
`exec.rs:659-663`).

### Step 2 — when a join is allowed to make one

> **In:** a hash join. **Out:** the refusals, and how the IN-predicate
> chapter's NULL rule shows up.

`allow_join_dynamic_filter_pushdown` (`exec.rs:1037-1085`) refuses when the
probe side is not **preserved** — when the join type must emit unmatched
probe rows, as an outer join does (`:1038-1041`); when the config switch
`enable_join_dynamic_filter_pushdown` is off (default **true**,
`datafusion/common/src/config.rs:1686`); for a **null-aware** join (the
`NOT IN` form) whose build keys can be NULL, because a pruned probe row would
make the three-valued answer wrong (`:1043-1058`); for preserved file
partitions that report hash partitioning without following the filter's hash
routing (`:1060-1076`); and for a partitioned join without usable routing
(`:1078-1082`).

A join whose NULLs compare equal is *not* refused. Instead the finished filter
is widened so that probe rows with a NULL key survive: `preserve_probe_nulls`
(`shared_bounds.rs:817-868`) ORs `key IS NULL` in front of the filter
(lines 854–867) whenever the join is null-aware, or null-equal with a build
that actually holds a NULL key (lines 826–828), and leaves the filter alone
for a build side with no NULL. That is [reading-in-predicates.md](reading-in-predicates.md)'s
point in code: the plain filter drops a probe-side NULL, which can match a
build-side NULL in a null-equal join, so the filter is only sound as a
semi-join on non-NULL-matching keys unless it carries the escape.

### Step 3 — routing filters by column ownership and across keys

> **In:** filters arriving at a join from above, plus its own new one.
> **Out:** which child each filter is sent to — and the difference from
> DuckDB.

```rust
// datafusion/physical-plan/src/joins/hash_join/exec.rs:1937–2014 — gather_filters_for_pushdown (excerpt)
1937          let (left_preserved, right_preserved) = lr_is_preserved(self.join_type);
      // ... 1938–1961: map each join output column to its left or right input column ...
1963          // Transfer filters across the equi-join keys: a parent filter over one
1964          // side's join-key columns holds for every matching row of the other
1965          // side too, so it is also pushed there, rewritten over that side's key
1966          // expressions. This is how a dynamic filter from a join above reaches
1967          // the scans on both sides of this join, and how a semi join prunes its
1968          // non-output side. Like the plain column routing, a transfer only
1969          // targets a side that `lr_is_preserved` permits.
1970          let (to_right, to_left) = if Self::supports_key_transfer(self.join_type) {
1971              self.key_transfer_maps(&column_indices)
1972          } else {
1973              Default::default()
1974          };
1975          let describe_child = |preserved: bool,
1976                                column_mapping: HashMap<usize, usize>,
1977                                key_map: &KeyTransferMap,
1978                                child: &Arc<dyn ExecutionPlan>|
1979           -> Result<ChildFilterDescription> {
1980              if !preserved {
1981                  return Ok(ChildFilterDescription::all_unsupported(&parent_filters));
1982              }
1983              let mut description = ChildFilterDescription::from_child_with_column_mapping(
1984                  &parent_filters,
1985                  column_mapping,
1986                  child,
1987              )?;
1988              transfer_key_filters(&parent_filters, key_map, &mut description)?;
1989              Ok(description)
1990          };
1991
1992          let left_child =
1993              describe_child(left_preserved, left_mapping, &to_left, self.left())?;
1994          let mut right_child =
1995              describe_child(right_preserved, right_mapping, &to_right, self.right())?;
      // ... 1996–2002: comment on not re-creating a filter this join already carries ...
2003          if phase == FilterPushdownPhase::Post
2004              && self.dynamic_filter.is_none()
2005              && self.allow_join_dynamic_filter_pushdown(config)
2006          {
2007              // Add actual dynamic filter to right side (probe side)
2008              let dynamic_filter = Self::create_dynamic_filter(&self.on);
2009              right_child = right_child.with_self_filter(dynamic_filter);
2010          }
2011
2012          Ok(FilterDescription::new()
2013              .with_child(left_child)
2014              .with_child(right_child))
```

Three things happen. Lines 2003–2009: the join's *own* filter goes to the
right (probe) child. Lines 1983–1987 and 1992–1995: filters from *above* —
including dynamic filters other joins sent down — go to whichever child
produces their columns (`left_mapping` / `right_mapping`, built from the join's
output columns at `:1949-1961`), as long as that side is preserved
(`lr_is_preserved`, `:2765-2780`; inner joins preserve both). That includes the
**left, build** child. And lines 1970–1974 and 1988: for inner and semi joins
(`supports_key_transfer`, `:979-984`) a parent filter whose columns are all join
keys is rewritten over the *other* side's key expression and pushed there too
(`transfer_key_filters`, `:2710-2724`; `transfer_filter_across_keys`,
`:2731-2758`). A dynamic filter comes out of the rewrite as a view sharing the
original's state, "so it keeps tracking the build side" (`:2729-2730`). So a
dynamic filter from a join above reaches the scans on *both* sides of the join
below.

In lane 1's bad order, the rare_tag filter on t could therefore be routed into
has_tag even though has_tag is a build side — the step DuckDB's probe-spine walk
cannot take ([reading-duckdb-join-filters.md](reading-duckdb-join-filters.md)
Step 3). Whether the filter is *populated* before that build scan reads it
depends on when the upper join's build finishes relative to the lower join's;
the `lit(true)` placeholder keeps it correct either way. This guide did not
trace the execution order, so it claims the routing, not the cascade's effect.
Installing is conditional: `handle_child_pushdown_result` (`:2017-2065`) keeps
the filter on the join only if a consumer holding the same expression is found
in the probe subtree (lines 2043–2061); otherwise it skips the bounds
accumulation altogether.

### Step 4 — what the filter becomes: an IN-list or the hash table

> **In:** a finished build. **Out:** the membership predicate, and the
> thresholds that pick its form.

```rust
// datafusion/physical-plan/src/joins/hash_join/exec.rs:3356–3406 — choosing the membership predicate
3356      let membership = if num_rows == 0 || (!prepared && !should_compute_dynamic_filters) {
3357          PushdownStrategy::Empty
3358      } else {
      // ... 3359–3361: comment on the two forms ...
3362          let estimated_size = left_values
3363              .iter()
3364              .map(|arr| arr.get_array_memory_size())
3365              .sum::<usize>();
3366
3367          let pushdown_inlist = !left_values.is_empty()
3368              && !left_values[0].is_empty()
3369              && estimated_size <= config.optimizer.hash_join_inlist_pushdown_max_size
3370              && map.num_of_distinct_key()
3371                  <= config
3372                      .optimizer
3373                      .hash_join_inlist_pushdown_max_distinct_values;
3374
3375          if pushdown_inlist
3376              && let Some(in_list_values) = build_struct_inlist_values(&left_values)?
3377          {
3378              PushdownStrategy::InList(in_list_values)
3379          } else {
3380              // Past the InList threshold use a bucket bitmap for container pruning.
      // ... 3381–3403: build a key-range bitmap for container pruning ...
3404              PushdownStrategy::Map(Arc::clone(&map), pruning_bitmap)
3405          }
3406      };
```

The defaults are **128 KiB** (`config.rs:1875`) and **150 distinct values**
(`config.rs:1889`, "inspired by Trino's `max-filter-keys-per-column`"), per
partition; the distinct count is the build's deduplicated key count
(`map.num_of_distinct_key()`, line 3370). Worked on lane 1's bad order: the
rare_tag build has 10 distinct tags — an IN-list; has_tag after filtering has
1,000 rows with ~1,000 distinct posts — over 150, so the scan gets the hash
table (line 3404). The line to focus on is 3404: there is no Bloom filter in
this path at all — nothing in `hash_join/` builds one.

The `Map` form is evaluated by `HashTableLookupExpr::evaluate`
(`partitioned_hash_eval.rs:394-410`), which hashes the probe keys and calls
`contain_hashes` on the build's hash map — or, for a dense integer build,
`contain_keys` on an array map. It checks the 64-bit *hash*, so a false
positive needs a full 64-bit collision: with ~1,000 build keys, the chance a
given absent key collides is about 1,000 / 2⁶⁴ ≈ 5 × 10⁻¹⁷, against lane 1b's
0.5% for DuckDB's register-blocked filter at the same key count. The price is
the probe: a hash-table lookup per row instead of one cached word.

The `Map` variant also carries an optional **key-range bitmap**
(`KeyRangeBitmap`, `datafusion/physical-plan/src/joins/key_range_bitmap.rs:52-102`):
one bit per bucket of keys across [min, max], up to 2²⁰ bits (128 KiB, line 29),
built only for a single integer key column and only when it could exclude
something — it is skipped when twice the distinct-key count exceeds
`min(span, buckets)` (lines 84–88) or when every bucket would be set (lines
94–96). It is a bucketed cousin of DuckDB's range bitmap with one difference in
use: it never filters rows. `HashTableLookupExpr::evaluate` ignores it; the
pruning code asks "might a container with min–max `[lo, hi]` hold a build key?"
(`may_contain_range`, `key_range_bitmap.rs:127-144`) through
`build_hash_lookup_pruning_expr` (`datafusion/pruning/src/pruning_predicate.rs:1553-1558`),
so a Parquet row group whose range falls in a gap of the build keys can be
skipped.

### Step 5 — bounds, and combining per-partition filters

> **In:** Step 4's membership predicate. **Out:** the final expression the
> scan evaluates.

Each build partition also reports min/max **bounds** per key column.
`create_bounds_predicate` (`shared_bounds.rs:159-197`) turns them into
`col >= min AND col <= max`, and `combine_membership_and_bounds`
(`:203-216`) ANDs bounds before membership, so the cheap range test runs
first and lets a Parquet scan skip row groups — RPT+'s cascade filter
(min–max, then membership) without the Bloom filter. With hash-partitioned
builds, `build_partitioned_filter` (`:666-805`) emits a `CASE hash(key) % n WHEN
i THEN <partition i's filter>` so each probe row is checked only against the
partition it would join with (lines 776–800; a range-partitioned probe routes
with `RangeExpr` instead, lines 754–774). Empty partitions become `false`,
partitions whose build was cancelled stay `true` (lines 679–680, 701–707), and
when only one partition has rows its filter is used without routing (lines
729–732). Each probe stream reports its build and waits for every partition
before probing (`stream.rs:633-673`, state `WaitPartitionBoundsReport`, handled
at `:734-743`), so the published filter is complete before use.

### Step 6 — DataFusion beside the papers

> **In:** Steps 1–5. **Out:** what this design has and lacks, in the
> vocabulary of the three papers.

- **Exact, not approximate.** The `Map` filter is a semi-join with
  negligible false positives — Yannakakis' operator, not PT's Bloom filter.
  The PT paper's §1 objection (hash-table cost in the semi-join phase) is
  answered differently: the hash table is not built for the filter, it is
  the join's own, already built.
- **Forward only, one hop per join.** Each join creates a filter for its
  probe subtree; there is no backward pass and no separate phase, as in SQL
  Server (CIDR'26 §3.2).
- **Routing can enter build sides** (Step 3), by column ownership and by key
  transfer — the precondition for SQL Server's cascade, which DuckDB's walk
  lacks.
- **No run-time abandon rule** in this path: unlike DuckDB's
  selectivity-optional wrapper or RPT+'s dynamic pipeline, a dynamic filter
  that passes every row is still evaluated (a search of `hash_join/` for
  pause or selectivity logic finds none; the scan side was not surveyed).

## How to read the source (with the concepts in hand)

1. `dynamic_filters/mod.rs:61-98` then `:311-400` — the filter object.
2. `hash_join/exec.rs:962-1085` — creation, key transfer and the refusals.
3. `exec.rs:1918-2065` — routing (`gather_filters_for_pushdown`) and
   installing (`handle_child_pushdown_result`); `:2693-2780` for key transfer
   and `lr_is_preserved`.
4. `exec.rs:3356-3406` — membership choice; `config.rs:1860-1889` for the
   knobs' documentation; `joins/key_range_bitmap.rs`.
5. `shared_bounds.rs:90-216`, `:618-868`; `partitioned_hash_eval.rs:275-410`.
6. `stream.rs:626-743` — the probe waiting on the build.

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
  placeholder (`exec.rs:967`). When the build finishes, `update` swaps in the
  real predicate and bumps a generation counter (`mod.rs:317-345`); consumers
  can await it (`mod.rs:393-400`). A scan that runs before the update is
  unfiltered, never incorrect.

  </details>

- [ ] You can list when a hash join refuses to create a dynamic filter.

  <details><summary>Answer</summary>

  Probe side not preserved (outer-type joins), the
  `enable_join_dynamic_filter_pushdown` switch off (default true), a null-aware
  join whose build keys can be NULL, Hive-style preserved file partitions
  under partitioned mode, and a partitioned join without hash or range routing
  (`exec.rs:1037-1085`). A null-equal join is not refused: the filter is
  widened with `key IS NULL` when the build holds a NULL
  (`shared_bounds.rs:817-868`).

  </details>

- [ ] You can say how routing differs from DuckDB's and why that matters for the cascade.

  <details><summary>Answer</summary>

  DataFusion sends each parent filter to whichever child owns its columns, if
  that side is preserved — build (left) or probe (right) —
  (`exec.rs:1983-1995`), transfers filters on join keys to the other side of
  an inner or semi join (`:1970-1988`), and sends its own filter to the probe
  side (`:2003-2009`). DuckDB's search follows only probe children and stops at
  a scan without the column. Reaching build sides is what lets a bitmap from an
  upper join filter a lower join's build — the SQL Server cascade that took
  lane 1's bad order from 3,020,210 to 60,504 intermediates in lane 3's model.

  </details>

- [ ] You can state the IN-list thresholds and what replaces the IN-list above them.

  <details><summary>Answer</summary>

  IN-list if the build's key arrays total ≤ 128 KiB and ≤ 150 distinct keys
  per partition (`config.rs:1875`, `:1889`); otherwise a reference to the
  build's hash map, evaluated by `HashTableLookupExpr` with `contain_hashes`
  (`exec.rs:3362-3379, 3404`, `partitioned_hash_eval.rs:394-410`), plus an
  optional key-range bitmap used only for container pruning. No Bloom filter.

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

- `apache/datafusion` at `350ca7650`:
  `datafusion/physical-expr/src/expressions/dynamic_filters/mod.rs`,
  `datafusion/physical-plan/src/joins/hash_join/{exec.rs, shared_bounds.rs, partitioned_hash_eval.rs, stream.rs}`,
  `datafusion/physical-plan/src/joins/key_range_bitmap.rs`,
  `datafusion/pruning/src/pruning_predicate.rs`,
  `datafusion/common/src/config.rs`.
- The DataFusion blog post the source links (`mod.rs:67-69`), "Dynamic
  Filters: Passing Information Between Operators During Execution for 25x
  Faster Queries" (2025-09-10) — not read for this chapter; its 25× is not
  quoted here.
- Previous: [reading-duckdb-join-filters.md](reading-duckdb-join-filters.md).
  Back to the topic: [README.md](README.md).
