# Roaring bitmaps: adaptive containers for integer sets

The workhorse of every "set of row/node IDs" problem: chop the u32
space into 64K chunks and store each chunk in whichever of three
encodings is smallest for its density. This chapter extends topic 23's
guide (`topics/23-search/reading-postings.md`) and its `postings.rs`
stub — array/bitmap containers exist there already; here we build the
full machine step by step — the density crossover, the chunking, the run
container, the pairwise kernels, the SIMD story — following the
roaring-rs port.

## The problem in one sentence

Store and intersect sets of u32 IDs: a sorted `Vec<u32>` costs 4 bytes
per element and slow intersections, a flat bitmap over the whole u32
space costs **512 MB no matter how few elements it holds** — and no
single encoding wins, because density varies wildly across the key space
of any real ID set.

## The concepts, step by step

### Step 1 — two encodings, one crossover: density decides

For a set of small integers there are two natural representations: a
**sorted array** of the values (cost proportional to how many you store)
and a **bitmap** (one bit per *possible* value — fixed cost, regardless
of how many are present). Over a 16-bit universe (65,536 possible
values), the arithmetic is exact: an array of 16-bit entries costs
2 bytes/element; the bitmap costs a flat 65,536 bits = **8 KB**. They
cross at 8 KB / 2 B = **4,096 elements** — below that the array is
smaller (and the bitmap mostly zeros); above it the bitmap is smaller
(and gives O(1) membership and word-at-a-time set operations for free).
No threshold tuning, pure arithmetic — the same density crossover
GraphBLAS meets at whole-matrix granularity (topic 20).

### Step 2 — chunking: apply the crossover per 64K range

Roaring makes the crossover *local*: split the u32 space by the high 16
bits into up to 65,536 chunks, and give each chunk its own **container**
holding the members' low 16 bits in whichever encoding is smallest *for
that chunk's density*:

| container | roaring-rs type | when | size |
|---|---|---|---|
| array | `ArrayStore` (sorted `Vec<u16>`) | card ≤ 4096 | 2 bytes/element |
| bitmap | `BitmapStore` (1024 × u64) | card > 4096 | 8 KB flat |
| run | `IntervalStore` (sorted (start, end) pairs) | few runs | 4 bytes/run |

Anchors: `store/mod.rs:28-31` (`enum Store { Array, Bitmap, Run }`),
`container.rs:9-11` (`ARRAY_LIMIT = 4096`, `RUN_MAX_SIZE = 2048`),
`container.rs:70` (`ensure_correct_store` — every mutation may
demote/promote). The payoff: a graph with one dense community (bitmap
containers) and a long sparse tail of node IDs (array containers) pays
the right price in *each region* — empty chunks cost nothing at all.

### Step 3 — the third container: runs, for clustered data

A **run container** stores maximal intervals as (start, length) pairs —
4 bytes per run — and wins when the data arrives *clustered*: sequential
IDs, time ranges, "all rows in partition". The threshold is the same
arithmetic as Step 1: a run container beats the 8 KB bitmap iff
runs × 4 bytes < 8 KB → `RUN_MAX_SIZE = 2048`. A chunk holding one run
of 60,000 consecutive IDs costs 4 bytes instead of 8 KB. The operational
wrinkle: checking run-worthiness on every insert would be wasteful, so
roaring formats have an explicit `optimize()`/run-conversion pass after
bulk load instead — `insert_range` (`store/mod.rs:107-109`) into a Run
is O(runs); into a Bitmap it's word-fill; into an Array it's a splice.

### Step 4 — the density algebra: ops pick kernels pairwise

Every binary set operation dispatches on the container *pair* — 3×3
kernels, each the natural algorithm for that shape (`store/mod.rs:207-224`
shows the is_disjoint/is_subset matrix; the BitAnd/BitOr impls follow the
same pattern):

```
             ∩ array              ∩ bitmap            ∩ run
  array      merge or GALLOP      probe bits per elem  probe intervals
  bitmap     (symmetric)          1024 x (a & b)       mask interval spans
  run        (symmetric)          (symmetric)          interval intersection
```

The galloping case is the one topic 23 met as skip-lists/WAND:
**galloping** (exponential search — probe at strides 1, 2, 4, 8... then
binary-search the bracketed range) exploits size asymmetry: when
|A| ≪ |B|, walk A and gallop through B — O(|A|·log|B|) beats the linear
merge. Same asymmetry-exploiting move as ALEX's exponential search
([reading-learned-indexes.md](reading-learned-indexes.md)) and topic 23's
galloping in `MAXSCORE`.

```rust
fn intersect_gallop(small: &[u16], big: &[u16], out: &mut Vec<u16>) {
    let mut lo = 0;
    for &x in small {                             // |small| ≪ |big|
        let mut step = 1;                         // gallop: 1, 2, 4, 8, ...
        while lo + step < big.len() && big[lo + step] < x { step <<= 1; }
        let hi = (lo + step + 1).min(big.len());
        match big[lo..hi].binary_search(&x) {     // then binary in the bracket
            Ok(i)  => { out.push(x); lo += i + 1; }
            Err(i) => { lo += i; }
        }
    }                                             // O(|small| · log|big|)
}
```

One subtlety worth noticing: union of two arrays can overflow
ARRAY_LIMIT, so `container.rs:106` checks
`union_cardinality <= ARRAY_LIMIT` *before* choosing the output
container — question 2 asks why counting first beats build-then-promote.

### Step 5 — the SIMD story: same kernels, vector width

`array_store/` splits into `scalar.rs` and `vector.rs` — the same
kernels twice, and the module picks at compile time (paper §3). The
paper's two famous kernels:

- **Array ∩ array**: compare a block of A against a block of B with a
  shuffle network; SPE'18 §3.2's `_mm_cmpistrm`-style or the simpler
  broadcast-compare. `vector.rs` uses portable `std::simd` — read its
  intersect and note the *tail fallback to scalar*.
- **Bitmap card**: population count over 1024 words; the paper's Harley-Seal
  AVX2 popcount is why `intersection_len` (`array_store/mod.rs:258`) style
  cardinality-only ops never materialize a result container.

Cardinality-only ops (`intersection_len`, `is_disjoint`) are
zero-allocation on purpose — they're the hot path in query *planning*
(estimate selectivity before executing, topic 9), where allocating a
result you'll throw away would dominate the cost.

### Step 6 — one idea, three systems: adaptive encodings everywhere

Roaring's promote-on-density-threshold move is not a bitmap trick — it's
a recurring systems pattern:

| | roaring | redis HLL sparse | postgres GIN posting |
|---|---|---|---|
| unit | 64K chunk | register stream | TID list segment |
| encodings | array/bitmap/run | ZERO/XZERO/VAL | varbyte deltas |
| promote when | card > 4096 | bytes > 3 KB or rank > 32 | page overflow → posting tree |

Fill in the *demotion* column yourself: which of the three ever converts
back down, and why is demotion rarer than promotion everywhere? Topic
20's GraphBLAS sparse↔bitmap switch is the same crossover at per-matrix
granularity — the same density arithmetic, measured twice.

## Where each step lives in the code

[roaring-rs](https://github.com/RoaringBitmap/roaring-rs)
`roaring/src/bitmap/` — the Rust port; `store/` holds the three
containers and the pairwise kernels.

| anchor | step | what it is |
|---|---|---|
| `store/mod.rs:28-31` | 2 | `enum Store { Array, Bitmap, Run }` |
| `container.rs:9-11` | 2–3 | `ARRAY_LIMIT = 4096`, `RUN_MAX_SIZE = 2048` — the two crossovers |
| `container.rs:70` | 2 | `ensure_correct_store` — every mutation may demote/promote |
| `container.rs:106` | 4 | union cardinality checked *before* choosing the output container |
| `store/mod.rs:107-109` | 3 | `insert_range` per container: O(runs) / word-fill / splice |
| `store/mod.rs:207-224` | 4 | the pairwise dispatch matrix (is_disjoint/is_subset) |
| `store/array_store/scalar.rs` + `vector.rs` | 5 | the same kernels twice; `std::simd` with scalar tail fallback |
| `array_store/mod.rs:258` | 5 | `intersection_len` — cardinality-only, zero-allocation |

## Tie back to the stubs

Topic 23's `postings.rs` stub already fixes array↔bitmap promotion at 4096.
After this guide: (a) add the galloping intersect to your mental model of
why FalkorDB label filters should be roaring, not `Vec<u64>`; (b) M26's plan
(roaring for label/type filtering) inherits the run container for
"all nodes created in bulk-load order" — measure whether your ID allocator
produces runs.

## Questions to answer in notes.md

1. Topic 20's GraphBLAS switches sparse↔bitmap per *matrix*; roaring
   switches per *64K chunk*. Same density crossover, different
   granularity. What workload makes per-chunk adaptivity decisively
   better? (Hint: a graph with one dense community and a long sparse
   tail of node IDs.)
2. Union of two arrays can overflow ARRAY_LIMIT. `container.rs:106`
   checks `union_cardinality <= ARRAY_LIMIT` *before* choosing the output
   container. Why is computing the exact union cardinality first cheaper
   than "build array, promote if too big"?
3. Cardinality-only ops (`intersection_len`, `is_disjoint`) are the hot
   path in query *planning* (estimate selectivity before executing —
   topic 9). Why does roaring make these zero-allocation while full ops
   allocate?
4. **(cross-topic thread)** Three adaptive encodings, one idea — the
   table in Step 6. Fill in the *demotion* column: which of the three
   ever converts back down, and why is demotion rarer than promotion
   everywhere?

## References

**Papers**
- Lemire et al. — "Roaring Bitmaps: Implementation of an Optimized
  Software Library" (Software: Practice & Experience 2018,
  [arXiv:1709.07821](https://arxiv.org/abs/1709.07821)) — §2
  containers, §3 SIMD kernels, skim benchmarks

**Code**
- [roaring-rs](https://github.com/RoaringBitmap/roaring-rs)
  `roaring/src/bitmap/` — the Rust port; `store/` holds the three
  containers and the pairwise kernels
