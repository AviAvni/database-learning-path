# Roaring bitmaps: no single set representation wins

The set representation that ate the world: Lucene doc-id sets,
Spark, ClickHouse, Druid, Pilosa — and the `postings::Roaring`
stub. The insight is that NO single representation wins: sorted
arrays win sparse, bitmaps win dense, so partition the 32-bit space
into 64K chunks and choose per chunk. This chapter builds that
argument from first principles — the two base representations and
their break-even point, the two-level partition, the per-pair
kernel matrix — and ends with why posting lists (the filter lane of
a search engine) care.

Source pins: two papers, cited by arXiv id below; the code you
implement is this repo's `experiments/src/postings.rs` stub; the
production cousin quoted at the end is RediSearch at `87276ca`. A
**posting list** here means the set of doc ids that contain a term.

## The problem in one sentence

Store "the set of doc ids matching a filter" so that both a
172-element set and a 99,888-element set (out of 100K docs) are
small AND intersect fast — a sorted `Vec<u32>` makes the dense one
400 KB and the intersection walk all 99,888 elements (measured:
52 µs), when the right representations do it in ~1 µs and 16 KiB.

## The concepts, step by step

### Step 1 — two ways to store a set of integers, and the break-even

> **In:** a set of integers drawn from a 65,536-value universe.
> **Out:** two representations (sorted array, bitmap) and the density where their sizes cross — 4096 elements — below which the array is smaller, above which the bitmap is.

A set of integers has two classic representations. A **sorted
array** stores each member explicitly — cost proportional to *how
many* members (2 bytes each if values fit u16). A **bitmap** stores
one bit per *possible* value — cost proportional to the *universe
size*, membership is one bit test, and intersection is a word-wise
AND running at 64 members per instruction. Over a 65,536-value
universe the bitmap costs a flat 8 KiB; the array costs
2·|set| bytes. Equating them (worked):

```
bitmap:  65536 bits / 8 = 8192 bytes                 (flat, any density)
array:   2 bytes/value · |set|
equal:   8192 / 2 = 4096 elements                    ← the crossover
  |set| =  100:  array 200 B    vs bitmap 8192 B   → array wins
  |set| = 4096:  array 8192 B   vs bitmap 8192 B   → tie
  |set| =50000:  array 100000 B vs bitmap 8192 B   → bitmap wins (12×)
```

Below 4096 the array is smaller, above it the bitmap is. Density
decides, and real data mixes both regimes in one set.

### Step 2 — the partition: choose a representation every 64K values

> **In:** a full 32-bit value space where density varies across ranges.
> **Out:** split each u32 into a 16-bit chunk key and 16-bit low half; each chunk stores its low bits in a **container** whose type is chosen by that chunk's local density — capping size at 8 KiB and at 2 bytes/value simultaneously.

Roaring splits each 32-bit value into high and low halves: the high
16 bits select a **chunk** (one of up to 64K aligned ranges of
65,536 values), and each chunk stores its members' low 16 bits in
its own **container**, whose type is chosen by that chunk's local
density:

```
  u32 value = [ high 16 bits | low 16 bits ]
                    │              │
                    ▼              ▼
     sorted Vec of (key, container); container holds the low bits:

     Array container: sorted Vec<u16>       when |chunk| ≤ 4096
     Bitmap container: [u64; 1024] = 8 KiB  when |chunk| > 4096
     Run container: (start,len) pairs       (the '16 paper's addition)

  4096 = the crossover where 2 bytes/value (array) meets
         8 KiB/65536 possible values (bitmap) — a container is
         NEVER worse than 2 bytes per value, and never bigger
         than 8 KiB.
```

The guarantee that falls out: every container is at most 8 KiB
*and* at most 2 bytes per stored value — the adaptive choice caps
both failure modes. The **run container** ((start, length) pairs —
run-length encoding, the 2016 Lemire paper's addition) handles the
third regime the first paper missed: long consecutive runs of ids,
where even a bitmap wastes bits. The 2016 paper (§4) only converts
to a run container when it would be smaller than *both* alternatives
and there are ≤ 2047 runs — and *only* on an explicit `runOptimize`
call, never automatically (question 1 asks which posting-list
shapes produce runs). This repo's Rust stub implements array +
bitmap only (`postings::Container`), the two the CRoaring reference
starts from.

### Step 3 — the kernel matrix: one algorithm per container pair

> **In:** two roaring bitmaps, each a sorted list of typed containers.
> **Out:** the set operation decomposes into per-chunk kernels, dispatched by the *pair* of container types — each kernel the textbook-optimal algorithm for that shape.

With two (or three) container types, a set operation between two
roaring bitmaps decomposes into per-chunk operations, each
dispatched to a specialized **kernel** by the pair of container
types (the kernel matrix the stub implements):

| A ∩/∪ B | array | bitmap |
|---|---|---|
| **array** | two-pointer merge (galloping when sizes differ ≥64×) | probe each u16 into the bitmap: O(|array|) word tests |
| **bitmap** | ← same, swapped | 1024 word-wise AND/OR + popcount to pick the OUTPUT container type |

Each kernel is the textbook-optimal algorithm *for that shape*:
two sorted arrays → two-pointer merge, escalating to **galloping**
(exponential jump-ahead search from the small list into the big
one) when one side is ≥64× smaller; array vs bitmap → probe each
array element (one word test each), never touching the bitmap's
other 65K bits; bitmap vs bitmap → 1024 unconditional word ANDs.
The stub you fill in stores containers exactly this way and returns
a plain `Vec<u32>` to compare against the two-pointer oracle:

```rust
// ILLUSTRATION — the design the reader implements in the stub at
// experiments/src/postings.rs:56-86 (Container enum, ARRAY_MAX = 4096,
// and()/or() stubs). Real bodies are `todo!()`; fill them to match this.
fn and(a: &Container, b: &Container) -> Container {
    match (a, b) {
        (Array(x), Array(y))  => two_pointer(x, y),     // gallop if ≥64× skew
        (Array(x), Bitmap(y)) =>                        // probe the small side
            Array(x.iter().copied().filter(|&v| y.get(v)).collect()),
        (Bitmap(x), Bitmap(y)) => {
            let mut w = [0u64; 1024];
            let mut card = 0u32;
            for i in 0..1024 {
                w[i] = x.words[i] & y.words[i];
                card += w[i].count_ones();      // popcount FUSED into the AND
            }
            if card <= 4096 { to_array(&w) } else { Bitmap(w) }
        }
        (Bitmap(_), Array(_)) => and(b, a),     // commute to the probe case
    }
}
```

### Step 4 — the two details that carry the performance

> **In:** the kernel matrix, which looks like a mechanical case-split.
> **Out:** the two decisions that actually decide performance — choosing the *output* container type by popcount, and tracking cardinality as a byproduct rather than recomputing it.

The match arms are obvious; two less-obvious decisions do the real
work:

- **output container choice**: bitmap∩bitmap may produce a sparse
  result — popcount during the AND, convert to array if ≤4096. Skip
  this and intersections degrade the structure until every chunk is
  a mostly-empty 8 KiB bitmap. (Union of bitmaps stays bitmap —
  cardinality never shrinks.)
- **cardinality is tracked**, not recomputed — every kernel returns
  it as a byproduct (the popcount is fused into the AND loop; on
  M-series that's `cnt` on each of 1024 words, memory-bound anyway),
  so the ≤4096 decision and later size queries are free.

The general lesson: an adaptive data structure lives or dies by its
*transition* logic, not its steady states.

### Step 5 — why posting lists care: the filter lane

> **In:** the two measured intersections from this topic — dense∩sparse and dense∩dense.
> **Out:** why roaring turns both the memory and the time of the dense side down by ~25×, and why this is the FILTER lane, not the RANKING lane BM25/WAND own.

Measured in fts_bench (this topic's `notes.md`): `t0 ∧ t5000`
(99,888 ∩ 172 docs) costs 52 µs with two-pointer — it walks all
99,888. Roaring: t0 at df≈100K over 100K docs is ~1.5 dense chunks
→ bitmap containers; the 172-element side probes 172 times → ~1 µs.
Same asymmetry galloping fixes for arrays, but roaring ALSO
compresses t0 to 8 KiB·2 instead of 400 KB — 25× less memory
traffic on the dense side, which is where the time actually goes
(question 3).

Lucene's `RoaringDocIdSet` and RediSearch's doc tables use exactly
this for filters (the `doc_ids_only` codec at
`src/redisearch_rs/inverted_index/src/codec/doc_ids_only.rs` is the
varint cousin — `RECOMMENDED_BLOCK_ENTRIES = 1000` there, doc_ids_only.rs:26).
Note what roaring does NOT store: tf, positions, scores — it's the
FILTER lane (Cypher `WHERE n.name CONTAINS ...` feeding a graph
traversal), not the RANKING lane; BM25/WAND (the previous chapters)
own that one. And a bitmap container is exactly a dense GraphBLAS
vector chunk (question 4) — the M20/M23 bridge.

## How to read the papers (with the concepts in hand)

Two short papers, both readable in one sitting:

- **Chambi et al. (Software: Practice & Experience 2016,
  [arXiv:1402.6407](https://arxiv.org/abs/1402.6407)).** §2 is
  Steps 1–2 (the partition and the 4096 crossover); §3 is Step 3's
  kernel matrix — read it against the `match` above and check every
  arm. This paper has TWO container types only (array + bitmap), and
  the experiments compare against WAH/Concise (older compressed
  bitmaps that lack random access) — skim, the lesson is that
  chunked-and-adaptive beats stream-compressed.
- **Lemire et al. (SPE 2016,
  [arXiv:1603.06549](https://arxiv.org/abs/1603.06549)).** Adds the
  run container (Step 2's third regime) and SIMD kernels, and
  describes the CRoaring C reference implementation; read the
  run-container conversion rules (§4: convert only when smaller than
  both, ≤2047 runs, only on `runOptimize`) — the same
  transition-logic discipline as Step 4.
- Then implement the `postings::Roaring` stub
  (`experiments/src/postings.rs`) — array/bitmap containers with
  AND/OR against the two-pointer vec oracle — before answering the
  questions.

## Questions (answer in notes.md)

1. Derive the 4096 crossover from bytes/value. Where does the
   run-container (RLE) change the math, and what posting-list shape
   produces runs (hint: doc ids assigned by insertion order +
   crawler locality)?
2. Our t0 has df 99888 over doc space 100K = 99.9% dense. What does
   its bitmap∩bitmap AND cost vs the measured 97 µs two-pointer for
   t0∧t1? Predict before implementing (1024·2 words ANDed…).
3. Galloping (skewed array∩array) vs container probing (array∩bitmap):
   both are O(small·log/const). When does roaring still win despite
   equal asymptotics? (memory traffic of the big side)
4. M20 tie-in: a bitmap container IS a dense GraphBLAS vector chunk;
   array container = sparse. Roaring's per-chunk format switch is
   GraphBLAS's sparse↔bitmap format lattice at 64K granularity —
   compare the switch thresholds (4096/65536 vs GB_conform's).
5. M23: full-text hit set → roaring → feed as mask into a matrix
   traversal. What conversion does FalkorDB pay today going
   RediSearch → node-id set → GraphBLAS vector, and what would a
   native roaring-masked mxv save?

## Done when

Answer each before unfolding it.

- [ ] You can derive the 4096 crossover from bytes per value.
  <details><summary>the arithmetic</summary>
  Bitmap is flat 65536/8 = 8192 bytes. Array is 2 bytes/value.
  8192/2 = 4096 values is where they cost the same; below it array
  is smaller, above it bitmap is. A container therefore never
  exceeds 8 KiB nor 2 bytes/value.
  </details>
- [ ] You can explain why the representation is chosen per 64K range rather than per set.
  <details><summary>local vs global density</summary>
  One set can be sparse in some 64K ranges and dense in others.
  Choosing per chunk (high 16 bits) lets each range pick the smaller
  representation, so the structure adapts to *local* density instead
  of paying one global choice.
  </details>
- [ ] You can name the kernel matrix idea: one algorithm per container pair.
  <details><summary>dispatch by type pair</summary>
  Each set op dispatches on (typeA, typeB): array∩array → two-pointer
  (gallop on skew), array∩bitmap → probe the array into the bitmap,
  bitmap∩bitmap → 1024 word ANDs + popcount to choose the output
  type. Each is optimal for that shape.
  </details>
- [ ] You can say what a 99.9%-dense list like `t0` (df 99,888 of 100,000) should become, and its cost against the sorted-vec baseline.
  <details><summary>bitmap containers</summary>
  ~1.5 dense chunks → bitmap containers (8 KiB each, ~16 KiB total vs
  400 KB as a Vec<u32>). Intersection with a sparse side probes the
  small side (~1 µs); dense∩dense is ~1024 word ANDs per chunk —
  against the sorted-vec baseline measured here (0.1178 ms for
  dense∧dense, notes.md).
  </details>
- [ ] You wrote answers to all five questions in notes.md, including the M20 bitmap-container tie-in.
  <details><summary>check</summary>
  Five answers in notes.md; question 4 explicitly maps a bitmap
  container to a dense GraphBLAS vector chunk and an array container
  to a sparse one, comparing the 4096/65536 thresholds to GraphBLAS's
  format lattice.
  </details>

## References

**Papers**
- Chambi, Lemire, Kaser, Godin — "Better bitmap performance with
  Roaring bitmaps" (Software: Practice & Experience 2016,
  [arXiv:1402.6407](https://arxiv.org/abs/1402.6407)) — the
  array/bitmap containers and the kernel matrix (§3)
- Lemire, Ssi-Yan-Kai, Kaser — "Consistently faster and smaller
  compressed bitmaps with Roaring" (SPE 2016,
  [arXiv:1603.06549](https://arxiv.org/abs/1603.06549)) — adds the
  run container (§4, the ≤2047-run / runOptimize rule) and SIMD
  kernels; describes the CRoaring reference implementation

**Code**
- This repo — `experiments/src/postings.rs`: `Container` enum
  (:58-61), `ARRAY_MAX = 4096` (:56), `Roaring::and`/`or` stubs
  (:79-86), the `vec_and`/`vec_or` oracle (:11, :29)
- [RediSearch](https://github.com/RediSearch/RediSearch) `@87276ca`
  `src/redisearch_rs/inverted_index/src/codec/doc_ids_only.rs` — the
  varint doc-id codec, `RECOMMENDED_BLOCK_ENTRIES = 1000` (:26)
