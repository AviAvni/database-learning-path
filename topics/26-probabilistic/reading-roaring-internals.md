# Roaring bitmaps: adaptive containers for integer sets

The workhorse of every "set of row/node IDs" problem: chop the u32
space into 64K chunks and store each chunk in whichever of three
encodings is smallest for its density. This chapter extends topic 23's
Roaring guide (`topics/23-fulltext/reading-roaring.md`) and its
`topics/23-fulltext/experiments/src/postings.rs` stub — array/bitmap
containers exist there already; here we build the full machine step by
step — the density crossover, the chunking, the run container, the
pairwise kernels, the SIMD story — reading the production code.

Every code anchor below is **roaring-rs** — the Rust port
`RoaringBitmap/roaring-rs` at commit `83caaca`, *not* the C library
CRoaring — quoted with the line numbers the code occupies in that
revision (all paths sit under `roaring/src/bitmap/`). Paper figures come
from Chambi, Lemire, Ssi-Yan-Kai & Kaser, "Roaring Bitmaps:
Implementation of an Optimized Software Library" (Software: Practice &
Experience 2018, [arXiv:1709.07821](https://arxiv.org/abs/1709.07821)),
cited by section or page. Where roaring-rs diverges from the paper or
CRoaring — it does in three places below — this guide says so rather than
describing a technique the source does not use.

## The problem in one sentence

Store and intersect sets of u32 IDs cheaply: a sorted `Vec<u32>` costs 4
bytes/element and answers membership by binary search — the topic
headline's **246 ns** point miss ([FINDINGS.md](../../FINDINGS.md) row
26) — while a flat bitmap over the whole u32 space answers in O(1) like
that row's **28 ns** 224 MB HashSet but costs **512 MB no matter how few
elements it holds** (2^32 bits ÷ 8 = 536,870,912 bytes = 512 MiB); no
single encoding wins, because density varies wildly across the key space
of any real ID set, so Roaring keeps both and picks per 64K chunk.

## The concepts, step by step

### Step 1 — two encodings, one crossover: density decides

> **In:** nothing yet — this step fixes the density arithmetic every
> later step reuses.
> **Out:** the array↔bitmap crossover at **4,096 elements**, derived from
> the 8 KB bitmap size; Step 2 applies it per chunk and Step 3 reuses the
> same "which encoding is smaller" test for runs.

A **container** here is the storage for one 16-bit key range, and it has
two natural encodings. A **sorted array** stores the present values
themselves, so its cost is proportional to how many you store; a
**bitmap** stores one bit per *possible* value, a fixed cost regardless
of how many are present. Over a 16-bit universe (2^16 = 65,536 possible
values) the arithmetic is exact and worth doing once:

- The bitmap is 2^16 bits = **65,536 bits = 8,192 bytes = 8 KB**, laid
  out as **1024 × 64-bit words** (65,536 ÷ 64 = 1024). That is exactly
  `BITMAP_LENGTH = 1024` in roaring-rs (`store/bitmap_store.rs:15`), and
  the paper's "1024 64-bit words (using 8 kB)" (§ containers, p. 5).
- An array entry is a `u16` = **2 bytes**.
- They cross where the array grows to the bitmap's size:
  8,192 bytes ÷ 2 bytes/element = **4,096 elements**. That is exactly
  `ARRAY_LIMIT = 4096` (`container.rs:9`).

Below 4,096 the array is smaller (and a bitmap would be mostly zeros); at
or above it the bitmap is smaller — and it also buys **O(1) membership**
and word-at-a-time set algebra, the 28 ns HashSet side of the topic
headline, where the array is the 246 ns binary-search side
([FINDINGS.md](../../FINDINGS.md) row 26). No threshold tuning, pure
arithmetic — the same density crossover GraphBLAS meets at whole-matrix
granularity (topic 20).

### Step 2 — chunking: apply the crossover per 64K range

> **In:** the array↔bitmap crossover from Step 1.
> **Out:** the per-chunk container assignment — up to 65,536 chunks, each
> an `Array`, `Bitmap` or `Run` store (`store/mod.rs:28-31`) — that Steps
> 3-4's kernels dispatch on.

Roaring makes the crossover *local*: split the u32 space by the high 16
bits into up to 65,536 chunks, and give each chunk its own **container**
holding the members' low 16 bits in whichever encoding is smallest *for
that chunk's density* (paper § containers, p. 2):

| container | roaring-rs type | when | size |
|---|---|---|---|
| array | `ArrayStore` (sorted `Vec<u16>`) | card ≤ 4096 | 2 bytes/element |
| bitmap | `BitmapStore` (1024 × `u64`) | card > 4096 | 8 KB flat |
| run | `IntervalStore` (sorted `{start, end}` pairs) | few runs | 4 bytes/run |

The `Store` enum is three variants (`store/mod.rs:28-31`). The threshold
lives in `ARRAY_LIMIT = 4096` (`container.rs:9`). Promotion and demotion
are re-checked on every mutation by `ensure_correct_store` (defined at
`container.rs:225`, called from `insert` at `:70`): its two arms promote
`Array → Bitmap` when `vec.len() > ARRAY_LIMIT` (`:230-231`) and demote
`Bitmap → Array` when `bits.len() <= ARRAY_LIMIT` (`:227-228`). The
payoff: a graph with one dense community (bitmap containers) and a long
sparse tail of node IDs (array containers) pays the right price in *each
region* — and empty chunks cost nothing at all.

### Step 3 — the third container: runs, for clustered data

> **In:** the array/bitmap chunks from Step 2.
> **Out:** the third encoding — the **run container** — and the
> serialized-size rule (not a fixed threshold) that `optimize()` uses to
> choose it, which Step 4's kernels dispatch on as a third shape.

A **run container** stores maximal runs of consecutive values and wins
when the data arrives *clustered*: sequential IDs, time ranges, "all rows
in a partition". Here roaring-rs **diverges from the paper**: the paper
stores each run as a `(start, length)` pair (§ containers, p. 6), but
roaring-rs's `IntervalStore` stores `Interval { start: u16, end: u16 }` —
an inclusive `{start, end}` pair (`store/interval_store.rs:900-902`).
Either way a run is **4 bytes** (two `u16`s, `RUN_ELEMENT_BYTES = 4` at
`store/interval_store.rs:14`), with a 2-byte run-count header
(`serialized_byte_size = 2 + 4 × runs`, `:39-40`).

The break-even against the bitmap is the same kind of arithmetic as
Step 1: a run container beats the 8 KB bitmap while its serialized bytes
stay under 8,192 — `2 + 4 × runs < 8192`, i.e. **runs ≤ 2047** (solving
`runs < 2047.5`). A chunk holding one run of 60,000 consecutive IDs is a
single `Interval` — **4 bytes instead of 8 KB**, a 2048× saving.

Two corrections to the folklore here. First, `RUN_MAX_SIZE = 2048`
(`container.rs:11`) — the round number `8192 ÷ 4` — is **`#[cfg(test)]`
only** (the attribute sits on `:10`); it does *not* drive production
conversion. Second, the real decision lives in `optimize()`
(`container.rs:243`), which compares *actual serialized sizes*:
`BITMAP_BYTES` (8192) against `IntervalStore::serialized_byte_size(num_runs)`
for a bitmap (`:246-251`), and `array.byte_size()` against the run size
for an array (`:254-262`). Checking run-worthiness on every insert would
be wasteful, so this is an explicit post-bulk-load pass, not a per-insert
test — `insert_range` itself (`store/mod.rs:107-109`) just dispatches:
into a `Run` it is O(runs), into a `Bitmap` word-fill, into an `Array` a
splice.

### Step 4 — the density algebra: ops pick kernels pairwise

> **In:** the three container shapes from Steps 2-3.
> **Out:** the 3×3 kernel dispatch (`is_disjoint` at `store/mod.rs:200`,
> `is_subset` at `:215`), the *linear-merge* array∩array kernel roaring-rs
> actually uses, and the `insert_range` promotion check
> (`container.rs:102-110`) that Step 5 vectorizes.

Every binary set operation dispatches on the container *pair* — a 3×3
grid of kernels, each the natural algorithm for that shape.
`store/mod.rs:200-213` (`is_disjoint`) and `:215-227` (`is_subset`) show
the full matrix; the `BitAnd`/`BitOr` impls follow the same pattern:

```
             ∩ array              ∩ bitmap            ∩ run
  array      linear merge         probe each u16       probe intervals
  bitmap     (symmetric)          1024 × (a & b)       mask interval spans
  run        (symmetric)          (symmetric)          interval intersection
```

Here roaring-rs **diverges from CRoaring**, and it is worth being honest
about. CRoaring's array∩array has a *galloping* variant (**galloping** =
exponential search — probe at strides 1, 2, 4, 8… then binary-search the
bracketed range, which beats a linear scan when one side is far smaller).
roaring-rs does **not** gallop: its scalar array∩array is a flat
two-pointer merge that advances one index at a time:

```rust
// store/array_store/scalar.rs — and(), the array∩array kernel, 37-54
   37  pub fn and(lhs: &[u16], rhs: &[u16], visitor: &mut impl BinaryOperationVisitor) {
   38      // Traverse both arrays
   39      let mut i = 0;
   40      let mut j = 0;
   41      while i < lhs.len() && j < rhs.len() {
   42          let a = unsafe { lhs.get_unchecked(i) };
   43          let b = unsafe { rhs.get_unchecked(j) };
   44          match a.cmp(b) {
   45              Less => i += 1,
   46              Greater => j += 1,
   47              Equal => {
   48                  visitor.visit_scalar(*a);
   49                  i += 1;
   50                  j += 1;
   51              }
   52          }
   53      }
   54  }
```

Line 45 is the one to watch: on `Less` it steps `i` by **one**, not by a
gallop stride. The size-asymmetry win galloping chases still exists in
roaring-rs, but it comes from a *different* kernel — the array-vs-bitmap
case (`store/mod.rs:204-206`), which iterates the small array and does an
**O(1) bit-test per element** into the big bitmap, so the cost is
O(|array|) regardless of how dense the bitmap is (the paper's O(|B₁|)
intersection argument, p. 2). That is the honest mapping of topic 23's
skip-list/WAND galloping onto this code: the same "walk the small side"
idea, realised by container choice rather than by exponential search.

One subtlety in the *insert* path feeds question 2. Adding a range to an
array chunk can overflow `ARRAY_LIMIT`, so `insert_range`'s array arm
counts the union first: it computes `union_cardinality = array.len() +
added_amount` (`container.rs:102`) and only *then* branches — `== 1<<16`
becomes a full-range `Run` (`:103-104`), `<= ARRAY_LIMIT` stays an array
(`:106-107`), otherwise it promotes to a bitmap before inserting
(`:108-110`). Counting first beats build-then-promote because it never
materialises an over-limit array it would immediately throw away.

### Step 5 — the SIMD story: same kernels, vector width

> **In:** the pairwise kernels from Step 4.
> **Out:** how roaring-rs vectorizes them — `core::simd` in `vector.rs`
> with a scalar tail fallback — and the two spots where its kernels are
> *not* the paper's.

`array_store/` splits into `scalar.rs` and `vector.rs` (`mod scalar; mod
vector;` at `array_store/mod.rs:1-2`) — the same kernels twice, picked at
compile time. `vector.rs` is gated `#![cfg(feature = "simd")]` (`:11`)
and built on portable `core::simd` (`:14-15`) with 8-wide `u16x8` lanes;
its `and` is at `:119`. Two honest divergences from the paper's kernels:

- **Array ∩ array**: the paper (and CRoaring) use the x86 `PCMPESTRM`
  string-compare instruction. roaring-rs's own header says it "replaced
  [PCMPESTRM] with a simple vector or-shift … what is available through
  LLVM intrinsics and is portable" (`vector.rs:6-9`). Read `and`
  (`:119`) and note the **tail fallback to `scalar`** for the leftover
  elements.
- **Bitmap cardinality**: the paper describes a vectorized Harley-Seal
  popcount; roaring-rs does **not** ship one — it sums
  `u64::count_ones()` per word (`store/bitmap_store.rs:34`, `:143`),
  leaning on the hardware `popcnt` intrinsic the compiler emits. That
  per-word popcount is why `intersection_len` (`array_store/mod.rs:258`)
  can count matches without materializing a result: it drives a
  `CardinalityCounter` visitor (`:259-264`) through `vector::and` /
  `scalar::and` and keeps only the tally.

Cardinality-only ops (`intersection_len`, `is_disjoint`) are
zero-allocation on purpose — they are the hot path in query *planning*
(estimate selectivity before executing, topic 9), where allocating a
result you will throw away would dominate the cost.

### Step 6 — one idea, three systems: adaptive encodings everywhere

> **In:** roaring's promote-on-density move from Steps 1-3.
> **Out:** the same adaptive-encoding pattern in two other systems, and
> the *demotion* question you answer in notes.md.

Roaring's promote-on-density-threshold move is not a bitmap trick — it is
a recurring systems pattern:

| | roaring | redis HLL sparse | postgres GIN posting |
|---|---|---|---|
| unit | 64K chunk | register run | TID list segment |
| encodings | array/bitmap/run | ZERO/XZERO/VAL opcodes | varbyte deltas |
| promote when | card > 4096 (`container.rs:9`, `:230`) | rank > 32 or bytes > max | page overflow → posting tree |

The redis numbers are checkable too: HLL's sparse encoding promotes to
dense when a register value exceeds `HLL_SPARSE_VAL_MAX_VALUE = 32`
(`redis src/hyperloglog.c:389`; `if (count > HLL_SPARSE_VAL_MAX_VALUE)
goto promote;` at `:683`) or when the sparse blob would exceed
`server.hll_sparse_max_bytes` (default 3000 ≈ 3 KB; `:863`). Fill in the
*demotion* column yourself: which of the three ever converts back down,
and why is demotion rarer than promotion everywhere? (Roaring's own
`Bitmap → Array` demotion at `container.rs:227-228` is the exception that
makes the rule interesting.) Topic 20's GraphBLAS sparse↔bitmap switch is
the same crossover at per-matrix granularity — the same density
arithmetic, measured twice.

## Where each step lives in the code

[roaring-rs](https://github.com/RoaringBitmap/roaring-rs)
`roaring/src/bitmap/` — the Rust port; `store/` holds the three
containers and the pairwise kernels.

| anchor | step | what it is |
|---|---|---|
| `store/mod.rs:28-31` | 2 | `enum Store { Array, Bitmap, Run }` |
| `container.rs:9` | 1-2 | `ARRAY_LIMIT = 4096` — the array↔bitmap crossover |
| `store/bitmap_store.rs:15` | 1 | `BITMAP_LENGTH = 1024` — the 1024×`u64` = 8 KB bitmap |
| `container.rs:10-11` | 3 | `RUN_MAX_SIZE = 2048` — **`#[cfg(test)]` only**, not the production rule |
| `container.rs:225` | 2 | `ensure_correct_store` — promote `Array→Bitmap` (`:230`), demote `Bitmap→Array` (`:227`) |
| `container.rs:243` | 3 | `optimize` — run conversion by comparing serialized byte sizes |
| `container.rs:102-110` | 4 | `insert_range` array arm: count `union_cardinality` *before* choosing the output container |
| `store/mod.rs:107-109` | 3 | `insert_range` per container: O(runs) / word-fill / splice |
| `store/mod.rs:200-227` | 4 | the pairwise dispatch matrix (`is_disjoint` `:200`, `is_subset` `:215`) |
| `store/array_store/scalar.rs:37` | 4 | array∩array = flat two-pointer merge (no galloping) |
| `store/array_store/vector.rs:11` | 5 | `#![cfg(feature="simd")]` `core::simd` port; `and` at `:119`, scalar tail fallback |
| `array_store/mod.rs:258` | 5 | `intersection_len` — cardinality-only, zero-allocation |

## Tie back to the stubs

Topic 23's `postings.rs` stub
(`topics/23-fulltext/experiments/src/postings.rs`) already fixes
array↔bitmap promotion at `ARRAY_MAX = 4096` and uses the same
two-pointer array∩array kernel this guide found in `scalar.rs:37`. After
this guide: (a) hold onto why FalkorDB label filters should be roaring,
not `Vec<u64>` — the array-vs-bitmap probe kernel is O(|small side|), not
O(|filter|); (b) M26's plan (roaring for label/type filtering) inherits
the run container for "all nodes created in bulk-load order" — measure
whether your ID allocator actually produces runs before assuming it does.

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

## Done when

Answer each before unfolding it.

- [ ] You can state the density crossover and why it is applied per 64K chunk.

  <details><summary>Answer</summary>

  Over a 16-bit key range the bitmap is a fixed 2^16 bits = 8,192 bytes =
  8 KB (`BITMAP_LENGTH = 1024` × `u64`, `store/bitmap_store.rs:15`), and a
  sorted array costs 2 bytes per `u16`. They cross at 8,192 ÷ 2 = **4,096
  elements** — exactly `ARRAY_LIMIT = 4096` (`container.rs:9`): below it
  the array is smaller, at/above it the bitmap is smaller and O(1) to
  probe.

  Roaring applies this *per chunk* — splitting the u32 space by the high
  16 bits into up to 65,536 containers (`Store::{Array,Bitmap,Run}`,
  `store/mod.rs:28-31`), each choosing its own encoding via
  `ensure_correct_store` (`container.rs:225`, promote at `:230`, demote at
  `:227`). Per-chunk adaptivity wins because density varies across the key
  space: one dense community lands in a bitmap while a sparse tail stays
  in arrays, and empty chunks cost nothing — a single global bitmap would
  pay 512 MB regardless.

  </details>

- [ ] You can explain what run containers add and which data shape wants them.

  <details><summary>Answer</summary>

  A run container stores maximal runs of consecutive values. In roaring-rs
  each run is an `Interval { start: u16, end: u16 }` (an inclusive
  `{start, end}` pair, `store/interval_store.rs:900-902`) at 4 bytes —
  *diverging from the paper's `(start, length)` encoding*. It wins on
  *clustered* data: sequential IDs, time ranges, bulk-loaded partitions. A
  single run of 60,000 consecutive IDs is one 4-byte `Interval` instead of
  an 8 KB bitmap — a 2048× saving.

  The break-even is `2 + 4 × runs < 8192`, i.e. **runs ≤ 2047**. The
  `RUN_MAX_SIZE = 2048` constant (`container.rs:11`) is only its round
  approximation, and it is `#[cfg(test)]`-only (`:10`); production instead
  picks runs in `optimize()` (`container.rs:243`) by comparing actual
  serialized byte sizes — `BITMAP_BYTES` (8192) against `2 + 4 × num_runs`
  (`:246-251`).

  </details>

- [ ] You can explain the density algebra: kernels chosen pairwise per container type.

  <details><summary>Answer</summary>

  Every binary op dispatches on the container *pair* — a 3×3 grid, shown
  by `is_disjoint` (`store/mod.rs:200-213`) and `is_subset` (`:215-227`).
  Array∩array is a flat two-pointer merge (`scalar.rs:37-54` — line 45
  steps by one, **not** the galloping/exponential search CRoaring has);
  array∩bitmap iterates the small array and does an O(1) bit-test per
  element (`store/mod.rs:204-206`), so its cost is O(|array|) whatever the
  bitmap's density; bitmap∩bitmap is 1024 word-wise `&`s.

  So the size-asymmetry win in roaring-rs comes from *container choice*
  (probe the small side into the big bitmap), not from galloping. Under
  the `simd` feature these kernels run 8 `u16` lanes wide in `vector.rs`
  (`:11`, `and` at `:119`) with a scalar tail fallback.

  </details>

- [ ] You can say what happens when a union overflows the array limit.

  <details><summary>Answer</summary>

  In `insert_range`'s array arm the code counts the union *before*
  choosing the output: `union_cardinality = array.len() + added_amount`
  (`container.rs:102`), then branches — `== 1<<16` becomes a full-range
  `Run` (`:103-104`), `<= ARRAY_LIMIT` stays an array (`:106-107`),
  otherwise `self.store = self.store.to_bitmap()` promotes before
  inserting (`:108-110`).

  Counting first beats "build an array, promote if it turns out too big"
  because the exact union cardinality is cheap (an `intersection_len`
  count), whereas building an over-limit array means allocating and
  filling storage you would immediately discard on the demote. The steady
  state is re-asserted by `ensure_correct_store` (`:225`), which promotes
  any array whose `len() > ARRAY_LIMIT` to a bitmap.

  </details>

- [ ] You can explain why cardinality-only operations are the hot path and what they skip.

  <details><summary>Answer</summary>

  Query planning estimates selectivity — "how many rows survive this
  intersection?" — before deciding a plan (topic 9), so it needs the
  *count* of a set operation, not its elements. `intersection_len`
  (`array_store/mod.rs:258`) serves that by driving a `CardinalityCounter`
  visitor (`:259-264`) through `vector::and` / `scalar::and`: it tallies
  matches and returns a `u64`, never allocating a result container.

  What it skips is the output buffer. A full `BitAnd` must materialize the
  intersected set; a cardinality-only op keeps a running counter, and for
  bitmaps that counter is just `u64::count_ones()` summed per word
  (`store/bitmap_store.rs:34`, `:143`) — the hardware `popcnt`, not the
  paper's Harley-Seal AVX2 kernel. Allocating a result you would throw
  away after reading its length would dominate the cost on this path.

  </details>

- [ ] You wrote answers to all questions in notes.md, including the three-adaptive-encodings cross-topic thread with GraphBLAS and HLL.

  <details><summary>Answer</summary>

  The four notes.md questions cover: per-chunk vs per-matrix adaptivity
  (roaring switches per 64K chunk, GraphBLAS per whole matrix, topic 20);
  why counting `union_cardinality` first is cheaper than build-then-promote
  (`container.rs:102`); why cardinality-only ops are zero-allocation
  (`intersection_len`, `array_store/mod.rs:258`); and the cross-topic
  *demotion* column of Step 6's table.

  For that last thread: roaring demotes `Bitmap → Array` when a container
  drops to `len() <= ARRAY_LIMIT` (`container.rs:227-228`); redis HLL
  never demotes dense → sparse (promotion at rank > 32 or bytes > max,
  `src/hyperloglog.c:389`, `:683`, `:863` is one-way); postgres GIN posting
  trees do not collapse back to inline lists. Demotion is rarer than
  promotion because promotion is forced by a size/precision bound while
  demotion is only an optional space reclaim after deletes — which most of
  these workloads never do.

  </details>

## References

**Papers**
- Lemire et al. — "Roaring Bitmaps: Implementation of an Optimized
  Software Library" (Software: Practice & Experience 2018,
  [arXiv:1709.07821](https://arxiv.org/abs/1709.07821)) — §2
  containers, §3 SIMD kernels, skim benchmarks

**Code**
- [roaring-rs](https://github.com/RoaringBitmap/roaring-rs) at commit
  `83caaca` — the Rust port (**not** CRoaring); `roaring/src/bitmap/`
  holds the containers, `store/` the three encodings and the pairwise
  kernels
