# DuckDB's join filters: min-max, IN-lists, exact bitmaps and a register-blocked Bloom filter

RPT+ was built on DuckDB v1.3.0 and describes its baseline as an engine that
"supports min-max filter transfer on join keys from the hash-table build side
to the probe side, but does not support Bloom filters" (RPT+ §6). DuckDB has
moved since. At the commit this repo pins, a hash join can push four kinds of
filter into a probe-side scan — a min-max range, an IN-list, an exact bitmap
over the key range, and a Bloom filter — chooses among them at run time from
what it actually built, and pauses filters that do not filter. This chapter
reads that code and holds it against the three papers: which of RPT+'s and
SQL Server's ideas are here, and which are not.

Every anchor is `duckdb/duckdb` at **`6c0c1a68`** (2026-07-10), read with
`tools/pinned-source.py`. The pin is the `duckdb` row of the table in
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
join (`src/optimizer/join_filter_pushdown_optimizer.cpp:390-396`) and calls
`GenerateJoinFilters`. The part that decides *where* filters can go:

```cpp
// src/optimizer/join_filter_pushdown_optimizer.cpp:294–388 — GenerateJoinFilters (excerpt)
294  void JoinFilterPushdownOptimizer::GenerateJoinFilters(LogicalComparisonJoin &join) {
295  	if (!JoinFilterPushdownUtil::JoinTypeIsSupported(join.join_type)) {
296  		return;
297  	}
     // ... 298–334: one pushdown column per =, <, <=, >, >= condition whose probe expression is a column ...
335  	// recurse the query tree to find the LogicalGets in which we can push the filter info
336  	vector<PushdownFilterTarget> pushdown_filter_targets;
337  	GetPushdownFilterTargets(*join.children[0], pushdown_columns, pushdown_filter_targets);
     // ... 338–380: attach a dynamic filter set to each target scan; one min and one max aggregate per condition ...
381  	if (!pushdown_info->probe_info.empty()) {
382  		const idx_t child_idx = join.children[1]->type == LogicalOperatorType::LOGICAL_DELIM_GET ? 0 : 1;
383  		pushdown_info->build_side_has_filter = IsFiltering(join.children[child_idx]);
384  	}
```

The line to focus on is 337: the search for target scans starts at
`children[0]`, the **probe** child. A **dynamic filter** is a predicate whose
value is not known when the plan is made; the scan holds an empty slot that the
join fills in at run time. Line 383 records whether the build side has any
filter of its own — a scan with table filters, a FILTER or a TOP-N below it
(`IsFiltering`, `:272-292`) — which Step 4 uses to decide on a Bloom filter.
That is the same question RPT+'s §3.6 asks: a filter built from an unfiltered
build side rarely filters anything.

### Step 2 — which join types are allowed

> **In:** Step 1's gate at line 295. **Out:** the join types excluded, and
> why they are exactly the ones the IN-predicate chapter warns about.

```cpp
// src/optimizer/join_filter_pushdown_optimizer.cpp:110–126 — JoinTypeIsSupported
110  bool JoinFilterPushdownUtil::JoinTypeIsSupported(JoinType join_type) {
111  	switch (join_type) {
112  	case JoinType::MARK:
113  	case JoinType::SINGLE:
114  	case JoinType::LEFT:
115  	case JoinType::OUTER:
116  	case JoinType::ANTI:
117  	case JoinType::RIGHT_ANTI:
118  		// cannot generate join filters for these join types
119  		// mark/single - cannot change cardinality of probe side
120  		// left/outer always need to include every row from probe side
121  		// FIXME: anti/right_anti - we could do this, but need to invert the join conditions
122  		return false;
123  	default:
124  		return true;
125  	}
126  }
```

Line 112 is the one this topic cares about: a **mark join** emits a
TRUE/FALSE/NULL marker for *every* probe row, so dropping probe rows would be
wrong — the correctness boundary [reading-in-predicates.md](reading-in-predicates.md)
Step 3 derives. Inner and semi joins get filters; anything that must keep or
mark every probe row does not.

### Step 3 — how far a filter travels: only down the probe spine

> **In:** the probe child from line 337. **Out:** the set of scans a filter
> can reach, and the plan shape that defeats it.

```cpp
// src/optimizer/join_filter_pushdown_optimizer.cpp:143–270 — GetPushdownFilterTargets (excerpt)
143  void JoinFilterPushdownOptimizer::GetPushdownFilterTargets(LogicalOperator &op,
144                                                             vector<JoinFilterPushdownColumn> columns,
145                                                             vector<PushdownFilterTarget> &targets) {
146  	auto &probe_child = op;
147  	switch (probe_child.type) {
     // ... 148–152: LIMIT / TOP_N stop the search ...
153  	case LogicalOperatorType::LOGICAL_FILTER:
154  	case LogicalOperatorType::LOGICAL_ORDER_BY:
155  	case LogicalOperatorType::LOGICAL_DISTINCT:
156  	case LogicalOperatorType::LOGICAL_COMPARISON_JOIN:
157  	case LogicalOperatorType::LOGICAL_CROSS_PRODUCT:
158  		// does not affect probe side - recurse into left child
159  		// FIXME: we can probably recurse into more operators here (e.g. window, unnest)
160  		GetPushdownFilterTargets(*probe_child.children[0], std::move(columns), targets);
161  		break;
     // ... 162–206: UNNEST, and UNION/EXCEPT/INTERSECT (into each child) ...
207  	case LogicalOperatorType::LOGICAL_GET: {
208  		// found LogicalGet
209  		auto &get = probe_child.Cast<LogicalGet>();
     // ... 210–215 ...
216  		for (auto &filter : columns) {
217  			if (filter.probe_column_index.table_index != get.table_index) {
218  				// the filter does not apply to the probe side here - bail-out
219  				return;
220  			}
```

Lines 156 and 160 are the argument: when the search meets another join, it
goes into that join's `children[0]` — its probe side — and nowhere else; when
it reaches a scan that does not produce the filtered column, it gives up
(lines 217–219). A filter therefore reaches only the scan at the bottom of
the **probe spine** — the chain of probe children below the join.

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
(`src/execution/operator/join/physical_hash_join.cpp:1581-1700`). For an
equality join with min ≠ max:

1. **IN-list**, if the build has 2–50 rows (`CanUseInFilter`, `:1245-1249`;
   `dynamic_or_filter_threshold` defaults to 50 in `src/common/settings.json:598-602`).
   `PushInFilter` (`:1251-1290`) skips it when the values contain NULL or form
   a dense range, and pushes it as an *optional* filter — "so that we can use
   it for zonemap pruning only, the IN-list is expensive to execute otherwise"
   (`:1283-1284`).
2. **Exact range bitmap** ("prefix-range filter"), if the key span is below
   2²⁶ (`:1664-1670`), or no larger than the bit budget a Bloom filter would
   get (`:1672-1684`). The bitmap has one bit per value in [min, max] when the
   span fits (`table_filter_prefix_range_function.cpp:63-79`, shift stays 0):
   SQL Server's exact bit-vector "if the ranges … are compact" (CIDR'26 §3.1),
   zero false positives.
3. Otherwise **min-max** (`:1686-1689`) and, if allowed, a **Bloom filter**
   (`:1690-1695`).

The Bloom decision:

```cpp
// src/execution/operator/join/physical_hash_join.cpp:1316–1337 — inside CanUseBloomFilter
1316  	// building the bloom filter is costly on the build to make probing faster,
1317  	// so only use it if there are less build tuples than probing tuples
1318  	static constexpr double BUILD_TO_PROBE_RATIO_THRESHOLD = 1.0;
1319  	auto build_count = ht->Count();
     // ... 1320–1322: fall back to the sink collection's count ...
1323  	const double build_to_probe_ratio =
1324  	    static_cast<double>(build_count) / static_cast<double>(op.children[0].get().estimated_cardinality);
1325  	if (build_to_probe_ratio > BUILD_TO_PROBE_RATIO_THRESHOLD) {
1326  		return false;
1327  	}
     // ... 1328 ...
1329  	// if we have a build side without a filter, only build the bloom filter if the ratio/size is small enough
1330  	static constexpr double NON_FILTERING_RATIO_THRESHOLD = 0.1;
1331  	static constexpr idx_t NON_FILTERING_BUILD_SIDE_THRESHOLD = 4194304;
1332  	if (!build_side_has_filter && build_to_probe_ratio > NON_FILTERING_RATIO_THRESHOLD &&
1333  	    build_count > NON_FILTERING_BUILD_SIDE_THRESHOLD) {
1334  		return false;
1335  	}
1336  
1337  	return true;
```

The line to focus on is 1332. Worked: a build of 5,000,000 rows against a
probe estimated at 20,000,000 has ratio 0.25. With a filtered build side, the
filter is built. With an *unfiltered* build side, 0.25 > 0.1 and 5,000,000 >
4,194,304, so it is not — the RPT+ §3.6 PK–FK argument as two constants.
Note also line 1324: the denominator is the probe side's **estimated**
cardinality, the number this whole topic distrusts. Earlier lines
(`:1298-1314`) also require exactly one equality condition.

### Step 5 — the Bloom filter: register-blocked, 12 bits per key minimum

> **In:** Step 4's decision to build one. **Out:** its layout, its size rule,
> and its measured accuracy against a classic filter.

```cpp
// src/planner/filter/table_filter_bloom_function.cpp:23–28, 62–79 — sizing and the bit mask
23  static constexpr idx_t MAX_NUM_SECTORS = (1ULL << 26);
24  static constexpr idx_t MIN_NUM_BITS_PER_KEY = 12;
25  static constexpr idx_t MIN_NUM_BITS = 512;
26  static constexpr idx_t LOG_SECTOR_SIZE = 6;             // a sector is 64 bits, log2(64) = 6
27  static constexpr idx_t SHIFT_MASK = 0x3F3F3F3F3F3F3F3F; // 6 bits for 64 positions
28  static constexpr idx_t N_BITS = 4;                      // the number of bits to set per hash
    // ... 29–61: Initialize, Merge, Reset ...
62  idx_t BloomFilter::GetNumberOfSectors(idx_t number_of_rows) {
63  	const idx_t min_bits = MaxValue(MIN_NUM_BITS, number_of_rows * MIN_NUM_BITS_PER_KEY);
64  	return MinValue(NextPowerOfTwo(min_bits) >> LOG_SECTOR_SIZE, MAX_NUM_SECTORS);
65  }
    // ... 66 ...
67  inline uint64_t GetMask(const hash_t hash) {
68  	const uint64_t shifts = hash & SHIFT_MASK;
69  	const auto shifts_8 = reinterpret_cast<const uint8_t *>(&shifts);
    // ... 70–72 ...
73  	for (idx_t bit_idx = 8 - N_BITS; bit_idx < 8; bit_idx++) {
74  		const uint8_t bit_pos = shifts_8[bit_idx];
75  		mask |= (1ULL << bit_pos);
76  	}
```

A **sector** is one 64-bit word. Insert and lookup (`:114-131`) pick the
sector with `hash & bitmask` — the low bits — and set or test four bits whose
positions are four 6-bit fields of the hash's high bytes (line 73). That is a
**register-blocked** Bloom filter: one word per probe, four bits per key,
whatever the size. Line 63–64 sizes it: at least 12 bits per row, rounded up
to a power of two. Worked: 1,000 build rows → 12,000 bits → 16,384 bits = 256
sectors, 16.38 bits per key; 10,000 rows → 131,072 bits, 13.11 per key. The
hashes are the hash table's own — `JoinHashTable::InsertHashes` feeds the
filter the same vector (`src/execution/join_hashtable.cpp:928-933`) — so the
filter costs no extra hashing.

Lane 1b builds this layout (with this crate's hash, not DuckDB's) and a
classic Bloom filter at the same bit budget:

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
> them, and the run-time rule that switches them off.

Filters are published when the hash table is finalized:
`PublishDeferredRuntimeFilters` (`physical_hash_join.cpp:1473-1496`) is called
from `FinishTasks` at `:1103-1105`, and each filter is wrapped as
**selectivity-optional** — a filter that monitors its own pass rate. The
thresholds:

```cpp
// src/planner/filter/table_filter_functions.cpp:36–62 — GetThresholdAndVectorsToCheck (excerpt)
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
37  	} else if (vectors_processed >= n_vectors_to_check) {
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
probing a Bloom filter that passes ≥ 50% of rows (RPT+ uses τ_stop = 0.9 on
the *ratio* between consecutive filters). The comment on line 41 says the
pause is meant to grow and then end at line 32. Tracing it says otherwise.
While paused, the Bloom select function calls `Update(0, 0)` once per vector
(`table_filter_bloom_function.cpp:205-207`); so does the wrapper
(`table_filter_selectivity_optional_function.cpp:143-145`). Each such call
increments `vectors_processed` by 1 (line 26) and, because the counters still
show the same high selectivity, increments `pause_multiplier` by 1 (line 41)
— so the target on line 32 moves up by 10 every time the counter moves up by
1:

```
vector  status before  vectors_processed  pause_multiplier  line-32 target
   6    active                6                  1               10
   7    paused                7                  2               20
   8    paused                8                  3               30
   n    paused                n                n − 5         10(n − 5)
```

For n ≥ 7 the target 10(n − 5) is always above n, so line 32 never fires and
the filter never resumes. Per the arithmetic as written, a paused filter is
off for the rest of that scan state — the same end result as RPT+'s "disable
further probing of that filter". This is a reading of the code (and a line-by-
line transcription run in Python while writing this chapter), not a DuckDB
measurement; whether a scan's states are re-created often enough to matter
was not traced.

## How to read the source (with the concepts in hand)

1. `src/optimizer/join_filter_pushdown_optimizer.cpp` top to bottom — 398
   lines; Steps 1–3.
2. `src/include/duckdb/execution/operator/join/join_filter_pushdown.hpp` —
   the data that crosses from plan time to run time.
3. `physical_hash_join.cpp:1245-1700` — Step 4's choices, then `:1103-1105`
   and `:1473-1496` for publication.
4. `src/planner/filter/table_filter_bloom_function.cpp` — Step 5;
   `table_filter_prefix_range_function.cpp:60-135` for the exact bitmap.
5. `table_filter_functions.cpp:36-62` and
   `table_filter_selectivity_optional_function.cpp:25-46` — Step 6.

## Questions (answer in notes.md)

1. Rewrite `GetPushdownFilterTargets` so a filter can enter a comparison
   join's *build* child when that child produces the column. Under what
   execution-order condition is the result correct? (Hint: SQL Server's
   pull order, and DuckDB's pipelines.)
2. The IN-list is pushed as an optional, zonemap-only filter. Why would an
   IN-list over 50 values be slower to evaluate per row than the
   register-blocked Bloom filter?
3. Step 4's exact bitmap triggers for spans under 2²⁶ bits (8 MiB). For lane
   1's keys (person ids 0–19,999, post ids 0–49,999), which filter would
   DuckDB choose for each join of the bad order?
4. If the permanent pause in Step 6 is real, what workload makes it a
   regression? (Hint: a sorted probe column whose first 6 vectors all match.)

## Done when

Answer each before unfolding it.

- [ ] You can say where DuckDB decides that a join gets a filter, and what it records for run time.

  <details><summary>Answer</summary>

  `JoinFilterPushdownOptimizer::GenerateJoinFilters`
  (`join_filter_pushdown_optimizer.cpp:294-388`): skip unsupported join types,
  make one pushdown column per comparison condition whose probe expression is a
  column, find target scans below the probe child (line 337), attach a dynamic
  filter set to each, add a min and a max aggregate per condition, and record
  whether the build side filters anything (lines 381–384).

  </details>

- [ ] You can explain why a filter cannot reach a build side below another join, citing lines.

  <details><summary>Answer</summary>

  `GetPushdownFilterTargets` handles a comparison join by recursing into
  `children[0]` only (lines 156, 160) — the probe child — and when it reaches a
  scan that does not produce the filtered column it returns (lines 217–219).
  A column produced by a lower join's build side is never reached. Lane 3's
  probe-spine-only model of this leaves lane 1's bad order at 3,020,210
  intermediates.

  </details>

- [ ] You can list the filter kinds in the order `FinalizeFilters` tries them and the condition for each.

  <details><summary>Answer</summary>

  min = max: a single comparison. Otherwise, for equality: an IN-list if the
  build has 2–50 rows (pushed as zonemap-only, skipped for NULLs or dense
  ranges); an exact range bitmap if the key span is under 2²⁶ bits or within
  the Bloom bit budget; else min-max plus, if `CanUseBloomFilter`, a deferred
  Bloom filter (`physical_hash_join.cpp:1636-1695`).

  </details>

- [ ] You can apply `CanUseBloomFilter`'s thresholds to a case.

  <details><summary>Answer</summary>

  Needs one equality condition and build/estimated-probe ≤ 1.0; an
  unfiltered build side is further refused if the ratio > 0.1 and the build
  > 4,194,304 rows (lines 1318–1335). 5 M build vs 20 M estimated probe:
  ratio 0.25 — built if the build side has a filter, refused if not.

  </details>

- [ ] You can describe the Bloom filter's layout and compare its accuracy with a classic filter.

  <details><summary>Answer</summary>

  Register-blocked: one 64-bit sector chosen by the hash's low bits, 4 bits
  set inside it from four 6-bit fields of the high bytes; ≥ 12 bits per key
  rounded to a power of two (lines 23–79). Lane 1b at equal bits: 1,000 keys
  (16.38 bits/key) 0.548% vs 0.045% classic; 10,000 keys (13.11) 0.901% vs
  0.181%.

  </details>

- [ ] You can state the pause rule and what the trace shows about resuming.

  <details><summary>Answer</summary>

  After 6 vectors, a Bloom (or range-bitmap) filter passing ≥ 50% of rows, or a
  min-max filter passing ≥ 90%, is paused (`table_filter_functions.cpp:38-44`,
  `table_filter_selectivity_optional_function.cpp:37-41`). It is meant to
  resume when `vectors_processed` reaches `pause_multiplier × 10` (line 32),
  but while paused each vector increments both, the multiplier by 1 and so
  the target by 10, so the counter never catches the target: as written, the
  pause is permanent for that state.

  </details>

## References

- `duckdb/duckdb` at `6c0c1a68`: `src/optimizer/join_filter_pushdown_optimizer.cpp`,
  `src/execution/operator/join/physical_hash_join.cpp`,
  `src/execution/join_hashtable.cpp`,
  `src/planner/filter/table_filter_bloom_function.cpp`,
  `src/planner/filter/table_filter_prefix_range_function.cpp`,
  `src/planner/filter/table_filter_functions.cpp`,
  `src/planner/filter/table_filter_selectivity_optional_function.cpp`.
- Qiao, Boncz, Zhang, RPT+ (VLDB'26) — §3.6, §4.2, §5.3, §6 (the v1.3.0
  baseline). [reading-rpt-plus.md](reading-rpt-plus.md).
- Zhao et al., SQL Server bitmaps (CIDR'26) — §3.1 exact bit-vectors.
  [reading-sqlserver-bitmaps.md](reading-sqlserver-bitmaps.md).
- Next: [reading-datafusion-dynamic-filters.md](reading-datafusion-dynamic-filters.md).
