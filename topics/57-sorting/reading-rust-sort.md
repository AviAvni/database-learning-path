# What `sort_unstable_by` actually does: ipnsort and driftsort in Rust's standard library

Lane 1 of this topic is one line — `rows.sort_unstable_by(|a, b| a.key.cmp(&b.key))`
— and the whole rest of the topic is measured against it. That line is not a
textbook quicksort. In the 1.99.0 library `slice::sort_unstable` is **ipnsort** and
`slice::sort` is **driftsort** (both by Lukas Bergdoll and Orson Peters; the
source comments say so), and
both are engineered around exactly the things a database sort meets: inputs
that are already sorted, inputs with few distinct values, rows that are cheap
or expensive to copy, comparators that may be arbitrarily slow. This chapter
reads them so that "the comparison sort we beat" in later chapters is a
specific algorithm with specific thresholds, not a strawman.

Every anchor is the Rust standard library of **rustc 1.99.0 (b940084d7,
2026-09-28)**, files under `library/core/src/slice/sort/` (and one in
`library/alloc/src/slice.rs`), quoted with the line numbers they occupy in that
release's `rust-src`. The design documents that the source comments point to
(`writeup/ipnsort_introduction`, `writeup/driftsort_introduction` in
`Voultapher/sort-research-rs`) were not used for any number in this chapter;
every number below is from the code or from a run in this repo.

## The problem in one sentence

How does a general-purpose comparison sort avoid its three classic failure
modes — a bad pivot, a long already-sorted input, and many equal keys — while
keeping the branch-free inner loop that makes it fast on random data?

## The concepts, step by step

### Step 1 — the vocabulary: runs, pivots, partitions

> **In:** nothing. **Out:** the four terms every later step uses.

A **comparison sort** orders elements only by asking `is_less(a, b)`. A
**quicksort** picks a **pivot** element, **partitions** the slice so every
element less than the pivot is on its left and the rest on its right, and
recurses on both sides. With a good pivot each level halves the problem and
the sort costs about n·log₂ n comparisons; with a bad pivot (always the
smallest element) it degrades to n²/2. A **run** is a maximal stretch of the
input that is already in order — non-decreasing, or strictly decreasing (which
can be reversed in place). **Introsort** is quicksort with a depth limit: when
too many partitions have been badly unbalanced, switch to **heapsort**, whose
worst case is O(n log n), so the total is bounded no matter the input.

Why it matters: a database sort is rarely given random data. Tables are
loaded in key order, a sort by `created_at` over an append-only table is
already sorted, and `GROUP BY` keys repeat. A sort that is only fast on random
input loses to one that notices structure.

### Step 2 — the entry point: tiny inputs, then find a run

> **In:** Step 1's vocabulary. **Out:** the two checks `sort_unstable` makes
> before it partitions anything, with the code.

```rust
// library/core/src/slice/sort/unstable/mod.rs, rustc 1.99.0 — lines 114-138 (ipnsort).
// The lines to read: 119 (find the run), 124-131 (the O(n) exit), 136-137 (the depth limit).
 114  fn ipnsort<T, F>(v: &mut [T], is_less: &mut F)
 ...
 118      let len = v.len();
 119      let (run_len, was_reversed) = find_existing_run(v, is_less);
 ...
 124      if run_len == len {
 125          if was_reversed {
 126              v.reverse();
 127          }
 ...
 131          return;
 132      }
 133  
 134      // Limit the number of imbalanced partitions to `2 * floor(log2(len))`.
 135      // The binary OR by one is used to eliminate the zero-check in the logarithm.
 136      let limit = 2 * (len | 1).ilog2();
 137      crate::slice::sort::unstable::quicksort::quicksort(v, None, limit, is_less);
```

First, in the public `sort` (same file, line 49) inputs of 20 elements or
fewer go straight to insertion sort: the comment says a general-purpose
program prefers the small instruction-cache footprint of insertion sort to a
fancier small sort. Then `ipnsort` calls `find_existing_run`
(`shared/mod.rs:20-47`), which looks at the first two elements to decide
whether the run is strictly descending or non-descending, and then walks until
the run breaks. If the run covers the whole slice the sort returns after
reversing (if needed): exactly **n − 1 comparisons**, no partitioning. That is
the entire reason a sort of already-sorted or reverse-sorted data takes about
as long as a scan.

The depth limit (line 136) is `2 · ⌊log₂ n⌋` imbalanced partitions before
falling back to heapsort. Worked: `n = 1,000,000` has ⌊log₂ n⌋ = 19 (2¹⁹ =
524,288 ≤ 10⁶ < 2²⁰), so the limit is 38; `n = 10,000,000` has ⌊log₂ n⌋ = 23
(2²³ = 8,388,608 ≤ 10⁷ < 2²⁴), limit 46.

Why it matters: the existing-run check is why lane 1's sorted-input
measurement is nearly free, and why a radix sort — which has no such check —
loses on presorted data (README, "where radix loses").

### Step 3 — the quicksort loop and the equal-keys trick

> **In:** Step 2's limit and the pivot vocabulary. **Out:** how ipnsort treats
> repeated keys, which is what a low-cardinality `ORDER BY` column needs.

```rust
// library/core/src/slice/sort/unstable/quicksort.rs, rustc 1.99.0 — lines 29-60.
// The lines to read: 30-33 (small-sort base case), 37-40 (heapsort bail-out), 50-59 (equal-to-ancestor).
  29      loop {
  30          if v.len() <= T::small_sort_threshold() {
  31              T::small_sort(v, is_less);
  32              return;
  33          }
  ...
  37          if limit == 0 {
  38              heapsort::heapsort(v, is_less);
  39              return;
  40          }
  ...
  45          let pivot_pos = choose_pivot(v, is_less);
  ...
  50          if let Some(p) = ancestor_pivot {
  51              if !is_less(p, &v[pivot_pos]) {
  52                  let num_lt = partition(v, pivot_pos, &mut |a, b| !is_less(b, a));
  ...
  56                  v = &mut v[(num_lt + 1)..];
  57                  ancestor_pivot = None;
  58                  continue;
  59              }
  60          }
```

Read lines 50–59. `ancestor_pivot` is the pivot of the partition this slice
is the right-hand side of — so everything in `v` is ≥ it. If the pivot chosen
now is *not greater than* the ancestor (`!is_less(p, pivot)`), then the new
pivot equals the ancestor and is the minimum of the slice. The code partitions
with the **flipped** comparison, `!is_less(b, a)`, which puts the elements
*equal* to the pivot on the left and the strictly greater ones on the right,
then skips the whole equal block (line 56) and continues with the greater
ones. Worked by tracing the code on keys A < B < C, many copies of each. Suppose
the first pivot is B: the left side holds the As, the right side holds every
B and C and has ancestor B. If the next pivot chosen on the right is B, it is
not greater than its ancestor, so the flipped partition sends every B left and
every C right in one pass, and the loop continues on the Cs with no ancestor.
A slice of only Cs then partitions with `num_lt = 0` (nothing is less than the
pivot), the right side keeps ancestor C, and the next round finds the pivot
equal to it and removes the whole block in one pass. By this reading, each
distinct key costs a bounded number of linear passes rather than a log n
recursion — a claim lane 1b's low-cardinality case lets you test, not one
this chapter measured.

`partition` itself is chosen by element size (`quicksort.rs:139-155`): a type
of at most 96 bytes (`MAX_BRANCHLESS_PARTITION_SIZE`, line 140) uses
`partition_lomuto_branchless_cyclic`, a **branchless** partition (the
comparison result becomes an arithmetic index increment, so there is no
conditional jump to mispredict); larger types use `partition_hoare_branchy_cyclic`,
because copying a big element under a branchless scheme costs more than the
mispredictions it saves. The base case `small_sort` for slices of at most `small_sort_threshold()`
elements depends on the element type (`shared/smallsort.rs:142-170`): a
`Copy` type of at most 8 bytes whose scratch fits in 4 KiB — a bare `u64` —
gets a **sorting network** (`small_sort_network`, threshold 32): a fixed
schedule of compare-and-swap operations that does not depend on the data, so
it has no data-dependent branches (topic 17's [branchless selection](../17-simd/README.md)
section is the same idea applied to filters; topic 17 has no sorting-network
chapter). A larger `Freeze` type whose 48-element scratch still fits in 4 KiB —
the 16-byte `Row64` of lane 1a: 16 × 48 = 768 ≤ 4,096 — gets
`small_sort_general` (threshold 32, `:113-121`); anything else, and any type
that is not `Freeze`, falls back to insertion sort with threshold 16
(`:174,199`). So lane 1a's bare-`u64` floor and its `Row64` comparator row
differ in more than row width: they use different base cases.

Why it matters: this is the comparison sort the rest of the topic competes
against. It already has a branchless partition and an equal-key fast path.
When a radix sort wins by 2× over it (README), that win is over *this*
algorithm, not over a naive quicksort.

### Step 4 — the stable one: driftsort

> **In:** Steps 1–3. **Out:** how `slice::sort` (stable) finds runs, decides
> how much scratch memory to allocate, and merges.

A **stable** sort keeps equal elements in their input order. `slice::sort` and
`sort_by` are driftsort (`stable/mod.rs:22-29`). Its entry allocates scratch:

```rust
// library/core/src/slice/sort/stable/mod.rs, rustc 1.99.0 — lines 111-135 (lines to read: 111-117, 134-135).
 111      const MAX_FULL_ALLOC_BYTES: usize = 8_000_000; // 8MB
 112      let max_full_alloc = MAX_FULL_ALLOC_BYTES / size_of::<T>();
 113      let len = v.len();
 114      let alloc_len = cmp::max(
 115          cmp::max(len - len / 2, cmp::min(len, max_full_alloc)),
 116          SMALL_SORT_GENERAL_SCRATCH_LEN,
 117      );
 ...
 134      let eager_sort = len <= T::small_sort_threshold() * 2;
 135      crate::slice::sort::stable::drift::sort(v, scratch, eager_sort, is_less);
```

Worked for lane 1's 16-byte `Row64` and `n = 10,000,000`: `max_full_alloc =
8,000,000 / 16 = 500,000`; `alloc_len = max(max(10,000,000 − 5,000,000,
min(10,000,000, 500,000)), 48) = 5,000,000` elements, i.e. 80 MB of scratch
for 160 MB of data: half. For `n = 100,000` the middle term is
`min(100,000, 500,000) = 100,000` and the larger of the two is `100,000`: a full
copy (the comment at lines 95–110 says a full-size buffer lets stable
quicksort run, "which allows better performance on random and low-cardinality
distributions"). So stable sort memory scales like n for small inputs and
n − n/2 for large.

In `drift.rs` the loop builds **runs** and merges them in the order a
**powersort** merge tree dictates (lines 18–19, 122–135; the heuristic is from
Munro and Wild's "Nearly-Optimal Mergesorts"). The line that matters for a
database is 37–44:

```rust
// library/core/src/slice/sort/stable/drift.rs, rustc 1.99.0 — lines 36-44 (lines to read: 36-41).
  36      const MIN_SQRT_RUN_LEN: usize = 64;
  37      let min_good_run_len = if len <= (MIN_SQRT_RUN_LEN * MIN_SQRT_RUN_LEN) {
  ...
  40          cmp::min(len - len / 2, MIN_SQRT_RUN_LEN)
  41      } else {
  42          sqrt_approx(len)
  43      };
```

A pre-existing run is only *used* if it is at least `min_good_run_len` long:
64 for inputs up to 4,096 elements, √n above. For `n = 10,000,000` that is
√10⁷ ≈ 3,162. The comment (lines 31–35) gives the reason: one short run would
force several merges and shrink the quicksort that handles the rest. Shorter
stretches become **unsorted runs** of `min_good_run_len` (`create_run`, lines
226–259) that are later sorted by the stable quicksort in one go — unless the
input is tiny (`eager_sort`, ≤ 2 small-sort thresholds), in which case each
stretch is small-sorted immediately. Fully ascending or descending input is
sorted with exactly n − 1 comparisons (doc comment, lines 15–16).

Why it matters: the stable sort is the right default when the row's original
position is part of the order (ties must keep arrival order) — a multi-pass
`ORDER BY a` then `ORDER BY b` plan relies on it, and the **subsort** of the
DuckDB paper (reading-duckdb-sort.md, Step 1) is exactly a stable-sort
pipeline. The price is the scratch memory above.

### Step 5 — `sort_by_cached_key`: lane 3's idea, already in std

> **In:** Step 1. **Out:** the standard library's version of "compute the key
> once", and why it is not the same as a normalized key.

When computing the sort key costs real work, `slice::sort_by_cached_key`
computes each element's key once instead of inside the comparator:

```rust
// library/alloc/src/slice.rs, rustc 1.99.0 — lines 312-334 (lines to read: 320-325, 326-333).
 312      pub fn sort_by_cached_key<K, F>(&mut self, f: F)
 ...
 318          macro_rules! sort_by_key {
 319              ($t:ty, $slice:ident, $f:ident) => {{
 320                  let mut indices: Vec<_> =
 321                      $slice.iter().map($f).enumerate().map(|(i, k)| (k, i as $t)).collect();
 ...
 325                  indices.sort_unstable();
 326                  for i in 0..$slice.len() {
 327                      let mut index = indices[i].1;
 328                      while (index as usize) < i {
 329                          index = indices[index as usize].1;
 330                      }
 331                      indices[i].1 = index;
 332                      $slice.swap(i, index as usize);
 333                  }
```

It builds `(key, index)` pairs once (line 321), sorts the pairs (line 325), and
then applies the permutation to the slice in place with swaps (326–333). The
doc says why: if `f` is expensive — its example is `k.to_string()` — calling
it twice per comparison is `O(n log n)` key computations; this does `n`. That
is the Schwartzian transform, and it is the structure of lane 3: evaluate the
collation once per string, sort `(key, row)` pairs, apply the order. What it
does *not* do is make the key `memcmp`-able; `K: Ord` is compared by `K`'s own
`cmp`. The next chapters add that second half — a key that is a flat byte
string — which is what lets a radix sort or a plain `memcmp` replace the
comparator entirely.

### Step 6 — what the lanes measure, in these terms

> **In:** Steps 2–5. **Out:** how to read the lane-1 numbers.

Lane 1a's `sort_unstable_by` over `Row64` is ipnsort with the branchless
partition (16 bytes ≤ 96). Its `sort_by` row is driftsort. Lane 1b's
`IntStr` rows are 12 bytes, so also branchless — but each comparison that ties
on `a` follows a pointer into an arena, and that cache miss, not the branch
predictor, is the cost (the paper's "random access" argument,
reading-duckdb-sort.md Step 1). Lane 1c's comparator is expensive per call
(three passes with UTF-8 decoding), which makes the *number* of comparisons —
about n·log₂ n for random input — the cost; Step 3's equal-key shortcut helps
only when keys are exactly equal, and there the collation's equal classes are
few.

## How to read the source

1. `unstable/mod.rs:42-52` (public entry, insertion sort up to 20), then
   `114-138` (`ipnsort`).
2. `shared/mod.rs:20-47` — `find_existing_run`; 28 lines for the whole O(n)
   fast path.
3. `unstable/quicksort.rs:21-80` (the loop), `139-155` (`inst_partition`),
   then one partition body (`partition_lomuto_branchless_cyclic`, line 249) once
   the loop is clear.
4. `shared/smallsort.rs:170-197` — thresholds, then `small_sort_network`.
5. `stable/mod.rs:94-136`, `stable/drift.rs:20-120` and `226-260`.
6. `library/alloc/src/slice.rs:312-355` — `sort_by_cached_key`.

## Questions (answer in notes.md)

1. Run lane 1a on already-sorted input and on input that is sorted except for
   one swapped pair. Which of Step 2's two exits does each take, and what
   would you expect the ratio to the random case to be?
2. `n = 50,000,000` rows of the 16-byte `Row64` through `sort_by`: how many
   bytes of scratch does `driftsort_main` allocate? Is it the same for
   `sort_unstable_by`?
3. A table scan returns rows in primary-key order and you `ORDER BY` a
   column that is 95% non-decreasing with a few out-of-order rows scattered
   in. Which of ipnsort and driftsort has an advantage, and why (Step 4's
   `min_good_run_len`)?
4. Why is `partition_hoare_branchy_cyclic` used for elements over 96 bytes
   even though a branchless partition mispredicts less?
5. `sort_by_cached_key` sorts `(K, usize)` pairs with `sort_unstable`. What
   would go wrong if the pair were `(K, u32)` and the slice had 2³² or more
   elements, and where does the code guard against it (line 344-351)?

## Done when

Answer each before unfolding it.

- [ ] You can say what `sort_unstable` does with an already-sorted or strictly descending input, and how many comparisons it costs.

  <details><summary>Answer</summary>

  `ipnsort` calls `find_existing_run` (`unstable/mod.rs:119`), which scans the
  maximal non-decreasing (or strictly descending) prefix; if it covers the
  whole slice the sort reverses it if it was descending and returns
  (`:124-131`) — n − 1 comparisons and at most one reversal pass, no
  partitioning.

  </details>

- [ ] You can explain how ipnsort handles many equal keys and why it makes few-distinct-value sorts fast.

  <details><summary>Answer</summary>

  If the pivot chosen for a slice is not greater than the pivot that created
  the slice (`!is_less(ancestor, pivot)`, `quicksort.rs:50-51`), it is the
  minimum, so the code partitions with the negated comparison to peel off all
  elements equal to it in one pass and continues on the strictly greater
  ones (`:52-58`). With k distinct keys the recursion removes one key's whole
  block per pass instead of splitting it recursively.

  </details>

- [ ] You can compute the heapsort fallback limit and the stable sort's scratch size for a given n.

  <details><summary>Answer</summary>

  Limit = 2·⌊log₂ n⌋ (`unstable/mod.rs:136`): 38 for n = 10⁶ (⌊log₂⌋ = 19),
  46 for n = 10⁷ (⌊log₂⌋ = 23). Scratch elements for `sort`:
  `max(max(n − n/2, min(n, 8,000,000/size_of::<T>())), 48)`
  (`stable/mod.rs:111-117`): for n = 10⁷ and 16-byte rows, 5,000,000 elements =
  80 MB; for n = 10⁵, 100,000 elements (a full copy).

  </details>

- [ ] You can say what `min_good_run_len` is and why driftsort ignores shorter existing runs.

  <details><summary>Answer</summary>

  64 for inputs up to 64·64 = 4,096 elements, ≈ √n above
  (`stable/drift.rs:36-43`). A short run would force several merge
  operations and cut down the size of the stable quicksort used on the
  rest; the comment sets the threshold at √n so only runs long enough to pay
  for their merges are kept. Shorter stretches become unsorted runs handed
  to the stable quicksort (`create_run`, `:226-259`).

  </details>

- [ ] You can say how `sort_by_cached_key` relates to a normalized sort key.

  <details><summary>Answer</summary>

  It computes each element's key once, sorts `(key, index)` pairs and applies
  the permutation (`alloc/src/slice.rs:320-333`): that removes repeated key
  computation (n instead of O(n log n) calls) — the same step as computing a
  collation sort key once. It does not make the key byte-comparable; the pairs
  are compared with `K`'s `Ord`. A normalized key additionally makes the
  comparison a `memcmp`, which is what enables radix sort.

  </details>

## References

- Rust standard library, rustc **1.99.0 (b940084d7, 2026-09-28)**:
  `library/core/src/slice/sort/{unstable,stable,shared}/`,
  `library/alloc/src/slice.rs` — read from the toolchain's `rust-src`.
- Design documents named by the source comments (not used for any number here):
  `Voultapher/sort-research-rs`, `writeup/ipnsort_introduction` and
  `writeup/driftsort_introduction` (cloned at `8d3eae4`).
- J. Ian Munro, Sebastian Wild, **"Nearly-Optimal Mergesorts: Fast, Practical
  Sorting Methods That Optimally Adapt to Existing Runs"**, ESA 2018 — named
  in `stable/drift.rs:122-123`; not read for this chapter.
- Previous: [reading-radix-sort.md](reading-radix-sort.md). Next:
  [reading-ips4o.md](reading-ips4o.md).
