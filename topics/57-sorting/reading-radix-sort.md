# Radix sort on normalized keys: counting passes instead of comparisons, and the four ways it loses

Once a row's key is a byte string (reading-duckdb-sort.md), the comparison
sort's `n log₂ n` comparisons are no longer forced: the key can be sorted by
*looking at its bytes*, one byte at a time, with no comparison at all. That is
**radix sort**, and it is what lane 2 asks you to write. This chapter builds
it from counting sort, works both orders on small examples, reads how DuckDB's
vendored `ska_sort` does it in place, and then does the part the papers do
quickly: list exactly which inputs make it lose to the comparison sort of
lane 1, so the lane-2 numbers can be read as a *map* rather than a trophy.

Sources: Kuiper & Mühleisen, ICDE 2023 §VI-C (read in full); DuckDB at
`d54cbbe3df`, `third_party/ska_sort/ska_sort.hpp` and `src/common/sort/sorted_run.cpp`
(line numbers are that commit's). The four objections to radix sort in Step 5
are Graefe's, as quoted in the paper; Graefe's survey itself is behind a
paywall and was not read.

## The problem in one sentence

A comparison sort spends ~log₂ n comparisons and as many data-dependent
branches per element; a byte string already says where each element belongs
one byte at a time — how do you use that without paying 8 or 16 full passes
over the data, and when is the comparison sort still the better answer?

## The concepts, step by step

### Step 1 — counting sort: the primitive

> **In:** a list of keys that are small integers (one **digit**, 0…255).
> **Out:** the three-phase routine — histogram, prefix sum, scatter — that
> every radix sort repeats.

A **digit** here is one byte of the key, a value 0…255. **Counting sort** sorts
by one digit in O(n) with no comparisons:

1. **Histogram:** count how many keys have each digit value.
2. **Prefix sum:** turn counts into **offsets** — digit d's elements start at the
   sum of the counts of all smaller digits.
3. **Scatter:** walk the input once, writing each element to `output[offset[d]++]`.

Worked on digits `[3, 1, 3, 0, 2, 1, 3]` (n = 7): counts `0:1, 1:2, 2:1, 3:3`;
offsets `0:0, 1:1, 2:3, 3:4`; scatter in order: 3 goes to slot 4 (offset 4 → 5),
1 to slot 1, the second 3 to slot 5, 0 to slot 0, 2 to slot 3, 1 to slot 2, the
third 3 to slot 6: output `[0, 1, 1, 2, 3, 3, 3]`. The scatter is **stable**:
equal digits keep their input order, because the offset for a digit only
increases.

Why it matters: the scatter is a write to one of 256 destinations per
element. Its cost is memory traffic and cache behaviour (256 write streams),
not comparisons — which is also why it has no unpredictable branch.

### Step 2 — LSD: one stable pass per byte, last byte first

> **In:** Step 1's stable counting sort; keys of `w` bytes. **Out:** the
> algorithm, its cost in passes, and the "skip" optimisation.

**LSD** (least significant digit first) sorts by the *last* byte, then the
second-to-last, … then the first, each time with a stable counting sort. Stability
is what makes it work: after the pass on byte `j`, elements are ordered by bytes
`j…w−1`, and a later pass on a more significant byte reorders only where that
byte differs, leaving ties in the order the earlier passes established.

Worked on two-digit decimal keys `[31, 12, 33, 10, 21]` (digit = decimal digit):
pass on units → `[10, 31, 21, 12, 33]` (0, then the 1s in input order 31, 21,
then 2, then 3); pass on tens, stable → tens 1: `10, 12`; tens 2: `21`; tens 3:
`31, 33` → `[10, 12, 21, 31, 33]`. Sorted, two passes for a 2-digit key.

The cost is `w` full passes: for a `u64` key, 8 passes, each reading and writing
every row. For lane 2's 16-byte `Row64` rows (the key plus an 8-byte payload) a
pass over 10,000,000 rows moves 160 MB in and 160 MB out. LSD also needs a
second buffer of the same size to scatter into, and the passes alternate
between the two. All eight histograms can be built in a *single* read pass
over the data (one 256-entry array per byte position), so the counting
phases cost 1 extra read, not 8.

The **skip** optimisation: after the histograms exist, a byte position where
*one* bucket holds all n elements does not change the order — skip its pass
entirely. For `u64` keys that are all below 2²⁰ the top five bytes are zero for
every row, so 5 of the 8 passes are skipped. The paper adds the same
optimisation to both its LSD and MSD sorts: "an optimization ... that avoids
copying data when all counted values belong to the same bucket" (§VI-C).

Why it matters: LSD's work is `w × n` regardless of the data, which makes it
the algorithm of choice for short keys; the paper selects it "when the key
size is ≤ 4 bytes" (§VI-C).

### Step 3 — MSD: bucket by the first byte, recurse

> **In:** Step 1. **Out:** the recursive algorithm, the small-bucket cutoff,
> and why MSD wins for long keys.

**MSD** (most significant digit first) counting-sorts by the *first* byte, which
splits the rows into up to 256 buckets that are already in order relative to
each other, then recurses into each bucket on the *second* byte, and so on. A
bucket of one row needs nothing more; a bucket of a handful of rows is cheaper
to finish by **insertion sort** (compare the whole key; the paper's cutoff is
24 tuples, §VI-C) than by another 256-bucket pass.

Worked on the same keys: bucket by tens → `1: [12, 10]`, `2: [21]`, `3: [31, 33]`;
the bucket `1` recurses on units → `[10, 12]`; the others are already in
order (`31, 33`), so MSD stops after touching only the buckets that needed it.
On random 16-byte keys the first two bytes already split 10,000,000 rows into
65,536 buckets of about 153 rows, so the later bytes are visited only for a
shrinking part of the data: MSD touches *far fewer than* `w × n` bytes when
keys are long and distinct.

An **in-place** MSD ("American flag sort") avoids the second buffer by
permuting elements into their buckets by cycle-following: while the
element at position `i` is not in its bucket, swap it with the element at its
bucket's next free slot. DuckDB's vendored `ska_sort` does this
(`ska_sort.hpp:1135-1213`, `american_flag_sort`, used for partitions below
1,024 rows) and a similar `ska_byte_sort` (`:1216`) above that.

Why it matters: LSD's cost is fixed by the key width; MSD's depends on the data.
For wide keys MSD does less work; for data with structure it does more
unpredictable work. Which is why the paper uses LSD for ≤ 4 bytes and MSD above.

### Step 4 — the key is not just a number: normalized keys and tie-breaks

> **In:** Steps 2–3 and reading-duckdb-sort.md Steps 2–3. **Out:** how a
> radix sort handles a key that is a *prefix* of the real order.

For `(int, string)` rows the radix sort sorts the normalized key: 4 bytes of
sign-flipped big-endian integer and a 12-byte padded string prefix, 16 bytes.
Two kinds of equality appear that a plain integer sort never meets:

- **Key-equal, row-different.** Two rows can have identical 16 key bytes but
  different strings, when the strings agree on their first 12 bytes and differ
  later. The radix sort leaves such rows adjacent; a **tie-break** pass must
  then sort each run of equal keys by the full strings. If the data has many
  such runs (every string starts with the same 20 bytes — lane 2's `b3` case)
  the tie-break *is* the sort, and the radix passes were wasted.
- **Skippable bytes.** Bytes that are equal for every row of the current bucket
  decide nothing; MSD detects "all in one bucket" and moves to the next byte
  without moving data (the paper's optimisation again; in the pinned DuckDB,
  also bytes proven constant from statistics, `sort_skippable_bytes`,
  `ska_sort.hpp:1126`).

DuckDB at the pin makes the tie-break explicit. It radix-sorts at most the
first 8 bytes of the key (`ska_sort_width = min(sort_width, 8)`,
`sorted_run.cpp:246`) and, if the key is longer (`requires_next_sort`, line
245), sends buckets that tie on those 8 bytes to a *comparison* sort on the full
key (`ska_sort.hpp:1454-1459`). Partitions below 128 rows skip radix entirely
and go to `pdqsort_branchless` (`ska_sort.hpp:1097-1107`, `1487`): the small-n
cutoff.

Why it matters: a normalized-key radix sort is a *prefix* sort followed by a
comparison sort for the leftovers. Its speed depends on how often the prefix
decides the order.

### Step 5 — the four ways it loses

> **In:** Steps 1–4. **Out:** a list of input shapes, each tied to a lane-2
> measurement, with the mechanism.

The paper quotes Graefe's objections to radix sort for databases (§VI-C): (1) "if
keys are long and the data contains duplicate keys, many of the passes over the
data are unproductive"; (2) it "is most effective if values are uniformly
distributed"; (3) "if input records are nearly sorted, and the keys have a
common prefix, the initial passes of radix sort are rather ineffective"; (4)
"radix sort has worse cache efficiency than comparison-based sorts". The
reference solution for lane 2 (recorded in `notes.md`, under *Reference numbers
for the exercise lanes*) measures each against `sort_unstable_by`:

| shape | what the radix sort does | mechanism |
|---|---|---|
| **small n** | LSD loses below about ten thousand rows | each pass sets up 256 counters and reads them back; for `n` of a few hundred the comparison sort's `n log n` is smaller than `256 × passes` |
| **already sorted / reverse sorted** | loses by more than an order of magnitude | `sort_unstable` finds the run in n − 1 comparisons (reading-rust-sort.md Step 2); radix does all its passes regardless |
| **shared prefix longer than the key prefix** | falls back to the comparison tie-break after wasted passes | Step 4 |
| **few distinct keys** | LSD skips the constant bytes for free and still wins; the simple MSD loses, paying a counting pass per bucket to learn the same thing | objection (2); the comparison sort has a dedicated equal-keys fast path (reading-rust-sort.md Step 3) |
| **uniform keys, large n** | wins, and the win did not shrink at 50 million rows | the case the paper's Fig. 7 "Random" column shows; objection (4) did not reproduce at this scale |

The notes record the numbers behind each row, with the machine load next to
them, and the crossing points. Read them as ratios on one machine, not as
constants: where the crossover lies depends on the cache sizes and on how
fast the comparison sort's branchless partition is, which is exactly what
reading-rust-sort.md describes.

Why it matters: the paper's premise is that radix sort is "potentially much
faster than quicksort when there are many tuples, as k does not depend on the
input size" (§VI-C, `k` being the key size), and its own Fig. 7 already
shows pdqsort ahead of its radix sort on the 128-distinct-value data with one
key column. A database picks a sort by *statistics* (the paper's §IX: "A heuristic
that takes these variables into account could improve the algorithm choice"),
and the pinned DuckDB's width classes and skippable bytes are the beginning of
that.

### Step 6 — memory and parallelism

> **In:** Step 3. **Out:** what the two radix flavours cost in memory, and
> what the paper's `§VII` pipeline does about it.

LSD and the simple MSD sort need a second buffer of the same size as the data
(the paper: "radix sort uses twice as much memory because it needs auxiliary
memory of the same size as the input, whereas quicksort sorts in place",
§VI-C); the in-place American flag variant does not, but swaps more. DuckDB
sorts thread-local runs of normalized keys separately, then merges the sorted
runs with a parallel 2-way cascaded merge sort whose partitions are found by
Merge Path (§VII): the radix sort only ever sees a *run* that fits in the
thread's memory reservation, and the merge phase uses `memcmp`-ordered key
comparisons. At the pin, runs are merged in `sorted_run_merger.cpp`, where the
final merged partitions go through `pdqsort_branchless` and vergesort
(`:657`, `:663`).

## How to read the source

1. The paper §VI-C and Figures 7–8, then §VII's pipeline description (Fig. 9).
2. `third_party/ska_sort/ska_sort.hpp:1097-1132` (thresholds, the skip), then
   `:1135-1213` (`american_flag_sort`) — read the swap loop at 1164–1193, which
   permutes in place without a second buffer.
3. `third_party/ska_sort/ska_sort.hpp:1425-1487` — `SortStarter`, where the
   `next_sort` fallback is wired, and the `<128, 1024>` constants.
4. `src/common/sort/sorted_run.cpp:204-262` — the extract-key functor
   (`ska_sort_width`, `ByteIsSkippable`) and the vergesort/ska_sort pairing.

## Questions (answer in notes.md)

1. A `Row64` key set has only 256 distinct values spread over the full 64-bit
   range (e.g. `v · 0x0101010101010101`). How many LSD passes are *not*
   skippable? How many MSD levels are visited before every bucket is a single
   distinct value?
2. Predict, before running lane 2, the shared-prefix length at which the
   `(int, string)` radix sort stops beating the comparison sort, given the
   12-byte string prefix in the key. Which part of the cost is the tie-break?
3. LSD with `w = 8` bytes on 16-byte rows reads and writes the data 8 times;
   MSD with an insertion-sort cutoff of 24 on 10⁷ random keys touches how many
   levels on average? (Count: after level `ℓ`, buckets have `10⁷ / 256^ℓ` rows.)
4. Why can't the American-flag in-place scatter be made *stable*, and does a
   database sort need stability? (See the `sort_by`/`ORDER BY` discussion in
   reading-rust-sort.md Step 4.)
5. `DESC` on a string column flips every byte of the key. Does that change
   anything about which passes can be skipped? Does `NULLS FIRST` change the
   first byte's distribution?

## Done when

Answer each before unfolding it.

- [ ] You can run counting sort, LSD and MSD by hand on a small input.

  <details><summary>Answer</summary>

  Digits `[3,1,3,0,2,1,3]` → counts `1,2,1,3` → offsets `0,1,3,4` → output
  `[0,1,1,2,3,3,3]`. Two-digit keys `[31,12,33,10,21]`: LSD sorts units
  (`[10,31,21,12,33]`) then tens stably (`[10,12,21,31,33]`); MSD buckets by
  tens (`1:[12,10], 2:[21], 3:[31,33]`) and recurses only into bucket 1
  (`[10,12]`).

  </details>

- [ ] You can explain why LSD needs a stable per-digit sort and MSD does not.

  <details><summary>Answer</summary>

  LSD sorts by the least significant byte first and relies on each later,
  more significant pass preserving the order the earlier passes established
  among equal digits — that is exactly stability. MSD finishes the most
  significant byte first and then sorts *within* each bucket independently,
  so no ordering from an earlier pass needs to survive.

  </details>

- [ ] You can state the paper's algorithm-selection rule and the pinned DuckDB's different structure.

  <details><summary>Answer</summary>

  The paper: LSD when the key is ≤ 4 bytes, MSD otherwise, with insertion
  sort for buckets of ≤ 24 tuples; pdqsort instead when the key contains
  strings (§VI-C, §VII). The pin: vergesort detects existing runs; its
  fallback is ska_sort radix-sorting at most the first 8 key bytes, skipping
  statistics-proven constant bytes; partitions under 128 rows go to
  `pdqsort_branchless`; buckets still tied after 8 bytes go to a full-key
  comparison sort (`sorted_run.cpp:244-260`, `ska_sort.hpp:1097-1107,1454-1459`).

  </details>

- [ ] You can list Graefe's four objections to radix sort and the lane-2 input that exercises each.

  <details><summary>Answer</summary>

  (1) long keys with duplicates → unproductive passes: lane 2's `b3`
  (20-byte shared prefix) and the prefix-length sweep; (2) wants uniform
  values → the 256-distinct-keys shape; (3) nearly sorted input with a common
  prefix → already-sorted and sorted-plus-1%-swaps shapes; (4) worse cache
  behaviour → the large-n rows, where scatter into 256 streams stresses the
  cache. The measurements are in `notes.md`.

  </details>

- [ ] You can explain what the tie-break pass is and when it dominates.

  <details><summary>Answer</summary>

  Rows whose 16 normalized-key bytes are equal but whose strings are longer
  than the 12-byte prefix are not ordered by the radix passes; each run of
  equal keys must be sorted on the full strings with a comparison sort. When
  the strings share a long prefix, nearly every row lands in such a run, so
  the sort degenerates into a comparison sort plus the cost of the wasted
  radix passes (lane 2, case `b3`).

  </details>

## References

- Laurens Kuiper, Hannes Mühleisen, **"These Rows Are Made for Sorting and
  That's Just What We'll Do"**, ICDE 2023 — §VI-C (radix sort, Graefe's
  objections), Figures 7–8, §VII (pipeline, Fig. 9), §IX (future work).
- Goetz Graefe, **"Implementing Sorting in Database Systems"**, ACM Computing
  Surveys 2006 — objections quoted via the paper above; not read directly.
- DuckDB at `d54cbbe3df`: `third_party/ska_sort/ska_sort.hpp`,
  `src/common/sort/sorted_run.cpp`, `src/common/sort/sorted_run_merger.cpp`.
- Previous: [reading-duckdb-sort.md](reading-duckdb-sort.md). Next:
  [reading-rust-sort.md](reading-rust-sort.md).
