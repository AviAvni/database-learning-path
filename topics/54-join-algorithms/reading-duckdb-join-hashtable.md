# DuckDB's join hash table: one shared table, and where the radix bits actually go

The three papers argue about whether to partition. This guide reads a
production engine that answers it, and the answer is a split decision: for a
join that fits in memory, DuckDB builds **one shared table** and probes it
directly, exactly the "no-partitioning" join of lane 1 (with a salted-pointer
table instead of tuples in slots); for a join that does *not* fit, it
radix-partitions both sides and runs partition pairs one memory-load at a
time. The radix bits are in the code either way, which makes the code a good
place to see what they are used for.

All anchors are `duckdb` at v2.0.0 (`d54cbbe3df`), read with
`tools/pinned-source.py`. Line numbers belong to that revision.

## The problem in one sentence

A hash join has to choose, per query, among a shared table (cheap while it
fits in cache), cache-sized partitions (cheap when it does not, but a second
pass over the data) and disk-sized partitions (the only option when memory
runs out) — DuckDB uses the same radix partitioning machinery for the third
and for parallelising the first, and not for the second.

## The concepts, step by step

### Step 1 — the table: 8-byte entries pointing at rows

> **In:** lane 1's table, in which each slot holds the whole 8-byte tuple.
> **Out:** DuckDB's entry layout, its sizing rule, and a worked table size.

DuckDB's join does not keep tuples in the table. The build side is first
appended to row-format storage (a **row** here is the build tuple's key and
payload columns laid out contiguously, plus a trailing 8-byte field that holds
the key's 64-bit hash during the build and, once the row is inserted, the
pointer to the next row with an equal key — `StorePointer(…, row + pointer_offset)`
at `join_hashtable.cpp:778` and `:793`). The hash table proper is an array of 8-byte **entries**,
each a pointer to a row with 16 bits of the hash — a **salt** — packed into the
pointer's unused upper bits:

```cpp
// src/include/duckdb/execution/ht_entry.hpp:34–76 — ht_entry_t — salt and pointer share 64 bits
    34  	//! Upper 16 bits are salt, lower 48 bits are the pointer
    35  	static constexpr const hash_t SALT_MASK = 0xFFFF000000000000;
    36  	static constexpr const hash_t POINTER_MASK = 0x0000FFFFFFFFFFFF;
    37  #endif
     // ... 38-73: constructors, IsOccupied, GetPointer, SetPointer ...
    74  	static inline hash_t ExtractSalt(const hash_t &hash) {
    75  		return hash | POINTER_MASK;
    76  	}
```

The pointer is 48 bits because user-space addresses are (line 36); the salt is
the hash's own top 16 bits (`ExtractSalt` keeps the hash's upper bits and sets
the lower 48 to ones, line 75). An entry of 0 means "empty" (`IsOccupied`,
`value != 0`). The table is sized from the row count and a **load factor**
(fraction of entries filled):

```cpp
// src/include/duckdb/execution/join_hashtable.hpp:625–642 — load factors and PointerTableCapacity
   625  	//! For a LOAD_FACTOR of 2.0, the HT is between 25% and 50% full
   626  	static constexpr double DEFAULT_LOAD_FACTOR = 2.0;
   627  	//! For a LOAD_FACTOR of 1.5, the HT is between 33% and 67% full
   628  	static constexpr double EXTERNAL_LOAD_FACTOR = 1.5;
     // ... 629-631: the capacity comment ...
   632  	double load_factor = DEFAULT_LOAD_FACTOR;
   633  
   634  	//! Capacity of the pointer table given the ht count
   635  	idx_t PointerTableCapacity(idx_t count) const {
   636  		const auto capacity = NextPowerOfTwo(LossyNumericCast<idx_t>(static_cast<double>(count) * load_factor));
   637  		return MaxValue<idx_t>(capacity, MINIMUM_CAPACITY);
   638  	}
   639  	//! Size of the pointer table (in bytes)
   640  	idx_t PointerTableSize(idx_t count) const {
   641  		return PointerTableCapacity(count) * sizeof(data_ptr_t);
   642  	}
```

The default factor 2.0 means capacity is `count × 2.0` rounded up to a power of
two, so the table is "between 25% and 50% full" (the comment on line 625). Work
it for a build side of 1,000,000 rows: 1,000,000 × 2.0 = 2,000,000, the next
power of two is 2,097,152 = 2²¹, times 8 bytes = 16,777,216 bytes = 16 MiB
of pointers (line 641), *plus* the rows themselves. Lane 1 sizes its table by the
same rule — `Table::capacity_for` is `next_power_of_two(2n)` — and its slots
hold the tuple, so for |R| = 2²⁰ both pointer arrays are 16 MiB, and DuckDB's
join has a further dependent load into row storage on every *match*.

### Step 2 — three fields of one hash

> **In:** Step 1's entry. **Out:** which bits of the 64-bit hash pick the slot,
> the salt and the radix partition, worked on one hash value.

DuckDB cuts one 64-bit hash into three disjoint fields, so that none of the
uses interferes with another:

- **slot**: the *low* bits, `hash & bitmask`, where `bitmask = capacity − 1`
  (`ApplyBitmaskAndGetSaltBuild`, `join_hashtable.cpp:186–206`, with the mask set at
  `:1097`);
- **salt**: the *top 16* bits (`ht_entry.hpp:35`);
- **radix partition**: the `radix_bits` bits just *below* the salt.

```cpp
// src/include/duckdb/common/radix_partitioning.hpp:26–58 — the partition field
    26  	static constexpr const idx_t MAX_RADIX_BITS = 12;
     // ... 27-44: NumberOfPartitions and helpers ...
    45  	static inline constexpr idx_t Shift(idx_t radix_bits) {
    46  		return (sizeof(hash_t) - sizeof(uint16_t)) * 8 - radix_bits;
    47  	}
    48  
    49  	//! Mask of the radix bits of the hash
    50  	static inline constexpr hash_t Mask(idx_t radix_bits) {
    51  		return (hash_t(1 << radix_bits) - 1) << Shift(radix_bits);
    52  	}
    53  
    54  	//! Apply the radix mask to a hash and return the partition index
    55  	static inline idx_t ApplyMask(const hash_t hash, idx_t radix_bits) {
    56  		D_ASSERT(radix_bits <= MAX_RADIX_BITS);
    57  		const auto result = (hash & Mask(radix_bits)) >> Shift(radix_bits);
    58  		D_ASSERT(result < NumberOfPartitions(radix_bits));
```

`Shift(radix_bits)` is `(8 − 2) × 8 − radix_bits = 48 − radix_bits`, so the
partition is `hash` bits `[48 − radix_bits, 48)`. Worked on the hash
`0xABCD_1234_5678_9ABC` with `radix_bits = 4` and a pointer table of 2²¹ entries:

```
salt       top 16 bits                  = 0xABCD
partition  bits 44..47 (Shift = 44)     = hex digit '1'  = partition 1
slot       low 21 bits (mask 0x1FFFFF)  = 0x789ABC & 0x1FFFFF = 0x189ABC
```

Compare lane 1, which splits the same way for the same reason:
`hash.rs` takes radix bits from the *low* end and the table slot from the
*top*, so that inside one partition (where the low bits are all equal) the
table still spreads. DuckDB instead puts the partition in the middle and the
slot at the bottom; both designs work as long as the fields do not overlap
for the sizes in use.

### Step 3 — the build: partition on the way in, one global table at the end

> **In:** Steps 1–2. **Out:** the order of events during a build and what the
> partitioning is used for.

**Sink** (the build phase, one call per incoming chunk of rows): hash the keys,
then append the rows to a *radix-partitioned* collection:

```cpp
// src/execution/join_hashtable.cpp:715–727 — JoinHashTable::Build (tail)
   715  	// hash the keys and obtain an entry in the list
   716  	// note that we only hash the keys used in the equality comparison
   717  	Hash(keys, *current_sel, added_count, hash_values);
   718  	if (bloom_filter) {
   719  		bloom_filter->InsertHashes(hash_values, *current_sel, added_count);
   720  	}
   721  
   722  	// Re-reference and ToUnifiedFormat the hash column after computing it
   723  	source_chunk.data[col_offset].Reference(hash_values);
   724  	hash_values.ToUnifiedFormat(append_state.chunk_state.vector_data.back().unified);
   725  
   726  	// We already called TupleDataCollection::ToUnifiedFormat, so we can AppendUnified here
   727  	sink_collection->AppendUnified(append_state, source_chunk, *current_sel, added_count);
```

`sink_collection` is a `RadixPartitionedTupleData` with `radix_bits` bits —
initially 4, so 16 partitions, or 5 when the engine has 100 or more threads
(`physical_hash_join.cpp:412`). The partitioning is *row* partitioning: the
hash column was appended to the row, and `ComputePartitionIndices`
(`radix_partitioning.cpp:141–146`) reads it to pick the partition.

**Finalize**: allocate the pointer table (`AllocatePointerTable`,
`join_hashtable.cpp:1066–1097`: `capacity = PointerTableCapacity(Count())`),
zero it, then insert every row's pointer, in parallel, one task per range of
rows inside each partition:

```cpp
// src/execution/operator/join/physical_hash_join.cpp:1164–1179 — HashJoinFinalizeEvent::GetTasks (excerpt)
  1164  		const bool parallel = !FinalizeSingleThreaded(sink, false) && ht.GetRadixBits() != 0;
  1165  		vector<vector<pair<idx_t, idx_t>>> task_ranges;
  1166  		if (!parallel) {
  1167  			task_ranges.push_back({{0, data_collection.ChunkCount()}});
  1168  		} else {
  1169  			const auto num_partitions = RadixPartitioning::NumberOfPartitions(ht.GetRadixBits());
  1170  			const auto &current_partitions = ht.GetCurrentPartitions();
  1171  			const auto max_tasks_per_partition = MaxValue<idx_t>(sink.num_threads / num_partitions, 1);
  1172  			for (idx_t partition_idx = 0; partition_idx < num_partitions; partition_idx++) {
  1173  				if (sink.external && !current_partitions.RowIsValidUnsafe(partition_idx)) {
  1174  					continue;
  1175  				}
  1176  				SplitPartitionRanges(data_collection.GetChunkRangesForPartition(partition_idx), max_tasks_per_partition,
  1177  				                     task_ranges);
  1178  			}
  1179  		}
```

Each task reads the stored hashes back out of its rows (`Finalize`,
`join_hashtable.cpp:1110–1131`: `Load<hash_t>(row_locations[i] + pointer_offset)`)
and inserts with a compare-and-swap so that several threads can fill the one
table (`InsertRowToEntry`, `:771–798`; the `PARALLEL` branch at line 775). The
line to focus on is 1171: `max_tasks_per_partition = max(num_threads /
num_partitions, 1)` — the partitions exist to hand out work in units, not to
make each unit's table small. **Every insert still lands at a random address
of the one global table.** In the vocabulary of the papers this is a
no-partitioning join whose build input happens to be stored grouped by hash.

### Step 4 — the probe: one table, salt first

> **In:** Step 3's finished table. **Out:** the probe loop's decision order and
> the size at which the salt is switched on.

The probe is a loop of linear probing over the pointer table. The salt is
compared *before* the pointer is dereferenced, so most non-matching probes
never touch the row:

```cpp
// src/execution/join_hashtable.cpp:237–264 — ProbeForPointersInternal (excerpt)
   237  	for (idx_t i = 0; i < count; i++) {
   238  		auto row_hash = hashes_dense[i]; // hashes have been flattened before -> always access dense
   239  		auto row_ht_offset = row_hash & bitmask;
   240  
   241  		if (USE_SALTS) {
   242  			// increment the ht_offset of the entry as long as the next entry is occupied and salt does not match
   243  			while (true) {
   244  				const ht_entry_t entry = entries.get()[row_ht_offset];
   245  				const bool occupied = entry.IsOccupied();
   246  
   247  				// the entry is empty -> no match possible
   248  				if (!occupied) {
   249  					break;
   250  				}
   251  
   252  				const hash_t row_salt = ht_entry_t::ExtractSalt(row_hash);
   253  				const bool salt_match = entry.GetSalt() == row_salt;
   254  				if (salt_match) {
   255  					// we know that the entry is occupied and the salt matches -> compare the keys
   256  					auto row_index = GetOptionalIndex<HAS_SEL>(row_sel, i);
   257  					AddPointerToCompare(state, entry, pointers_result_v, row_ht_offset, keys_to_compare_count,
   258  					                    row_index);
   259  					break;
   260  				}
   261  
   262  				// full and salt do not match -> continue probing
   263  				IncrementAndWrap(row_ht_offset, bitmask);
   264  			}
```

Read it as: take the slot (`:239`); if the entry is empty the key is absent
(`:248`); if the salt equals the probe's salt, queue the row for a full key
comparison (`:254–259`); otherwise step to the next entry (`:263`). A wrong
slot is rejected after one 8-byte compare, with a false-match probability of
about 2⁻¹⁶ per occupied entry (16 salt bits). The salt is not always used:

```cpp
// src/include/duckdb/execution/join_hashtable.hpp:140–142 — USE_SALT_THRESHOLD
   140  	//! only compare salts with the ht entries if the capacity is larger than 8192 so
   141  	//! that it does not fit into the CPU cache
   142  	static constexpr const idx_t USE_SALT_THRESHOLD = 8192;
```

`8192` entries × 8 bytes = 64 KiB. The comment says it directly — "only compare
salts … if the capacity is larger than 8192 so that it does not fit into the
CPU cache" — which is the engine encoding the first half of Bandle et al.'s
finding in one constant: for a table that fits the cache, the extra compare
costs more than the cache miss it would avoid.

### Step 5 — when DuckDB does partition: the external join

> **In:** Steps 1–4 and the paper's "larger than main-memory working sets"
> concession. **Out:** the trigger, the repartitioning rule, and the spilled
> probe side.

At the end of the build the engine compares the *reserved memory* with the
build's total size; if it does not fit, it lowers the load factor to
`EXTERNAL_LOAD_FACTOR` 1.5 (to try to avoid going external at all) and, if it
still does not fit, repartitions:

```cpp
// src/execution/operator/join/physical_hash_join.cpp:1985–2024 — HashJoinFinalizeEvent / PhysicalHashJoin::Finalize (excerpt)
  1985  	sink.external = sink.temporary_memory_state->GetReservation() < sink.total_size;
  1986  	if (sink.external) {
  1987  		// For external join we reduce the load factor, this may even prevent the external join altogether
  1988  		ht.load_factor = JoinHashTable::EXTERNAL_LOAD_FACTOR;
  1989  
     // ... 1990-1994: the ht size is recomputed at the lower load factor ...
  1995  		if (temp_total_size < sink.temporary_memory_state->GetReservation()) {
  1996  			// We prevented the external join by reducing the load factor. Update the state accordingly
  1997  			sink.temporary_memory_state->SetMinimumReservation(temp_total_size);
  1998  			sink.temporary_memory_state->SetRemainingSizeAndUpdateReservation(context, temp_total_size);
  1999  
  2000  			sink.total_size = temp_total_size;
  2001  			sink.max_partition_size = temp_max_partition_size;
  2002  			sink.max_partition_count = temp_max_partition_count;
  2003  
  2004  			sink.external = false;
  2005  		}
  2006  	}
  2007  	DUCKDB_LOG(context, PhysicalOperatorLogType, *this, "PhysicalHashJoin", "Finalize",
  2008  	           {{"external", to_string(sink.external)}});
  2009  	if (sink.external) {
  2010  		// External Hash Join
  2011  		// Recursive preserved-build reuse only applies to the in-memory finalized HT. External hash join
  2012  		// runs through repeated build/probe rounds with partition/probe-spill state, so it cannot be
  2013  		// treated as a stable materialized build that later recursive iterations may skip entirely.
  2014  		sink.preserve_build_for_reuse = false;
  2015  		sink.perfect_join_executor.reset();
  2016  
  2017  		const auto max_partition_ht_size = sink.max_partition_size + ht.PointerTableSize(sink.max_partition_count);
  2018  		const auto very_very_skewed = // No point in repartitioning if it's this skewed
  2019  		    static_cast<double>(max_partition_ht_size) >= 0.8 * static_cast<double>(sink.total_size);
  2020  		if (!very_very_skewed &&
  2021  		    (max_partition_ht_size + sink.probe_side_requirement) > sink.temporary_memory_state->GetReservation()) {
  2022  			// We have to repartition
  2023  			const auto radix_bits_before = ht.GetRadixBits();
  2024  			ht.SetRepartitionRadixBits(sink.GetMaxExternalHTSize(), sink.max_partition_size, sink.max_partition_count);
```

`SetRepartitionRadixBits` chooses how many more bits to add — up to
`MAX_RADIX_BITS = 12`, i.e. 4,096 partitions (`radix_partitioning.hpp:26`):

```cpp
// src/execution/join_hashtable.cpp:2462–2481 — SetRepartitionRadixBits
  2462  void JoinHashTable::SetRepartitionRadixBits(const idx_t max_ht_size, const idx_t max_partition_size,
  2463                                              const idx_t max_partition_count) {
  2464  	D_ASSERT(max_partition_size + PointerTableSize(max_partition_count) > max_ht_size);
  2465  
  2466  	const auto max_added_bits = RadixPartitioning::MAX_RADIX_BITS - radix_bits;
  2467  	idx_t added_bits = 1;
  2468  	for (; added_bits < max_added_bits; added_bits++) {
  2469  		double partition_multiplier = static_cast<double>(RadixPartitioning::NumberOfPartitions(added_bits));
  2470  
  2471  		auto new_estimated_size = static_cast<double>(max_partition_size) / partition_multiplier;
  2472  		auto new_estimated_count = static_cast<double>(max_partition_count) / partition_multiplier;
  2473  		auto new_estimated_ht_size =
  2474  		    new_estimated_size + static_cast<double>(PointerTableSize(LossyNumericCast<idx_t>(new_estimated_count)));
  2475  
  2476  		if (new_estimated_ht_size <= static_cast<double>(max_ht_size) / 4) {
  2477  			// Aim for an estimated partition size of max_ht_size / 4
  2478  			break;
  2479  		}
  2480  	}
  2481  	radix_bits += added_bits;
```

The loop at 2468–2480 stops at the first `added_bits` for which a partition
is estimated to fit `max_ht_size / 4`. Worked, ignoring the pointer-table
term: if the largest partition is 1 GiB and the memory the table may use is
256 MiB, the target is 64 MiB, and 1 GiB / 2^added_bits ≤ 64 MiB first holds at
added_bits = 4, so 16× more partitions. The engine then processes partitions
in rounds: build the table for the partitions that fit, probe, and spill the
probe rows whose partition is not resident — the radix field of the hash decides:

```cpp
// src/execution/join_hashtable.cpp:2663–2681 — ProbeAndSpill (excerpt)
  2663  	// find out which keys we can match with the current pinned partitions
  2664  	SelectionVector true_sel(STANDARD_VECTOR_SIZE);
  2665  	SelectionVector false_sel(STANDARD_VECTOR_SIZE);
  2666  	const auto true_count =
  2667  	    RadixPartitioning::Select(hashes, FlatVector::IncrementalSelectionVector(), probe_keys.size(), radix_bits,
  2668  	                              current_partitions, &true_sel, &false_sel);
  2669  	const auto false_count = probe_keys.size() - true_count;
  2670  
  2671  	// can't probe these values right now, append to spill
  2672  	spill_chunk.Reset();
  2673  	spill_chunk.Reference(probe_chunk);
  2674  	spill_chunk.data.back().Reference(hashes);
  2675  	spill_chunk.Slice(false_sel, false_count);
  2676  	probe_spill.Append(spill_chunk, spill_state);
  2677  
  2678  	// slice the stuff we CAN probe right now
  2679  	hashes.Slice(true_sel, true_count);
  2680  	probe_keys.Slice(true_sel, true_count);
  2681  	probe_chunk.Slice(true_sel, true_count);
```

This is the textbook partitioned (Grace-style) hash join, there for the case
the papers set aside: more data than memory. It is the "other reason" in
Bandle et al.'s conclusion — "larger than main-memory working sets" — and the
only place the DuckDB join trades an extra pass over both sides for locality.

### Step 6 — the other shortcut: a perfect hash join

> **In:** Schuh et al.'s lesson 7 (array joins on dense keys). **Out:** the
> condition under which DuckDB skips the hash table altogether.

`CanUsePerfectHashJoin` (`physical_hash_join.cpp:278–288`) asks for a single
integral join key whose build-side min and max are known from statistics,
then `PerfectHashJoinExecutor::CanDoPerfectHashJoin` checks that the key range
`max − min` is at most `MAX_BUILD_SIZE = 1,048,576` (`perfect_hash_join_executor.cpp:121–122`)
and that the rows do not outnumber that range (`:128`). If so the table is a
plain array indexed by `key − min`: no hash, no collision, no probe loop. It is
Schuh et al.'s array join (NOPA, lesson 7: "up to 44%" faster on dense keys)
as an optimiser rule. Lane 1's keys are sparse on purpose, so this shortcut
never applies to it.

## How to read the source (with the concepts in hand)

1. **`ht_entry.hpp`** — the 64-bit entry (Step 1). Ten minutes.
2. **`join_hashtable.hpp:100–145, 620–645`** — constants: salt threshold,
   load factors, capacity.
3. **`join_hashtable.cpp:186–277`** — the slot/salt extraction and the probe
   loop (Steps 2 and 4). Then **`:771–900`** for the build-side insert.
4. **`physical_hash_join.cpp:1160–1185`** (Finalize tasks) and
   **`:1985–2030`** (the external decision): Steps 3 and 5.
5. **`radix_partitioning.hpp` and `.cpp:1–150`** — where the partition number
   comes from.
6. Skip, on a first pass: the MARK/ANTI/OUTER join machinery, the Bloom and
   prefix-range filters (topic 47's reading guide covers them), and the
   dictionary emission paths.

## Where each step lives in this topic

| step | where | what to look at |
|---|---|---|
| 1 | `experiments/src/npj.rs` | `Table::capacity_for` and 8-byte slots: lane 1's table is DuckDB's pointer array with the tuple in the slot |
| 2 | `experiments/src/hash.rs` | `radix_of` (low bits) and `slot_of` (high bits): the same three-purposes-one-hash design with a different split |
| 3 | `experiments/src/radix.rs` | `partition`: the histogram-and-scatter DuckDB does with its `RadixPartitionedTupleData` appends |
| 4 | `Table::probe`, `probe_batched` | the probe loop without a salt; DuckDB's salt threshold at 64 KiB sits well below lane 1's smallest table (256 KiB) |
| 5 | notes.md | the reference crossover: how far above the 16 MiB L2 a build side must be before partitioning pays on the M3 |
| 6 | `gen.rs` | `key_of` makes keys sparse; a dense variant is exercise 6 |

## Questions (answer in notes.md)

1. DuckDB stores the key's hash in the row and reloads it in `Finalize`. What
   does that save, and what does it cost in row width? (Compare with Bandle et
   al. §5.4.2's 8 B hash in a 32 B tuple.)
2. Salt false-match rate is about 2⁻¹⁶ per occupied entry probed. At lane 1's
   load factor of up to 0.5, estimate the expected number of entries examined per
   probe, and from it how often the salt saves a row dereference.
3. `max_tasks_per_partition = max(num_threads / num_partitions, 1)` (line
   1171). With 16 partitions and 11 threads, how many tasks per partition,
   and what happens to load balance if one partition holds half of the rows?
4. The external trigger compares a reservation with `total_size`. Construct
   the smallest build side that goes external on a machine with a 4 GiB
   reservation, assuming 32-byte rows and the default load factor, and state
   what the pointer table costs on top.

## Done when

Answer each before unfolding it.

- [ ] You can name what DuckDB uses its radix bits for.

  <details><summary>Answer</summary>

  Two things, neither of them cache locality for an in-memory join. (1) Build
  rows are appended into `2^radix_bits` partitions (4 bits, 16 partitions, by
  default) so that Finalize can hand the insertion of each partition's rows to
  worker threads (`physical_hash_join.cpp:412, 1164–1178`). (2) When the build
  does not fit in the reserved memory, the partitions are the unit that is
  built, probed and spilled in rounds (`join_hashtable.cpp:2462–2481,
  2656–2692`). The probe in the in-memory case is against one global pointer
  table.

  </details>

- [ ] You can split a hash into slot, salt and partition.

  <details><summary>Answer</summary>

  Slot: the low `log2(capacity)` bits (`hash & bitmask`). Salt: the top 16 bits.
  Partition: the `radix_bits` bits below the salt, `Shift = 48 − radix_bits`.
  For hash 0xABCD123456789ABC, 4 radix bits and 2²¹ entries: salt 0xABCD,
  partition 1 (bits 44–47), slot 0x189ABC.

  </details>

- [ ] You can size the pointer table for a build side.

  <details><summary>Answer</summary>

  `next_power_of_two(count × 2.0)` entries of 8 bytes (`join_hashtable.hpp:635–641`).
  For 1,000,000 rows: 2,097,152 entries = 16 MiB, plus the rows. With 1.5
  (the external load factor) the same build is `next_power_of_two(1,500,000)` =
  2,097,152, the same size — the factor only changes the power of two at the
  boundaries.

  </details>

- [ ] You can say why the salt is off for small tables.

  <details><summary>Answer</summary>

  `USE_SALT_THRESHOLD = 8192` entries (64 KiB): below it the pointer table is
  cache-resident, so the row dereference the salt would avoid is not a cache
  miss and the extra compare is pure cost. The constant's own comment: only
  compare salts "if the capacity is larger than 8192 so that it does not fit
  into the CPU cache" (`join_hashtable.hpp:140–142`).

  </details>

- [ ] You can state the relationship to the papers.

  <details><summary>Answer</summary>

  In memory, DuckDB is a no-partitioning join with prefetch-friendly batched
  probes (2,048-key vectors) — the BHJ of Bandle et al.; its table is the
  pointer-and-salt variant. Out of memory it is a radix-partitioned join, the
  one case Bandle et al. concede to partitioning. A cache-sized radix pass of
  the kind Balkesen et al. and Schuh et al. tune does not appear anywhere in
  the engine.

  </details>

## References

- DuckDB v2.0.0 (`d54cbbe3df`): `src/include/duckdb/execution/ht_entry.hpp`,
  `src/include/duckdb/execution/join_hashtable.hpp`,
  `src/execution/join_hashtable.cpp`,
  `src/execution/operator/join/physical_hash_join.cpp`,
  `src/execution/operator/join/perfect_hash_join_executor.cpp`,
  `src/include/duckdb/common/radix_partitioning.hpp`,
  `src/common/radix_partitioning.cpp`.
- Topic 11 ([README](../11-execution-models/README.md) §5) for the vectorised
  probe and the hash-join skeleton; topic 47's
  [reading-duckdb-join-filters.md](../47-semijoin-reduction/reading-duckdb-join-filters.md)
  for the Bloom, bitmap and IN-list filters built from the same table.
- Previous: [reading-bandle-to-partition.md](reading-bandle-to-partition.md).
  Next: [reading-datafusion-hash-join.md](reading-datafusion-hash-join.md).
