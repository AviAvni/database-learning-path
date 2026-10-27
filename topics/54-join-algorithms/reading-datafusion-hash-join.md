# DataFusion's hash join: thread-level partitioning, one chained table per partition

DuckDB (the previous guide) is a C++ engine with one table per query that
spills when it must. DataFusion is a Rust engine in which a hash join is two
independent choices: *how many tables* (one shared one, or one per partition,
by `PartitionMode`) and *what the table is* (a hashbrown map plus a chain of
row indices, or a plain array when the keys are dense integers). Neither choice
is a cache decision. Reading the two together shows what "partitioned" means in
a query engine: a way to spread work over threads, with the cache effect — good
or bad — a side effect rather than the goal.

All anchors are `datafusion` at `350ca7650`, read with
`tools/pinned-source.py`; line numbers belong to that revision.

## The problem in one sentence

A join must be parallel across cores and its probe must not thrash the cache;
DataFusion solves the first with hash *repartitioning* (the plan's data
exchange, not a cache-sized radix pass) or by sharing one table, and leaves the
second to the table design.

## The concepts, step by step

### Step 1 — two modes: one shared table, or one table per partition

> **In:** the radix join's "co-partitioning" idea from the Balkesen guide.
> **Out:** what `CollectLeft` and `Partitioned` mean, and which of the papers'
> algorithms each is.

DataFusion executes a query plan as operators, each producing **partitions**:
independent streams of record batches, one per worker (typically one per core).
A hash join's `PartitionMode` says how the two inputs are partitioned:

```rust
// datafusion/physical-plan/src/joins/mod.rs:118–129 — PartitionMode
   118  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
   119  /// Hash join Partitioning mode
   120  pub enum PartitionMode {
   121      /// Left/right children are partitioned using the left and right keys
   122      Partitioned,
   123      /// Left side will collected into one partition
   124      CollectLeft,
   125      /// DataFusion optimizer decides which PartitionMode
   126      /// mode(Partitioned/CollectLeft) is optimal based on statistics. It will
   127      /// also consider swapping the left and right inputs for the Join
   128      Auto,
   129  }
```

The planner turns the mode into a *distribution requirement* on each input:

```rust
// datafusion/physical-plan/src/joins/hash_join/exec.rs:1529–1551 — HashJoinExec::input_distribution_requirements
  1529      fn input_distribution_requirements(&self) -> InputDistributionRequirements {
  1530          match self.mode {
  1531              PartitionMode::Partitioned => {
  1532                  let (left_expr, right_expr) = self
  1533                      .on
  1534                      .iter()
  1535                      .map(|(l, r)| (Arc::clone(l), Arc::clone(r)))
  1536                      .unzip();
  1537                  InputDistributionRequirements::co_partitioned(vec![
  1538                      Distribution::KeyPartitioned(left_expr),
  1539                      Distribution::KeyPartitioned(right_expr),
  1540                  ])
  1541              }
  1542              PartitionMode::CollectLeft => InputDistributionRequirements::new(vec![
  1543                  Distribution::SinglePartition,
  1544                  Distribution::UnspecifiedDistribution,
  1545              ]),
  1546              PartitionMode::Auto => InputDistributionRequirements::new(vec![
  1547                  Distribution::UnspecifiedDistribution,
  1548                  Distribution::UnspecifiedDistribution,
  1549              ]),
  1550          }
  1551      }
```

- **`CollectLeft`**: the left (build) input is required to be a **single
  partition** (`Distribution::SinglePartition`, line 1543), the probe input is
  left as it is. One table is built once and shared: `left_fut` is an
  `Arc<OnceAsync<JoinLeftData>>` that "for CollectLeft partition mode … is
  *shared* across all output streams" (`exec.rs:820–826`), so every probe
  partition (thread) waits for the one build and then probes the same read-only
  table. This is lane 1's join with the build side collected into one table
  (the build is a single stream), and the probe parallelised the way lane 1c
  does.
- **`Partitioned`**: both inputs must be **hash-partitioned on the join keys**
  (`KeyPartitioned`, lines 1538–1539; `co_partitioned` says the partition `p` of
  one side pairs with partition `p` of the other). The physical optimizer
  satisfies the requirement by repartitioning each input — a `RepartitionExec`,
  whose hash function is quoted below (the rule that inserts it is not read
  here). Every output partition
  builds its *own* table from its left partition and probes it with its right
  partition. This is the structure of a radix join — partition both sides by
  hash, join pairs — with a fan-out equal to the number of partitions, which is
  the number of cores, not the number of cache-sized pieces.

The repartitioning function is a modulo, not a bit-slice: the hash partitioner
"precomputes a strength-reduced reducer for `hash % num_partitions`"
(`repartition/mod.rs:1020`). That makes the partition count free to be any
integer (24 partitions is the example the source uses, `exec.rs:655`).

`Auto` lets the optimiser choose between the two from statistics, and may swap
the inputs so the smaller side builds. The docs say why (`exec.rs:665–667`): "it
is VERY important that the *smaller* input is placed on the build side".

### Step 2 — the table: hashbrown, plus a chain of row indices

> **In:** the table of lane 1 (8-byte tuples in open-addressed slots) and
> DuckDB's pointer table. **Out:** the third design, worked on the source's own
> example.

The build side is not stored in the table. The build batches are concatenated
into one big batch; the table holds, for each distinct *hash value*, the index
of the last build row with that hash, and a second array `next` chains the
earlier rows with the same hash:

```rust
// datafusion/physical-plan/src/joins/join_hash_map.rs:170–177 — JoinHashMapU32
   170  pub struct JoinHashMapU32 {
   171      // Stores hash value to last row index
   172      map: HashTable<(u64, u32)>,
   173      // Stores indices in chained list data structure
   174      next: Vec<u32>,
   175      // Whether any hash value maps to more than one row
   176      chained: bool,
   177  }
```

`map` is a `HashTable<(u64, u32)>` from the `hashbrown` crate: each entry is a
64-bit hash and a 32-bit "row index + 1" — 16 bytes after alignment, against 8
bytes in lane 1 and DuckDB. The `u32` flavour is chosen when the build has at
most `u32::MAX` rows (`new_join_hashmap`, `exec.rs:2926–2960`; the `u64` flavour
otherwise). The insert shows the chaining:

```rust
// datafusion/physical-plan/src/joins/join_hash_map.rs:358–380 — update_from_iter
   358      for (row, &hash_value) in iter {
   359          let entry = map.entry(
   360              hash_value,
   361              |&(hash, _)| hash_value == hash,
   362              |&(hash, _)| hash,
   363          );
   364  
   365          match entry {
   366              Occupied(mut occupied_entry) => {
   367                  // Already exists: add index to next array
   368                  let (_, index) = occupied_entry.get_mut();
   369                  let prev_index = *index;
   370                  // Store new value inside hashmap
   371                  *index = T::try_from(row + 1).unwrap();
   372                  // Update chained Vec at `row` with previous value
   373                  next[row - deleted_offset] = prev_index;
   374                  chained = true;
   375              }
   376              Vacant(vacant_entry) => {
   377                  vacant_entry.insert((hash_value, T::try_from(row + 1).unwrap()));
   378              }
   379          }
   380      }
```

On a hash that is already present (`Occupied`, line 366) it stores the *new* row
in the map and the *previous* index in `next[row]`: a linked list threaded
through an array, newest first. The source's comment works it on rows with
hashes 10, 20, 10, 10:

```
Insert (10,1):  map {10 -> 2}                    next [0 0 0 0 0]
Insert (20,2):  map {10 -> 2, 20 -> 3}           next [0 0 0 0 0]
Insert (10,3):  map {10 -> 4, 20 -> 3}           next [0 0 0 2 0]
Insert (10,4):  map {10 -> 5, 20 -> 3}           next [0 0 0 2 4]
```

Probe hash 10 (the loop at `join_hash_map.rs:399–425`): `map[10] = 5`, so
`i = 5 − 1 = 4`, emit row 4; `next[4] = 4`, so `i = 4 − 1 = 3`, emit row 3;
`next[3] = 2`, so `i = 1`, emit row 1; `next[1] = 0`, stop. Matches 4, 3, 1: the
rows in reverse insertion order, which is why the build inserts the batches in
reverse (`exec.rs:692–696`, so the output keeps the build order).

Two consequences. First, **the table is keyed by hash, not by key**: two
different keys with the same 64-bit hash share a chain, so every candidate row
must be compared on the real key afterwards. That is the `equal_rows_arr…` call
that follows the lookup (`stream.rs:503–512`). Second, a lookup is two or three
dependent memory accesses on a hit: the hashbrown entry, then `next[...]`, then,
when the output is built, the build rows themselves. Lane 1's tuple-in-slot
table needs one access for a unique key.

### Step 3 — the probe, a batch at a time

> **In:** Step 2's table and a batch of probe-side record batches. **Out:** the
> four steps of the probe loop.

The probe stream processes one probe record batch per iteration.
First all keys' hashes are computed in one vectorised pass:

```rust
// datafusion/physical-plan/src/joins/hash_join/stream.rs:790–806 — HashJoinStream — hashing a probe batch
   790                  // Precalculate hash values for fetched batch
   791                  let keys_values = evaluate_expressions_to_arrays(&self.on_right, &batch)?;
   792  
   793                  let valid_keys = if let Map::HashMap(_) =
   794                      self.build_side.try_as_ready()?.left_data.map()
   795                  {
   796                      self.hashes_buffer.clear();
   797                      self.hashes_buffer.resize(batch.num_rows(), 0);
   798                      create_hashes(
   799                          &keys_values,
   800                          &self.random_state,
   801                          &mut self.hashes_buffer,
   802                      )?;
   803                      matchable_join_keys(&keys_values, self.null_equality)
   804                  } else {
   805                      None
   806                  };
```

then `lookup_join_hashmap` (`stream.rs:481–512`) calls
`get_matched_indices_with_limit_offset`, which walks the chains for every
probe hash and fills two **index buffers** — `probe_indices` (which probe row)
and `build_indices` (which build row) — then verifies the keys
(`equal_rows_arr_with_normalized_left`), and finally the output batch is assembled from the two index arrays by
`build_batch_from_indices` (`stream.rs:1012`), which **gathers** each column with
Arrow's `take` (`joins/utils.rs:1537, :1545`). Four steps: hash, look up,
verify, gather. Compare lane 1's probe, which does the first three in one
register-resident loop and then writes nothing at all: lane 1 produces a checksum
where DataFusion produces batches, and the gather is where a wide payload pays,
the cost Bandle et al. count against the radix join in §5.4.2.

### Step 4 — the array table, and what partitioning does to it

> **In:** Step 2's general table and Schuh et al.'s lesson 7 (dense keys →
> array). **Out:** the condition under which the table is an array, and the
> source's own warning that `Partitioned` mode can defeat it.

For a single integer key, `HashJoinExec` may use an `ArrayMap`, which the
source calls a **perfect hash join**: build keys `min..=max` index a plain array.
`array_map_key_range` decides (`exec.rs:126–200`); the doc comment lists the
conditions (`:629–642`) — one key, an integer of at most 64 bits, a key range
that is small *or* dense, fewer than `u32::MAX` rows, no NULLs when NULLs equal
each other. The density rule, as code:

```rust
// datafusion/physical-plan/src/joins/hash_join/exec.rs:191–197 — array_map_key_range — the density test
   191      let dense_ratio = (num_row as f64) / ((range + 1) as f64);
   192  
   193      if range >= perfect_hash_join_small_build_threshold as u64
   194          && dense_ratio <= perfect_hash_join_min_key_density
   195      {
   196          return Ok(None);
   197      }
```

`perfect_hash_join_small_build_threshold` defaults to 1024 and
`perfect_hash_join_min_key_density` to 0.15 (`datafusion/common/src/config.rs:922,
:932`). Worked: 1,000,000 keys spread over a range of 5,000,000 have
density 0.2 > 0.15, so the array is used (5,000,000 slots); over 10,000,000 the
density is 0.1 ≤ 0.15 and the range is ≥ 1024, so it is not.

The doc comment then states the interaction this topic cares about
(`exec.rs:644–657`): "when using `PartitionMode::Partitioned`, the build side is
split into multiple partitions. This can cause a dense build side to become
sparse within each partition, potentially disabling this optimization" — its
example, `range(10000)` joined with itself on 24 partitions, gives a first
partition "like `3, 10, 18, 39, 43`". Here partitioning has a measurable
*penalty* unrelated to cache: it destroys the key structure an array join
relies on. (Arithmetic: 10,000 keys over 24 partitions leaves ≈ 417 per
partition, each spread over the whole 0–9,999 range: density ≈ 0.04, below
0.15.)

### Step 5 — three things that are not here

> **In:** Steps 1–4. **Out:** what a reader should not expect to find.

- **No cache-sized radix pass.** `Partitioned` fan-out equals the core count;
  there is no histogram-and-scatter into hundreds of partitions to fit L2, and
  no write-combine buffers, in the hash-join files.
- **A different hash seed from the exchange.** The join hashes with the fixed
  `HASH_JOIN_SEED` "to ensure hash values from the hash join differ from
  `RepartitionExec`, avoiding collisions" (`exec.rs:113–115`), while the
  repartitioner uses seed 0 (`repartition/mod.rs:677`). If both used the same
  hash, every row in partition *p* would have `hash % N = p`, and a table that
  uses low hash bits would see only 1/N of them. It is the same constraint
  that made `hash.rs` split the hash bits between partition and slot, and
  that DuckDB solved by giving the radix partition, the salt and the slot
  disjoint bit ranges.
- **No spilling read here.** The build reserves memory with
  `reservation.try_grow(...)?` (`exec.rs:2944, 2957`), which fails the query when
  the pool is exhausted; `stream.rs` has no spill code, and I did not look for
  one outside the hash-join files. Contrast DuckDB's external join (Step 5 of
  the previous guide).

## How to read the source (with the concepts in hand)

1. **`joins/mod.rs:118–129`** and **`hash_join/exec.rs:1529–1551`** — Step 1.
2. **`join_hash_map.rs:60–118`** (the doc), **`:170–177`**, **`:347–430`** —
   Step 2.
3. **`hash_join/stream.rs:481–512`, `:790–830`** — Step 3.
4. **`hash_join/exec.rs:119–200`, `:625–660`** — Step 4.
5. Skip on a first pass: dynamic filters and bounds (`shared_bounds.rs`,
   topic 47's reading guide), null-aware joins, and the outer-join bitmaps.

## Where each step lives in this topic

| step | where | what to look at |
|---|---|---|
| 1 | `experiments/src/npj.rs`, lane 1c | `CollectLeft` is a shared table and parallel probe: `probe_parallel` |
| 1 | `experiments/src/radix.rs` | `Partitioned` at fan-out = threads is lane 2 with `bits = log2(threads)` and one table per partition |
| 2 | `experiments/src/npj.rs` | tuple-in-slot (lane 1) vs hash-plus-index chain (DataFusion): one dependent access vs two or three |
| 4 | `gen.rs` (`key_of`) | lane 1's keys are sparse; exercise 6 makes them dense and asks when an array wins |
| 5 | `hash.rs` | the low/high bit split that the seed difference replaces |

## Questions (answer in notes.md)

1. DataFusion's entry is `(u64 hash, u32 row+1)`: 16 bytes. Lane 1's is 8.
   At |R| = 2²², how large is each table (this guide does not give
   hashbrown's load factor: state the assumption you make), and which fits the
   M3's 16 MiB cluster L2?
2. In `Partitioned` mode with 11 partitions on an 11-core machine, what is the
   per-partition table size for |R| = 2²⁴, and how does it compare with lane 2's
   best configuration from notes.md?
3. A join on a dense `i32` key with 2²⁰ build rows and probe side of 2²⁵: in
   `CollectLeft` mode does `array_map_key_range` choose the array map? In
   `Partitioned` mode with 11 partitions?
4. The probe compares hashes first and keys second. For a workload of 64-bit
   keys with a good hash, how often does the second comparison reject a row, and
   what does that say about whether the table should store the key?

## Done when

Answer each before unfolding it.

- [ ] You can say what `Partitioned` means and what it does not.

  <details><summary>Answer</summary>

  Both inputs are hash-repartitioned on the join keys into N partitions
  (N = the degree of parallelism), and each partition pair is joined with
  its own table (`exec.rs:1531–1541`). It is the structure of a radix join with
  a thread-count fan-out; it does not choose the fan-out for a cache size, does
  no histogram-and-scatter into cache-sized pieces, and uses `hash % N`
  (`repartition/mod.rs:1020`), not a bit-slice.

  </details>

- [ ] You can run the chain example by hand.

  <details><summary>Answer</summary>

  After the inserts (10,1), (20,2), (10,3), (10,4): `map = {10→5, 20→3}`,
  `next = [0,0,0,2,4]`. Lookup of 10: `i = 5−1 = 4` → emit row 4; `next[4] = 4` →
  `i = 3` → emit row 3; `next[3] = 2` → `i = 1` → emit row 1; `next[1] = 0` →
  stop. Rows 4, 3, 1: reverse insertion order (`join_hash_map.rs:69–113,
  399–425`).

  </details>

- [ ] You can state why the join's hash seed differs from the exchange's.

  <details><summary>Answer</summary>

  Rows in repartition partition *p* all satisfy `hash % N = p` for the
  exchange's hash. If the join used the same hash, its table would see only
  the hashes in that residue class, which clusters entries and makes slots
  collide. The join's fixed seed gives it an independent hash
  (`exec.rs:113–115`). The general rule is that one stage's partitioning bits
  must not be the next stage's table bits.

  </details>

- [ ] You can apply the array-map density test.

  <details><summary>Answer</summary>

  Use the array when range ≥ 1024 and density = rows / (range + 1) > 0.15, or
  when the range is below 1024 (`exec.rs:191–197`, defaults at
  `config.rs:922, :932`). 1,000,000 rows over a 5,000,000 range: density 0.2,
  array. Over 10,000,000: density 0.1, hash map.

  </details>

- [ ] You can state the cost of `Partitioned` that is not about cache.

  <details><summary>Answer</summary>

  It can turn a dense build side sparse inside each partition, disabling the
  array map (`exec.rs:644–657`): 10,000 consecutive keys over 24 partitions
  leave about 417 keys in each, spread across the whole range, density about
  0.04 against the 0.15 threshold. This is Schuh et al.'s lesson 7 (array
  joins win on dense keys) colliding with partitioning.

  </details>

## References

- DataFusion `350ca7650`: `datafusion/physical-plan/src/joins/mod.rs`,
  `.../joins/hash_join/exec.rs`, `.../joins/hash_join/stream.rs`,
  `.../joins/join_hash_map.rs`, `.../repartition/mod.rs`,
  `datafusion/common/src/config.rs`.
- The chained-index design cites a thesis chapter in the source
  (`join_hash_map.rs:67`): "Balancing vectorized query execution with
  bandwidth-optimized storage", chapter 5.3; not read for this guide.
- Previous: [reading-duckdb-join-hashtable.md](reading-duckdb-join-hashtable.md).
  Back to the [README](README.md).
