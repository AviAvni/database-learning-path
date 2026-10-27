# IPS⁴o: the comparison sort that distributes into 256 buckets, in place, without a branch

Radix sort needs a key that is a byte string. Quicksort needs only a comparator
but touches every element about log₂ n times. **Samplesort** is the third
design point: it needs only a comparator, like quicksort, but distributes
every element into one of k ≈ 256 buckets per pass, so it makes only
log₂₅₆(n) ≈ 3 passes over 10 million elements instead of ~20. Axtmann,
Witt, Ferizovic and Sanders' **IPS⁴o** (ESA 2017) makes that work *in place*,
in parallel, and without a single data-dependent branch in the classification
loop. This chapter is the comparison-sort baseline for the radix lanes of this
topic: when a database sort has a comparator it cannot turn into bytes —
because the collation is not available as a sort key, or the type has no
order-preserving encoding — this is the algorithm family to reach for.

Sources: the paper, arXiv:1705.02257v2 (29 Jun 2017), read in full
(section numbers below are the paper's); the reference implementation
`ips4o/ips4o` at commit `08a5b92` (2025-01-15), header-only C++, quoted with the
line numbers the code occupies at that commit. The journal version (arXiv
2009.13569, which adds a radix variant) was not read for this chapter.

## The problem in one sentence

A comparison sort that partitions around one pivot is bound to ~log₂ n passes
over the data and one unpredictable branch per comparison — can a k-way
partition remove both, and still run in the input's own memory?

## The concepts, step by step

### Step 1 — samplesort: k−1 splitters, k buckets, one pass

> **In:** an unsorted array of n elements and a comparator. **Out:** the
> bucket-count arithmetic for k = 256, worked, and the vocabulary for
> Steps 2–4.

A **splitter** is a value that separates two adjacent buckets. **Samplesort**
(§3) picks k−1 splitters, then sends every element to the bucket whose range
contains it, then sorts each bucket recursively. To get *balanced* buckets it
draws **αk − 1 random samples** (α is the **oversampling factor**), sorts
them, and takes every α-th as a splitter: with more samples per splitter, the
splitters are closer to the true quantiles. The paper's configuration (§4.7)
is up to k = 256, α = 0.2 · log n, base-case size 16 and a block size of about
2 KiB; the reference code has the same defaults (`config.hpp:56`, `:64`, `:80`,
`:88`).

Work the levels for `n = 10,000,000` using the code's own rule
(`config.hpp:161-188`, `logBuckets`): the single-level threshold is
`16 · 2⁸ = 4,096`, the two-level threshold `4,096 · 2⁸ = 1,048,576`. Level 1:
`n = 10⁷` exceeds both, so it uses the maximum, `2⁸ = 256` buckets, each
about 39,062 elements. Level 2: `39,062` is between the thresholds, so the
buckets are split "evenly" over the remaining two levels:
`log₂(39,062 / 16) = log₂(2,441) = 11` (2¹¹ = 2,048 ≤ 2,441 < 4,096), so
`(11 + 1) / 2 = 6` → 64 buckets of about 610 elements. Level 3: `610 ≤ 4,096`,
`log₂(610 / 16) = log₂(38) = 5` → 32 buckets of about 19 elements, which the
base case finishes with insertion sort. The product 256 · 64 · 32 = 524,288
final buckets of about 19 elements. Three passes, against `log₂(10⁷ / 16) ≈
19.3` levels for a binary quicksort. The oversampling at level 1 is
`0.2 · log₂(10⁷) = 0.2 · 23 = 4.6`, which the code truncates to a step of 4, so
`4 · 256 − 1 = 1,023` samples are drawn and sorted (`config.hpp:169-172`,
`sampling.hpp:79-80`).

Why it matters: fewer passes is the point. A pass over 10 million 16-byte
rows moves 160 MB through the memory hierarchy; going from ~19 passes to ~3
is the structural advantage a k-way distribution has over a 2-way one, before
any branch tricks.

### Step 2 — classification without branches

> **In:** Step 1's splitters. **Out:** the comparison-tree trick, worked on 7
> splitters, and the line that implements it.

To find an element's bucket among 256 you do 8 comparisons (a binary search
over 255 splitters). Done with `if`, each comparison is a conditional branch whose outcome, for
random data, depends on the data; the paper calls these "inherently
hard-to-predict conditional branches" (§2), and a misprediction flushes the
CPU's pipeline. **Super scalar samplesort** (s³-sort, the
paper's starting point, §2–3) removes the branch: the splitters are stored as
an implicit **complete binary tree** in an array — node i has children 2i
and 2i+1 — and the next node index is *computed*, not chosen:

```cpp
// classifier.hpp, ips4o/ips4o/classifier.hpp at 08a5b92 — lines 100-112 (the line to read: 108).
 100      bucket_type classify(const value_type& value) const {
 ...
 106          bucket_type b = 1;
 107          for (int l = 0; l < log_buckets; ++l)
 108              b = 2 * b + comp_(splitter(b), value);
 109          if (kEqualBuckets)
 110              b = 2 * b + !comp_(value, sortedSplitter(b - num_buckets));
 111          return b - (kEqualBuckets ? 2 * num_buckets : num_buckets);
```

Line 108 is the whole trick: `comp_(splitter(b), value)` is 0 or 1 (a
boolean), and `2 * b + 0` or `2 * b + 1` is the left or right child. No branch,
so nothing to mispredict, and the compiler can interleave several elements'
traversals (the `kUnrollClassifier = 7` default, `config.hpp:92`).

Worked with 8 buckets and 7 splitters 10, 20, 30, 40, 50, 60, 70 stored as the
tree array `a₁ = 40, a₂ = 20, a₃ = 60, a₄ = 10, a₅ = 30, a₆ = 50, a₇ = 70`.
Classify `value = 35` with `comp(s, v) = (s < v)`: start `b = 1`. Level 0:
`a₁ = 40 < 35`? no (0) → `b = 2`. Level 1: `a₂ = 20 < 35`? yes (1) → `b = 5`.
Level 2: `a₅ = 30 < 35`? yes → `b = 11`. Return `11 − num_buckets = 11 − 8 = 3`:
bucket 3 is (30, 40], and 35 is in it. A value equal to a splitter goes left
(`s < v` is false), so 40 would also end in bucket 3.

Why it matters: the same idea — turn a decision into arithmetic — is what
`partition_lomuto_branchless_cyclic` does in Rust's ipnsort
([reading-rust-sort.md](reading-rust-sort.md) Step 3) and what a radix sort
does by its construction (the byte *is* the bucket index).

### Step 3 — in place: buffer blocks, a block permutation, a cleanup

> **In:** Step 2's classifier. **Out:** the three phases that move elements to
> their buckets using O(k · b) extra memory instead of O(n).

A classical samplesort distributes into a second array of size n. IPS⁴o's
contribution (§4) is to do it inside the input array. Elements are moved in
**blocks** of b ≈ 2 KiB (`config.hpp:64`; for 16-byte rows,
`2048 / 16 = 128` elements per block, rounded down to a power of two,
`config.hpp:288-292`). The partition has three phases (§4.1–4.3):

1. **Local classification (§4.1).** Each thread scans its stripe of the array
   and classifies every element with Step 2's tree, appending it to a small
   per-bucket **buffer block** (k buffers of b elements). When a buffer fills
   it is written back into the *already scanned* part of the stripe — "there
   is enough space to write b elements into the local stripe, since at least b
   more elements have been scanned from the stripe than have been written
   back". Afterwards the stripe is a sequence of full blocks, each containing
   elements of a single bucket, plus empty blocks, plus partial buffers.
2. **Block permutation (§4.2).** A prefix sum over the bucket counts gives
   each bucket's start in the array. For each bucket there is a read pointer
   and a write pointer; blocks are swapped into their correct bucket one block
   at a time using two swap buffers. Whole blocks move, not elements, so this
   phase is a sequence of 2 KiB `memcpy`s.
3. **Cleanup (§4.3).** Bucket boundaries do not coincide with block boundaries,
   so some elements are still misplaced at the heads and tails; the
   leftover buffers are flushed into the gaps.

Memory: Theorem 2 (§4.5) gives O(k · b · t + log_k(n/n₀)) additional space for
t threads; the first term, the buffers, dominates. For k = 256, b = 2 KiB,
t = 1 that is 256 × 2 KiB = 512 KiB — constant in n. §4.6 shows how to make
the recursion stack constant too.

The paper's accounting of why this beats the non-in-place s³-sort despite
writing the data twice (§4.5, Appendix B): for k = 256, 8-byte elements, one
recursion level, sequential IS⁴o needs "about 48n bytes of I/O volume,
whereas s³-sort needs (more than) 86n", the difference being memory
management, allocation misses and associativity misses.

Why it matters: in-place is not only about memory. The paper says the in-place
feature is "part of the speed advantage" (abstract of the journal version,
quoted in the repo README); a sort that doubles memory does so in the page
cache and in the cache hierarchy too, and a database sort at the scale of
this topic's lane 1 (10 million rows) already allocates scratch comparable to
the data (compare driftsort's 80 MB for 160 MB, [reading-rust-sort.md](reading-rust-sort.md) Step 4).

### Step 4 — many equal keys: equality buckets

> **In:** Steps 1–2. **Out:** what the sort does when the data has few
> distinct values, and a discrepancy between the paper and the code.

Samplesort has a classical weakness: if a key occurs more than n/k times, it
becomes a splitter and its bucket could end up huge, recursing again and
again on identical elements. §4.4 fixes this with **equality buckets**: after
the splitters are chosen, there is an extra bucket for each splitter that
holds the elements *equal* to it, selected by one more comparison without a
branch (`classifier.hpp:109-110`, the `kEqualBuckets` line). Equality buckets
are skipped during recursion, because all their elements are equal, and so
they are not a load-balancing problem.

The paper says (§4.7) the technique is "only used if there were duplicate
splitters". The code is stricter: after deduplicating the sampled splitters it
enables equality buckets only when the number of removed duplicates is at
least `kEqualBucketsThreshold`, 5 by default:

```cpp
// sampling.hpp, ips4o/ips4o/sampling.hpp at 08a5b92 — lines 94-106 (lines to read: 97, 104-106).
  94      for (int i = 2; i < num_buckets; ++i) {
  95          splitter += step;
  96          // Skip duplicates
  97          if (comp(*sorted_splitters, *splitter)) {
 ...
 103      // Check for duplicate splitters
 104      const auto diff_splitters = sorted_splitters - classifier.getSortedSplitters() + 1;
 105      const bool use_equal_buckets = Cfg::kAllowEqualBuckets
 106              && num_buckets - 1 - diff_splitters >= Cfg::kEqualBucketsThreshold;
```

Worked: with 256 buckets (255 splitters), if the sample has only 100 distinct
splitter values then `diff_splitters = 100` and `255 − 100 = 155 ≥ 5`, so
equality buckets are on; with 252 distinct (3 duplicates, `255 − 252 = 3 < 5`)
they stay off. The paper's text and the code threshold differ, and the
code is what runs.

Why it matters: this is the comparison-sort answer to the same input that
ipnsort handles with its equal-to-ancestor partition (reading-rust-sort.md
Step 3), and that makes a radix sort's buckets fat (README, lane 2's
256-distinct-keys row).

### Step 5 — what the paper measures, and where IPS⁴o loses

> **In:** Steps 1–4. **Out:** the numbers to carry, each with its section, and
> the honest limits.

All from §5 (machines Intel2S, Intel4S, AMD1S; 64-bit floating point elements
unless stated; each point the average of 15 runs for n < 2³⁰):

- **Sequential (IS⁴o), uniform input, n = 2³²:** 1.14× faster than
  BlockQuicksort on Intel2S, 1.22× on Intel4S, 1.57× on AMD1S;
  `std::sort` and dual-pivot quicksort are at least 1.86× slower; their
  branch mispredictions are about 10× higher.
- **Small inputs lose:** "Due to the initial overhead, IS⁴o is slower than
  BlockQ for n ≤ 2¹⁵." The sampling and the buffer setup are a fixed cost.
- **Presorted input loses:** "DualPivot outperforms IS⁴o on AlmostSorted input
  by a factor of 1.70 for n = 2³² (Intel2S)", and only on `Sorted` and `Ones`
  inputs is IPS⁴o beaten by TBB, which detects presorted input and returns.
  The paper's results imply IPS⁴o has no shortcut for presorted input
  (this chapter did not search the code for one); ipnsort and vergesort do
  ([reading-rust-sort.md](reading-rust-sort.md) Step 2).
- **Parallel, n = 2³² uniform:** 2.13× over the closest competitor on Intel2S
  and 1.75× on AMD1S; the abstract claims "up to 3" over its closest in-place
  competitor.
- **Few distinct keys help:** with `RootDup`, `EightDup`, `Exponential`
  IS⁴o is "even faster by a factor of up to two".

What the paper does not claim: that a comparison sort beats a radix sort. §1
sets radix sort aside — "integer sorting algorithms like radix sort ... are
less attractive, since these algorithms cannot be used in a reusable library
where they have to work for arbitrary data types" — and compares only
comparison-based algorithms. The database case this topic studies is exactly
the one where the data type *is* known and the key can be made bytes, so the
paper's reason for excluding radix sort does not apply.

### Step 6 — how it is used in this topic

> **In:** Steps 1–5. **Out:** what the one IPS⁴o measurement in the notes
> means, and what it does not.

`notes.md` records a single sequential run of `ips4o::sort` against
`std::sort` on 16-byte rows with uniform random `u64` keys. It is a C++
driver compiled by this chapter's author (not part of the crate), and the
random generator differs from the Rust crate's, so the *ratio* between the
two C++ sorts is the finding, not a comparison of C++ against the Rust
numbers. The parallel version needs TBB and a thread-count study that this
topic does not do.

## How to read the source

1. The paper's §3 (s³-sort and the tree layout), then §4.1–4.3 with Figures
   1–5 beside you; skip §4.5 and the appendices on a first read.
2. `include/ips4o/classifier.hpp:100-112` — one element's classification;
   `:139` onward for the unrolled batch version (`classifyUnrolled`).
3. `include/ips4o/sampling.hpp:72-122` — sample, deduplicate, choose
   equality buckets, fill to a power of two, build the tree.
4. `include/ips4o/config.hpp:55-92` (defaults) and `:161-188` (`logBuckets`).
5. `include/ips4o/local_classification.hpp`, `block_permutation.hpp`,
   `cleanup_margins.hpp` — one file per phase of Step 3.

## Questions (answer in notes.md)

1. With k = 256 and 16-byte elements, how many elements fit in a block, and
   how many bytes of buffer does one thread hold? What changes for 100-byte
   rows?
2. Trace `classify(35)` on the 7-splitter tree of Step 2 for the value 70 and
   for the value 5. Which buckets?
3. Sampling `αk − 1` elements costs a sort of that many. For n = 10⁷, how
   large is that sample relative to n, and why is it ignorable?
4. Using Step 5's numbers, give an input size and distribution at which you
   would *not* pick IPS⁴o over ipnsort.
5. IPS⁴o only needs `comp(a, b)`. For a `(i32, string-with-collation)` key,
   which of the two costs — the comparator's cost or the number of passes —
   does IPS⁴o reduce relative to ipnsort, and which does it not?

## Done when

Answer each before unfolding it.

- [ ] You can state how many passes a 256-way samplesort makes over 10 million elements and compute the bucket counts per level.

  <details><summary>Answer</summary>

  About three: the code's `logBuckets` gives 256 buckets at n = 10⁷ (above
  the 1,048,576 two-level threshold), 64 buckets at ~39,062 elements per
  bucket, and 32 buckets at ~610, for 256 × 64 × 32 = 524,288 final buckets
  of ~19 elements finished by a base case of at most 16 or so elements
  (`config.hpp:161-188`). A binary quicksort needs about log₂(10⁷/16) ≈ 19.3
  levels.

  </details>

- [ ] You can explain how classification avoids branches and trace one element.

  <details><summary>Answer</summary>

  The splitters sit in an implicit binary tree array; the next index is
  `b = 2*b + comp(splitter(b), value)` (`classifier.hpp:108`), where the
  comparison result is 0 or 1 and is added, not branched on. With splitters
  10…70 stored as `[40, 20, 60, 10, 30, 50, 70]`, the value 35 goes
  `b = 1 → 2 → 5 → 11`, and `11 − 8 = 3`: bucket 3, the range (30, 40].

  </details>

- [ ] You can name the three phases of the in-place partition and what bounds the extra memory.

  <details><summary>Answer</summary>

  Local classification into k buffer blocks that are flushed back into the
  already-scanned part of the array; block permutation, which moves whole
  blocks into their bucket's range using two swap buffers; cleanup of the
  bucket heads and tails. Extra memory is O(k·b·t) for the buffers plus a
  logarithmic recursion stack (Theorem 2, §4.5) — about 256 × 2 KiB = 512
  KiB per thread, independent of n.

  </details>

- [ ] You can say what equality buckets are for, and how the code's rule differs from the paper's.

  <details><summary>Answer</summary>

  A key occurring more than n/k times becomes a splitter and, without a
  special bucket, could recurse repeatedly on equal elements; an equality
  bucket per splitter holds elements equal to it and is skipped in
  recursion (§4.4). The paper enables them if there were duplicate splitters
  (§4.7); the code enables them when at least `kEqualBucketsThreshold` = 5
  splitters were removed as duplicates (`sampling.hpp:104-106`,
  `config.hpp:76`).

  </details>

- [ ] You can list the inputs on which the paper reports IPS⁴o losing.

  <details><summary>Answer</summary>

  Small n: slower than BlockQuicksort for n ≤ 2¹⁵ because of initial
  overhead (§5). Almost-sorted input: dual-pivot quicksort is 1.70× faster at
  n = 2³² (Intel2S). Sorted and all-equal inputs: TBB's sort, which detects
  presorted input and returns immediately, beats it (§5, parallel).

  </details>

## References

- Michael Axtmann, Sascha Witt, Daniel Ferizovic, Peter Sanders,
  **"In-place Parallel Super Scalar Samplesort (IPS⁴o)"**, ESA 2017,
  arXiv:1705.02257v2 — §1 (why not radix), §3 (s³-sort), §4.1–4.7, §5
  (Figures 6–8), Appendix B (I/O volume).
- `ips4o/ips4o` at `08a5b92` (2025-01-15): `include/ips4o/{classifier,sampling,config}.hpp`.
- Previous: [reading-rust-sort.md](reading-rust-sort.md). Next:
  [reading-unicode-collation.md](reading-unicode-collation.md).
