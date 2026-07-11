# Reading guide — Roaring bitmap internals (SPE'18 + roaring-rs)

**Sources:**
- Lemire et al. — "Roaring Bitmaps: Implementation of an Optimized Software
  Library" (Software: Practice & Experience 2018) — read §2 (containers),
  §3 (SIMD kernels), skim benchmarks
- `~/repos/roaring-rs/roaring/src/bitmap/` — the Rust port
- Extends topic 23's guide (`topics/23-search/reading-postings.md`) and
  stub (`postings.rs` — array/bitmap containers already built there); this
  guide adds the third container, the density algebra, and the SIMD story

## 1. Recap + the missing third container

A roaring bitmap chops the u32 space into 64K chunks by the high 16 bits;
each chunk stores its low-16-bit members in whichever container is
smallest:

| container | roaring-rs type | when | size |
|---|---|---|---|
| array | `ArrayStore` (sorted `Vec<u16>`) | card ≤ 4096 | 2 bytes/element |
| bitmap | `BitmapStore` (1024 × u64) | card > 4096 | 8 KB flat |
| run | `IntervalStore` (sorted (start, end) pairs) | few runs | 4 bytes/run |

Anchors: `store/mod.rs:28-31` (`enum Store { Array, Bitmap, Run }`),
`container.rs:9-11` (`ARRAY_LIMIT = 4096`, `RUN_MAX_SIZE = 2048`),
`container.rs:70` (`ensure_correct_store` — every mutation may demote/promote).

The thresholds are pure arithmetic, not tuning:
- 4096 × 2 bytes = 8 KB = the bitmap's fixed cost → array wins below, bitmap above.
- A run container beats the bitmap iff runs × 4 bytes < 8 KB → `RUN_MAX_SIZE = 2048`.

**Q1.** Topic 20's GraphBLAS switches sparse↔bitmap per *matrix*; roaring
switches per *64K chunk*. Same density crossover, different granularity.
What workload makes per-chunk adaptivity decisively better? (Hint: a graph
with one dense community and a long sparse tail of node IDs.)

## 2. The density algebra — ops pick kernels pairwise

Every binary op dispatches on the container *pair* — 3×3 kernels, each the
natural algorithm for that shape (`store/mod.rs:207-224` shows the
is_disjoint/is_subset matrix; the BitAnd/BitOr impls follow the same
pattern):

```
             ∩ array              ∩ bitmap            ∩ run
  array      merge or GALLOP      probe bits per elem  probe intervals
  bitmap     (symmetric)          1024 x (a & b)       mask interval spans
  run        (symmetric)          (symmetric)          interval intersection
```

The galloping case is the one topic 23 met as skip-lists/WAND: when
|A| ≪ |B|, walk A and *exponentially search* B — O(|A|·log|B|) beats the
linear merge. Same asymmetry-exploiting move as ALEX's exponential search
([reading-learned-indexes.md](reading-learned-indexes.md)) and topic 23's
galloping in `MAXSCORE`.

**Q2.** Union of two arrays can overflow ARRAY_LIMIT. `container.rs:106`
checks `union_cardinality <= ARRAY_LIMIT` *before* choosing the output
container. Why is computing the exact union cardinality first cheaper than
"build array, promote if too big"?

## 3. The SIMD story (paper §3, `store/array_store/vector.rs`)

`array_store/` splits into `scalar.rs` and `vector.rs` — the same kernels
twice, and the module picks at compile time. The paper's two famous kernels:

- **Array ∩ array**: compare a block of A against a block of B with a
  shuffle network; SPE'18 §3.2's `_mm_cmpistrm`-style or the simpler
  broadcast-compare. `vector.rs` uses portable `std::simd` — read its
  intersect and note the *tail fallback to scalar*.
- **Bitmap card**: population count over 1024 words; the paper's Harley-Seal
  AVX2 popcount is why `intersection_len` (`array_store/mod.rs:258`) style
  cardinality-only ops never materialize a result container.

**Q3.** Cardinality-only ops (`intersection_len`, `is_disjoint`) are the
hot path in query *planning* (estimate selectivity before executing —
topic 9). Why does roaring make these zero-allocation while full ops
allocate?

## 4. Run containers and sortedness

`Run` shines exactly when data arrives clustered: sequential IDs, time
ranges, "all rows in partition." `insert_range` (`store/mod.rs:107-109`)
into a Run is O(runs); into a Bitmap it's word-fill; into an Array it's a
splice. This is why roaring formats have an explicit `optimize()`/run
conversion pass after bulk load rather than checking on every insert.

**Q4 (cross-topic thread).** Three adaptive encodings, one idea:

| | roaring | redis HLL sparse | postgres GIN posting |
|---|---|---|---|
| unit | 64K chunk | register stream | TID list segment |
| encodings | array/bitmap/run | ZERO/XZERO/VAL | varbyte deltas |
| promote when | card > 4096 | bytes > 3 KB or rank > 32 | page overflow → posting tree |

Fill in the *demotion* column yourself: which of the three ever converts
back down, and why is demotion rarer than promotion everywhere?

## 5. Tie back to the stubs

Topic 23's `postings.rs` stub already fixes array↔bitmap promotion at 4096.
After this guide: (a) add the galloping intersect to your mental model of
why FalkorDB label filters should be roaring, not Vec<u64>; (b) M26's plan
(roaring for label/type filtering) inherits the run container for
"all nodes created in bulk-load order" — measure whether your ID allocator
produces runs.
