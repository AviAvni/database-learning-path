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

## The problem in one sentence

Store "the set of doc ids matching a filter" so that both a
172-element set and a 99,888-element set (out of 100K docs) are
small AND intersect fast — a sorted `Vec<u32>` makes the dense one
400 KB and the intersection walk all 99,888 elements (measured:
52 µs), when the right representations do it in ~1 µs and 16 KiB.

## The concepts, step by step

### Step 1 — two ways to store a set of integers, and the break-even

A set of integers has two classic representations. A **sorted
array** stores each member explicitly — cost proportional to *how
many* members (2 bytes each if values fit u16). A **bitmap** stores
one bit per *possible* value — cost proportional to the *universe
size*, membership is one bit test, and intersection is a word-wise
AND running at 64 members per instruction. Over a 65,536-value
universe the bitmap costs a flat 8 KiB; the array costs
2·|set| bytes. Equating them: 8192 bytes / 2 bytes = **4096
elements** — below that the array is smaller, above it the bitmap
is. Density decides, and real data mixes both regimes in one set.

### Step 2 — the partition: choose a representation every 64K values

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
run-length encoding, the 2016 paper's addition) handles the third
regime the first paper missed: long consecutive runs of ids, where
even a bitmap wastes bits (question 1 asks which posting-list
shapes produce runs).

### Step 3 — the kernel matrix: one algorithm per container pair

With two (or three) container types, a set operation between two
roaring bitmaps decomposes into per-chunk operations, each
dispatched to a specialized **kernel** by the pair of container
types (§3 of the paper — what the stub implements):

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

```rust
// the whole design in one match: kernel AND output type chosen per chunk
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

Measured in fts_bench: `t0 ∧ t5000` (99888 ∩ 172 docs) costs 52 µs
with two-pointer — it walks all 99888. Roaring: t0 at df≈100K over
100K docs is ~1.5 dense chunks → bitmap containers; the 172-element
side probes 172 times → ~1 µs. Same asymmetry galloping fixes for
arrays, but roaring ALSO compresses t0 to 8 KiB·2 instead of 400 KB
— 25× less memory traffic on the dense side, which is where the
time actually goes (question 3).

Lucene's `RoaringDocIdSet` and RediSearch's doc tables use exactly
this for filters (the `docs_ids_only` codec in
`redisearch_rs/inverted_index/src/codec/doc_ids_only.rs` is the
varint cousin). Note what roaring does NOT store: tf, positions,
scores — it's the FILTER lane (Cypher `WHERE n.name CONTAINS ...`
feeding a graph traversal), not the RANKING lane; BM25/WAND (the
previous chapters) own that one. And a bitmap container is exactly
a dense GraphBLAS vector chunk (question 4) — the M20/M23 bridge.

## How to read the papers (with the concepts in hand)

Two short papers, both readable in one sitting:

- **Chambi et al. 2014/2016 (arXiv:1402.6407).** §2 is Steps 1–2
  (the partition and the 4096 crossover); §3 is Step 3's kernel
  matrix — read it against the `match` above and check every arm.
  The experiments compare against WAH/Concise (older compressed
  bitmaps that lack random access) — skim, the lesson is that
  chunked-and-adaptive beats stream-compressed.
- **Lemire et al. 2016 (arXiv:1603.06549).** Adds the run container
  (Step 2's third regime) and SIMD kernels; read the run-container
  conversion rules ("convert only when smaller") — the same
  transition-logic discipline as Step 4.
- Then implement the `postings::Roaring` stub — array/bitmap
  containers with AND/OR against the two-pointer vec oracle —
  before answering the questions.

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

## References

**Papers**
- Chambi, Lemire, Kaser, Godin — "Better bitmap performance with
  Roaring bitmaps" (Software: Practice & Experience 2016,
  [arXiv:1402.6407](https://arxiv.org/abs/1402.6407)) — the
  array/bitmap containers and the kernel matrix (§3)
- Lemire, Ssi-Yan-Kai, Kaser — "Consistently faster and smaller
  compressed bitmaps with Roaring" (SPE 2016,
  [arXiv:1603.06549](https://arxiv.org/abs/1603.06549)) — adds the
  run container and the SIMD kernels
