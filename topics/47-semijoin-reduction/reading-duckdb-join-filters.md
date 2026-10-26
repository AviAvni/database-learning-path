# DuckDB's join filters: min-max, IN-lists, exact bitmaps and a register-blocked Bloom filter

RPT+ was built on DuckDB v1.3.0 and describes its baseline as an engine that
"supports min-max filter transfer on join keys from the hash-table build side
to the probe side, but does not support Bloom filters" (RPT+ §6). DuckDB v2.0.0
is further along. A hash join can push four kinds of filter into a probe-side
scan — a min-max range, an IN-list, a bitmap over the key range, and a Bloom
filter — chooses among them at run time from what it actually built, and
pauses the filters that do not filter. This chapter reads that code and holds
it against the three papers: which of RPT+'s and SQL Server's ideas are here,
and which are not.

Every anchor is `duckdb/duckdb` at **v2.0.0 (`d54cbbe3df`)**, read with
`tools/pinned-source.py`. The revision is the `duckdb` row of the table in
[resources/codebases.md](../../resources/codebases.md).

## The problem in one sentence

A hash join knows, the moment its build finishes, exactly which key values
can match — how much of that knowledge does DuckDB hand to the scan on the
other side, in what form, and how far down the plan can it travel?

## The concepts, step by step

### Step 1 — the plan-time half: which joins get a filter

> **In:** a logical comparison join. **Out:** a `JoinFilterPushdownInfo`
> attached to it — the columns to filter, the scans to push into, and min/max
> aggregates to compute during the build.

The optimizer pass `JoinFilterPushdownOptimizer` visits every comparison
join (`src/optimizer/join_filter_pushdown_optimizer.cpp:416-420`) and calls
`GenerateJoinFilters`. The part that decides *where* filters can go:

```cpp
// src/optimizer/join_filter_pushdown_optimizer.cpp:317–414 — GenerateJoinFilters (excerpt)
317  void JoinFilterPushdownOptimizer::GenerateJoinFilters(LogicalComparisonJoin &join) {
318  	if (!JoinFilterPushdownUtil::JoinTypeIsSupported(join.join_type)) {
319  		return;
320  	}
     // ... 321–357: re-order the conditions; one pushdown column per =, <, <=, >, >= condition whose probe expression pushes down to a column (NOT DISTINCT FROM is skipped, line 339) ...
358  	// recurse the query tree to find the LogicalGets in which we can push the filter info
359  	vector<PushdownFilterTarget> pushdown_filter_targets;
360  	GetPushdownFilterTargets(*join.children[0], pushdown_columns, pushdown_filter_targets);
361  	for (auto &target : pushdown_filter_targets) {
362  		auto &get = target.get;
     // ... 363–406: attach a dynamic filter set to each target scan; one min and one max aggregate per condition ...
407  	if (!pushdown_info->probe_info.empty()) {
408  		const idx_t child_idx = join.children[1]->type == LogicalOperatorType::LOGICAL_DELIM_GET ? 0 : 1;
409  		pushdown_info->build_side_has_filter = IsFiltering(join.children[child_idx]);
410  	}
```

The line to focus on is 360: the search for target scans starts at
`children[0]`, the **probe** child. A **dynamic filter** is a predicate whose
value is not known when the plan is made; the scan holds an empty slot that the
join fills in at run time. Line 409 records whether the build side has any
filter of its own — a scan with table filters, a FILTER or a TOP-N below it
(`IsFiltering`, `:295-315`) — which Step 4 uses to decide on a Bloom filter.
That is the same question RPT+'s §3.6 asks: a filter built from an unfiltered
build side rarely filters anything.

### Step 2 — which join types are allowed

> **In:** Step 1's gate at line 318. **Out:** the join types excluded, and
> why they are exactly the ones the IN-predicate chapter warns about.

```cpp
// src/optimizer/join_filter_pushdown_optimizer.cpp:128–144 — JoinTypeIsSupported
128  bool JoinFilterPushdownUtil::JoinTypeIsSupported(JoinType join_type) {
129  	switch (join_type) {
130  	case JoinType::MARK:
131  	case JoinType::SINGLE:
132  	case JoinType::LEFT:
133  	case JoinType::OUTER:
134  	case JoinType::ANTI:
135  	case JoinType::RIGHT_ANTI:
136  		// cannot generate join filters for these join types
137  		// mark/single - cannot change cardinality of probe side
138  		// left/outer always need to include every row from probe side
139  		// FIXME: anti/right_anti - we could do this, but need to invert the join conditions
140  		return false;
141  	default:
142  		return true;
143  	}
144  }
```

Line 130 is the one this topic cares about: a **mark join** emits a
TRUE/FALSE/NULL marker for *every* probe row, so dropping probe rows would be
wrong — the correctness boundary [reading-in-predicates.md](reading-in-predicates.md)
Step 3 derives. Inner and semi joins get filters; anything that must keep or
mark every probe row does not.

### Step 3 — how far a filter travels: only down the probe spine

> **In:** the probe child from line 360. **Out:** the set of scans a filter
> can reach, and the plan shape that defeats it.

```cpp
// src/optimizer/join_filter_pushdown_optimizer.cpp:161–293 — GetPushdownFilterTargets (excerpt)
161  void JoinFilterPushdownOptimizer::GetPushdownFilterTargets(LogicalOperator &op,
162                                                             vector<JoinFilterPushdownColumn> columns,
163                                                             vector<PushdownFilterTarget> &targets) {
164  	auto &probe_child = op;
165  	switch (probe_child.type) {
166  	case LogicalOperatorType::LOGICAL_LIMIT:
167  	case LogicalOperatorType::LOGICAL_TOP_N:
     // ... 168–170: LIMIT / TOP_N stop the search (a filter below them would change which rows survive the limit) ...
171  	case LogicalOperatorType::LOGICAL_ORDER_BY:
     // ... 172–177: ORDER BY, DISTINCT and CROSS PRODUCT recurse into the left child ...
178  	case LogicalOperatorType::LOGICAL_FILTER:
179  	case LogicalOperatorType::LOGICAL_COMPARISON_JOIN:
180  		GetPushdownFilterTargets(*probe_child.children[0], std::move(columns), targets);
181  		break;
     // ... 182–226: UNNEST, and UNION / EXCEPT / INTERSECT (into each child, EXCEPT only the first) ...
227  	case LogicalOperatorType::LOGICAL_GET: {
228  		// found LogicalGet
229  		auto &get = probe_child.Cast<LogicalGet>();
230  		if (!get.function.filter_pushdown) {
231  			// filter pushdown is not supported - no need to consider this node
232  			return;
233  		}
234  		get.ResolveOperatorTypes();
235  		const auto bindings = get.GetColumnBindings();
236  		for (auto &filter : columns) {
237  			if (filter.probe_column_index.table_index != get.table_index) {
238  				// the filter does not apply to the probe side here - bail-out
239  				return;
240  			}
     // ... 241–260: record the scan's storage type; the scan becomes a target ...
261  	case LogicalOperatorType::LOGICAL_PROJECTION: {
262  		auto &proj = probe_child.Cast<LogicalProjection>();
     // ... 263–268: a PROJECTION passes a column through only if its expression pushes down ...
269  		GetPushdownFilterTargets(*probe_child.children[0], std::move(projected_columns), targets);
270  		break;
272  	case LogicalOperatorType::LOGICAL_AGGREGATE_AND_GROUP_BY: {
273  		// we can push filters through aggregates IF they all point to groups
274  		auto &aggr = probe_child.Cast<LogicalAggregate>();
275  		for (auto &filter : columns) {
276  			if (filter.probe_column_index.table_index != aggr.group_index) {
277  				// index does not refer to a group - bail-out
278  				return;
279  			}
     // ... 280–285: the group expression must push down too ...
286  		GetPushdownFilterTargets(*probe_child.children[0], std::move(columns), targets);
287  		break;
288  	}
289  	default:
290  		// unsupported child type
291  		break;
292  	}
293  }
```

Lines 178–180 are the argument: when the search meets another comparison
join it goes into that join's `children[0]` — its probe side — and nowhere
else; when it reaches a scan that does not produce the filtered column, it
gives up (lines 237–240). The walk also passes through projections and through
a GROUP BY when the filtered columns are group keys (lines 261–288); these let
the filter reach a scan through more plan shapes, but they never turn the walk
toward a build child. A filter therefore reaches only scans at the bottom of
the **probe spine** — the chain of probe children below the join, bridged by
projections, filters and group-bys.

Lane 3 models exactly this as `Pushdown::ProbeSpineOnly`. On lane 1's bad
order `((knows ⋈ likes) ⋈ has_tag) ⋈ rare_tag`, the rare_tag filter is on t,
which only has_tag produces — but has_tag is the *build* side of the middle
join, so the search walks past it to knows and bails out. The lane-3
reference: probe-spine-only leaves the bad order at **3,020,210**
intermediates, while the SQL Server cascade into lower builds brings it to
**60,504** ([reading-sqlserver-bitmaps.md](reading-sqlserver-bitmaps.md)
Step 4). That is a model of this code path, not a DuckDB measurement; DuckDB's
own join-order optimizer would also not choose that plan. But it shows which
half of Yannakakis this design has: one hop down the probe spine, no cascade
through build sides, no backward pass.

### Step 4 — the run-time half: choosing a filter from what was built

> **In:** a finished build: its min, its max, its row count and the hash
> table. **Out:** the filters actually pushed, in the order the code tries
> them.

When the build finishes, `FinalizeFilters` runs once per pushed column
(`src/execution/operator/join/physical_hash_join.cpp:1837-1967`). For a
comparison other than equality it pushes a half-open min-max filter and stops
(`:1903-1909`). For an equality join with min ≠ max the branch at `:1911-1962`
runs these steps in order:

1. **IN-list**, if the build has 2–50 rows (`CanUseInFilter`, `:1433-1437`;
   `dynamic_or_filter_threshold` defaults to 50, `src/common/settings.json:721-725`).
   `PushInFilter` (`:1745-1795`) returns without pushing when the values contain
   NULL or form a dense range (line 1786), and otherwise pushes the list as an
   **optional** filter (line 1792). An optional filter has a select callback
   that passes every row (`src/planner/filter/table_filter_optional_function.cpp:53-55`)
   and a prune callback that consults only the storage statistics (`:69-82`):
   it can skip a zone-map block, and it never rejects a row.
2. **Bitmap over the key range** ("prefix-range filter"), if
   `CanUsePrefixRangeFilter` allows it (`:1486-1502`) and the key span is below
   2²⁶ (`:1923-1924`), or no larger than the bit budget a Bloom filter would get
   (`:1931-1942`). `CanUsePrefixRangeFilter` first calls `CanUseBloomFilter` —
   the build-to-probe ratio rules below apply to the bitmap too — and then
   requires an equality on a supported key type (8- to 64-bit integers and, outside debug builds, VARCHAR; `PrefixRangeFilter::SupportedType`, `table_filter_prefix_range_function.cpp:557-579`). When the span
   fits, the bitmap has one bit per value in [min, max] (`shift` stays 0,
   `table_filter_prefix_range_function.cpp:63-73`): SQL Server's exact bit-vector
   "if the ranges … are compact" (CIDR'26 §3.1), zero false positives. When the
   span is larger than the budget, `shift` grows until the buckets fit, and a
   set bit then means "some key in this 2^shift-wide bucket". If it is
   registered, the code `continue`s: no min-max, no Bloom filter for this column
   (lines 1924–1929, 1937–1942). An IN-list pushed in step 1 stays.
3. Otherwise **min-max** (`:1945-1948`, skipped if step 1 pushed an IN-list) and,
   if allowed, a **Bloom filter** (`:1949-1962`). For an equality join the
   min-max pair is pushed through `CreateDynamicMinMaxFilter` with
   `selectivity_optional` false, which builds the same *optional*,
   statistics-only filter as the IN-list (lines 1700–1704). So the filters that
   reject rows one by one are the bitmap, the Bloom filter, and the half-open
   min-max of non-equality joins; the IN-list and the equality min-max prune
   zone-map blocks.

The Bloom decision:

```cpp
// src/execution/operator/join/physical_hash_join.cpp:1464–1483 — inside CanUseBloomFilter
1464  	auto build_count = ht->Count();
1465  	if (build_count == 0) {
1466  		build_count = ht->GetSinkCollection().Count();
1467  	}
1468  	auto probe_cardinality = op.children[0].get().estimated_cardinality;
1469  	if (build_side_has_filter) {
1470  		probe_cardinality = MaxValue(info.estimated_scan_cardinality, probe_cardinality);
1471  	}
1472  	const double build_to_probe_ratio = static_cast<double>(build_count) / static_cast<double>(probe_cardinality);
      // ... 1473–1476: refuse if the ratio exceeds BUILD_TO_PROBE_RATIO_THRESHOLD (1.0, line 355) ...
1477  	// if we have a build side without a filter, only build the bloom filter if the ratio/size is small enough
1478  	if (!build_side_has_filter && build_to_probe_ratio > NON_FILTERING_RATIO_THRESHOLD &&
1479  	    build_count > NON_FILTERING_BUILD_SIDE_THRESHOLD) {
1480  		return false;
1481  	}
1482
1483  	return true;
```

The line to focus on is 1478, with its constants on lines 355–358. Worked: a
build of 5,000,000 rows against a probe estimated at 20,000,000 has ratio
0.25. With a filtered build side, the filter is built. With an *unfiltered*
build side, 0.25 > 0.1 and 5,000,000 > 4,194,304, so it is not — the RPT+
§3.6 PK–FK argument as two constants. Note the denominator on lines
1468–1472: it is the probe side's **estimated** cardinality, the number this
whole topic distrusts — and when the build side has a filter, it is the larger
of that estimate and the scan's own estimate (`estimated_scan_cardinality`,
set at `join_filter_pushdown_optimizer.cpp:371-373`). Earlier lines
(`:1446-1462`) also require exactly one equality condition; since v2.0.0 an
`IS NOT DISTINCT FROM` condition counts as an equality there.

Which thread builds the Bloom filter also depends on the join. An in-memory
join fills it during finalize (`join_hashtable.cpp:1075-1079` creates it once
the real build count is known; `InsertHashes` at `:1004-1009` fills it). A join
that is likely to spill — its estimated build exceeds a quarter of the memory
budget (`JoinIsLikelyExternal`, `physical_hash_join.cpp:363-374`) — fills it
during `Sink`, because a spilling finalize only ever sees some partitions
(`ShouldBuildBloomFilterInSink`, `:376-405`; the insert is
`join_hashtable.cpp:717-720`).

### Step 5 — the Bloom filter: register-blocked, sized at 12 bits per key, then folded

> **In:** Step 4's decision to build one. **Out:** its layout, its size rule,
> what happens to the size when it is sealed, and its measured accuracy against
> a classic filter.

```cpp
// src/planner/filter/table_filter_bloom_function.cpp:27–33, 133–150 — sizing and the bit mask
 27  static constexpr idx_t MAX_NUM_SECTORS = (1ULL << 26);
 28  static constexpr idx_t MIN_NUM_BITS_PER_KEY = 12;
 29  static constexpr idx_t MIN_NUM_BITS = 512;
 30  static constexpr idx_t LOG_SECTOR_SIZE = 6;             // a sector is 64 bits, log2(64) = 6
 31  static constexpr idx_t SHIFT_MASK = 0x3F3F3F3F3F3F3F3F; // 6 bits for 64 positions
 32  static constexpr idx_t N_BITS = 4;                      // the number of bits to set per hash
 33  static constexpr idx_t BITS_PER_SECTOR = 1ULL << LOG_SECTOR_SIZE;
     // ... 34–132: Initialize (capped at 1/64 of the memory budget), Fold, Density, Seal ...
133  idx_t BloomFilter::GetNumberOfSectors(idx_t number_of_rows) {
134  	const idx_t min_bits = MaxValue(MIN_NUM_BITS, number_of_rows * MIN_NUM_BITS_PER_KEY);
135  	return MinValue(NextPowerOfTwo(min_bits) >> LOG_SECTOR_SIZE, MAX_NUM_SECTORS);
136  }
     // ... 137 ...
138  inline uint64_t GetMask(const hash_t hash) {
139  	const uint64_t shifts = hash & SHIFT_MASK;
140  	const auto shifts_8 = reinterpret_cast<const uint8_t *>(&shifts);
141
142  	uint64_t mask = 0;
143
144  	for (idx_t bit_idx = 8 - N_BITS; bit_idx < 8; bit_idx++) {
145  		const uint8_t bit_pos = shifts_8[bit_idx];
146  		mask |= (1ULL << bit_pos);
147  	}
148
149  	return mask;
150  }
```

A **sector** is one 64-bit word. Insert and lookup (`InsertOne`, `LookupOne`,
`:203-226`) pick the sector with `hash & bitmask` — the low bits — and set or
test four bits whose positions are four 6-bit fields of the hash's high bytes
(line 144). That is a **register-blocked** Bloom filter: one word per probe,
four bits per key, whatever the size. Lines 134–135 size it: at least 12 bits
per row, rounded up to a power of two. Worked: 1,000 build rows → 12,000 bits →
16,384 bits = 256 sectors, 16.38 bits per key; 10,000 rows → 131,072 bits,
13.11 per key. On the build side the hashes are the hash table's own — the
`Hash` call at `join_hashtable.cpp:717` feeds both the table and the filter, and
`InsertHashes` at `:1004-1009` does the same on the finalize path — so building
the filter costs no extra hashing. The scan side does hash: `SelectBloomFilter`
calls `VectorOperations::Hash` on each vector of key values
(`table_filter_bloom_function.cpp:271`) before the lookup.

The 12 bits per key is where the size starts, not necessarily where it ends.
`Seal` (`:91-131`) runs when the build is complete. It reads the density of the
bit array — the fraction of bits set — and halves the filter, OR-ing sector
`i` onto sector `i & new_mask` (`Fold`, `:61-72`), for as long as the false
positive rate that the *folded* density implies, density⁴, stays at or below
`TargetFalsePositiveRate()` (`:86-89`; 4 bits per key and 12 bits per key give
(1 − e^(−4/12))⁴ ≈ 0.646%). A filter sized from an over-estimate therefore
shrinks to what the real key set needs: worked from the formulas, 1,000 keys in
a filter sized for an estimate of 100,000 rows (2,097,152 bits = 32,768
sectors) have an expected density of 0.19%, and seven folds bring it to 256
sectors, density 21.7%, density⁴ ≈ 0.22% — the same 16,384-bit layout as an
exactly-sized 1,000-key filter. That arithmetic uses the expected density
1 − e^(−4n/m), not a run of the code. Finally `Seal` sets `useful` to whether
density⁴ is below the Bloom pause threshold of 0.5 (line 130); a filter that
ended up that dense is not published (`CreateRuntimeFilterExpression`,
`physical_hash_join.cpp:1631-1633`), because it would be paused on its first
vectors anyway.

Lane 1b builds the 12-bits-per-key layout (with this crate's hash, not
DuckDB's) and a classic Bloom filter at the same bit budget. Its sizes are
exact (n keys, no duplicates, no over-estimate), so `Seal` would not fold any
of them — for 1,000, 10,000 and 100,000 keys the folded density⁴ is 2.2%,
4.4% and 1.0%, all above the 0.646% target:

```
      keys filter                                 bits  bits/key   k  measured FPR    formula
      1000 classic Bloom                         16384     16.38  11       0.00045    0.00038
      1000 register-blocked (DuckDB layout)      16384     16.38   4       0.00548          —
     10000 classic Bloom                        131072     13.11   9       0.00181    0.00184
     10000 register-blocked (DuckDB layout)     131072     13.11   4       0.00901          —
```

About **12×** and **5×** more false positives for the same memory, bought for
one memory access per probe. For a single-hop filter that trade is usually
right; over several hops (RPT+ §4.2) it compounds.

### Step 6 — publishing, and pausing filters that do not filter

> **In:** Step 4's deferred filters. **Out:** when the probe-side scan sees
> them, and the run-time rule that switches them off — and back on.

Bloom and bitmap filters are published when the hash table is finalized:
`PublishDeferredRuntimeFilters` (`physical_hash_join.cpp:1659-1687`) is called
from `FinishTasks` at `:1259-1264` after `SealBloomFilter`, and each is wrapped
as **selectivity-optional** — a filter that monitors its own pass rate
(`:1668-1679`). The half-open min-max filter of a non-equality join gets the same
wrapper (`:1695-1699`). The thresholds:

```cpp
// src/planner/filter/table_filter_functions.cpp:36–44 — GetThresholdAndVectorsToCheck (excerpt)
36  void GetThresholdAndVectorsToCheck(SelectivityOptionalFilterType type, float &selectivity_threshold,
37                                     idx_t &n_vectors_to_check) {
38  	static constexpr float MIN_MAX_THRESHOLD = 0.9f;
39  	static constexpr float BF_THRESHOLD = 0.5f;
40  	static constexpr float PRF_THRESHOLD = 0.5f;
    // ... 41 ...
42  	static constexpr idx_t MIN_MAX_CHECK_N = 6;
43  	static constexpr idx_t BF_CHECK_N = 6;
44  	static constexpr idx_t PRF_CHECK_N = 6;
```

and the rule that applies them:

```cpp
// src/planner/filter/table_filter_selectivity_optional_function.cpp:25–46 — SelectivityStats::Update
25  void SelectivityOptionalFilterState::SelectivityStats::Update(idx_t accepted, idx_t processed) {
26  	vectors_processed++;
27  	tuples_accepted += accepted;
28  	tuples_processed += processed;
    // ... 29 ...
30  	static constexpr idx_t VECTOR_PAUSE = 10;
31  	D_ASSERT(n_vectors_to_check < VECTOR_PAUSE);
32  	if (vectors_processed == MaxValue<idx_t>(pause_multiplier, 1) * VECTOR_PAUSE) {
33  		vectors_processed = 0;
34  		tuples_accepted = 0;
35  		tuples_processed = 0;
36  		status = FilterStatus::ACTIVE;
37  	} else if (status == FilterStatus::ACTIVE && vectors_processed >= n_vectors_to_check) {
38  		// pause the filter if we processed enough vectors and the selectivity is too high
39  		if (GetSelectivity() >= selectivity_threshold) {
40  			status = FilterStatus::PAUSED_DUE_TO_HIGH_SELECTIVITY;
41  			pause_multiplier++; // increase the pause duration
42  		} else {
43  			pause_multiplier = 0; // selective enough, reset the pause duration
44  		}
45  	}
46  }
```

This is RPT+'s selective probing (§5.3) with DuckDB's constants: sample 6
vectors (DuckDB's vector is 2,048 rows, `src/include/duckdb/common/vector_size.hpp:16`), and stop
probing a Bloom filter or bitmap that passes ≥ 50% of rows — or a non-equality
min-max filter that passes ≥ 90% (RPT+ uses τ_stop = 0.9 on the *ratio*
between consecutive filters). While a filter is paused, the Bloom select
function calls `Update(0, 0)` once per vector
(`table_filter_bloom_function.cpp:330-333`); so does the wrapper
(`table_filter_selectivity_optional_function.cpp:143-146`) and the bitmap
function (`table_filter_prefix_range_function.cpp:650-651`). Each such call
increments `vectors_processed` (line 26), and line 32 resets the counters and
turns the filter back on when `vectors_processed` reaches
`max(pause_multiplier, 1) × 10`. The multiplier goes up by one only when an
*active* filter is paused (lines 37 and 41), so each pause is longer than the
one before. Tracing it for a filter that passes every row (a line-by-line
transcription run in Python while writing this chapter):

```
vectors   filter state   pause_multiplier after   line-32 target
  1-6     evaluated              1 (at vector 6)         10
  7-10    skipped                1                       10   -> reset at vector 10
 11-16    evaluated              2 (at vector 16)        20
 17-30    skipped                2                       20   -> reset at vector 30
 31-36    evaluated              3 (at vector 36)        30
 37-60    skipped                3                       30   -> reset at vector 60
```

The skipped runs are 4, 14, 24, 34, … vectors long (10·m − 6 after the m-th
pause); the evaluated runs are always 6 vectors. A filter that stays
unselective is therefore evaluated for a shrinking share of the scan — 6 of the
first 10 vectors, 12 of the first 30, 18 of the first 60 — and is never off
for good: the same end as RPT+'s "disable further probing of that filter",
reached by backing off rather than by a one-way switch. When a check finds the
filter selective again, line 43 resets the multiplier to 0 and the pause
length starts over. This is a reading of the code, not a DuckDB measurement.

## How to read the source (with the concepts in hand)

1. `src/optimizer/join_filter_pushdown_optimizer.cpp` top to bottom — 424
   lines; Steps 1–3.
2. `src/include/duckdb/execution/operator/join/join_filter_pushdown.hpp` —
   the data that crosses from plan time to run time.
3. `physical_hash_join.cpp:1433-1502` (the `Can…` predicates) and
   `:1837-1967` — Step 4's choices, then `:1259-1264` and `:1659-1687` for
   publication; `:363-405` for the spilling-join variant.
4. `src/planner/filter/table_filter_bloom_function.cpp` — Step 5;
   `table_filter_prefix_range_function.cpp:60-125` for the bitmap.
5. `table_filter_functions.cpp:36-62` and
   `table_filter_selectivity_optional_function.cpp:25-46` — Step 6.

## Questions (answer in notes.md)

1. Rewrite `GetPushdownFilterTargets` so a filter can enter a comparison
   join's *build* child when that child produces the column. Under what
   execution-order condition is the result correct? (Hint: SQL Server's
   pull order, and DuckDB's pipelines.)
2. The IN-list and the equality min-max are pushed as optional filters: they
   prune zone-map blocks and never reject a row. What would row-by-row
   evaluation of a 50-value IN-list cost per row, compared with the
   register-blocked Bloom filter's one word per probe?
3. Step 4's exact bitmap triggers for spans under 2²⁶ bits (8 MiB), provided
   `CanUsePrefixRangeFilter` (and so `CanUseBloomFilter`) agrees. For lane
   1's keys (person ids 0–19,999, post ids 0–49,999), which filter would
   DuckDB choose for each join of the bad order?
4. The pause lengths in Step 6 grow as 4, 14, 24, … vectors. What probe
   column makes that a regression? (Hint: a column whose first 6 vectors all
   match and whose later vectors almost never do — how many selective
   vectors run unfiltered before the filter is evaluated again?)

## Done when

Answer each before unfolding it.

- [ ] You can say where DuckDB decides that a join gets a filter, and what it records for run time.

  <details><summary>Answer</summary>

  `JoinFilterPushdownOptimizer::GenerateJoinFilters`
  (`join_filter_pushdown_optimizer.cpp:317-414`): skip unsupported join types,
  make one pushdown column per comparison condition whose probe expression is a
  column, find target scans below the probe child (line 360), attach a dynamic
  filter set to each, add a min and a max aggregate per condition, and record
  whether the build side filters anything (lines 407–410).

  </details>

- [ ] You can explain why a filter cannot reach a build side below another join, citing lines.

  <details><summary>Answer</summary>

  `GetPushdownFilterTargets` handles a comparison join by recursing into
  `children[0]` only (lines 178–180) — the probe child — and when it reaches a
  scan that does not produce the filtered column it returns (lines 237–240).
  It also passes through projections and group-by keys (lines 261–288), but
  never into a build child. A column produced by a lower join's build side is
  never reached. Lane 3's probe-spine-only model of this leaves lane 1's bad
  order at 3,020,210 intermediates.

  </details>

- [ ] You can list the filter kinds in the order `FinalizeFilters` tries them and the condition for each.

  <details><summary>Answer</summary>

  min = max: a single comparison. A non-equality comparison: half-open min-max.
  Equality: an IN-list if the build has 2–50 rows (skipped for NULLs or dense
  ranges; pushed as an optional, statistics-only filter); then an exact or
  bucketed bitmap if `CanUsePrefixRangeFilter` passes and the key span is under
  2²⁶ or within the Bloom bit budget — which ends the column's processing;
  else min-max (optional, unless an IN-list was pushed) plus, if
  `CanUseBloomFilter`, a deferred Bloom filter
  (`physical_hash_join.cpp:1892-1962`).

  </details>

- [ ] You can apply `CanUseBloomFilter`'s thresholds to a case.

  <details><summary>Answer</summary>

  Needs one equality condition and build/probe ≤ 1.0, where the probe figure is
  the estimated probe cardinality (the larger of that and the scan's estimate
  when the build side has a filter); an unfiltered build side is further
  refused if the ratio > 0.1 and the build > 4,194,304 rows (lines 1468–1483).
  5 M build vs 20 M estimated probe: ratio 0.25 — built if the build side has a
  filter, refused if not.

  </details>

- [ ] You can describe the Bloom filter's layout and compare its accuracy with a classic filter.

  <details><summary>Answer</summary>

  Register-blocked: one 64-bit sector chosen by the hash's low bits, 4 bits
  set inside it from four 6-bit fields of the high bytes; sized at ≥ 12 bits per
  key rounded to a power of two (lines 27–33, 133–150), then folded at `Seal`
  while the folded density⁴ stays within about 0.646%. Lane 1b at equal bits
  (no fold applies): 1,000 keys (16.38 bits/key) 0.548% vs 0.045% classic;
  10,000 keys (13.11) 0.901% vs 0.181%.

  </details>

- [ ] You can state the pause rule and what the trace shows about resuming.

  <details><summary>Answer</summary>

  After 6 vectors, a Bloom or bitmap filter passing ≥ 50% of rows, or a
  non-equality min-max filter passing ≥ 90%, is paused
  (`table_filter_functions.cpp:38-44`,
  `table_filter_selectivity_optional_function.cpp:37-41`). While paused, each
  vector increments `vectors_processed`; when it reaches
  `max(pause_multiplier, 1) × 10` (line 32) the counters reset and the filter is
  active again. The multiplier rises by one each time an active filter is
  paused, so a filter that stays unselective is skipped for 4, 14, 24, …
  vectors between 6-vector evaluation windows, and a selective check resets the
  multiplier to 0.

  </details>

## References

- `duckdb/duckdb` at v2.0.0 (`d54cbbe3df`): `src/optimizer/join_filter_pushdown_optimizer.cpp`,
  `src/execution/operator/join/physical_hash_join.cpp`,
  `src/execution/join_hashtable.cpp`,
  `src/planner/filter/table_filter_bloom_function.cpp`,
  `src/planner/filter/table_filter_prefix_range_function.cpp`,
  `src/planner/filter/table_filter_optional_function.cpp`,
  `src/planner/filter/table_filter_functions.cpp`,
  `src/planner/filter/table_filter_selectivity_optional_function.cpp`.
- Qiao, Boncz, Zhang, RPT+ (VLDB'26) — §3.6, §4.2, §5.3, §6 (the v1.3.0
  baseline). [reading-rpt-plus.md](reading-rpt-plus.md).
- Zhao et al., SQL Server bitmaps (CIDR'26) — §3.1 exact bit-vectors.
  [reading-sqlserver-bitmaps.md](reading-sqlserver-bitmaps.md).
- Next: [reading-datafusion-dynamic-filters.md](reading-datafusion-dynamic-filters.md).
