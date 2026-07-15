# DiskANN: one SSD read per hop

The paper that put billion-point ANN on SSDs without giving up
recall — topics 3/4's disk-layout discipline applied to graphs.
Before the paper, this chapter builds its three ideas one at a time:
why HNSW can't just be paged to disk, a flat graph built for
provably few hops (Vamana's α-slack pruning), a block layout that
co-locates a node's vector and links so each hop is exactly one
read, and PQ codes in RAM that steer the walk while exact f32
distances rank the results. This chapter assumes
[reading-hnsw-paper.md](reading-hnsw-paper.md) (greedy graph search,
beams, ef) and [reading-pq.md](reading-pq.md) (PQ codes, ADC).

## The problem in one sentence

A billion 128-d vectors need ~512 GB for the vectors plus ~100 GB
for HNSW links — far beyond one machine's RAM — but naively paging
HNSW to SSD turns each query's ~200–500 hops into 2+ random 100 µs
reads apiece, i.e. **~50–100 ms per query**, 10–20× too slow.

## The concepts, step by step

### Step 1 — why HNSW can't just go to disk

HNSW search is a beam of *dependent* point lookups: you can't know
which node to read next until the current node's distances are
computed — topic 0's pointer chase, at SSD latency. On disk each hop
needs the node's vector AND its neighbor list, which in an
RAM-designed layout live in different places — two random reads per
hop:

```
 HNSW paged to SSD, per hop:
   read vector block   ~100 µs ┐  dependent — can't overlap
   read links block    ~100 µs ┘
 × ~300 hops/query  ⇒  ~60 ms/query — dead on arrival
```

DiskANN's redesign targets exactly the metric that matters:
**number of SSD round trips per query**. Every idea below either
removes reads (Steps 2–3) or overlaps them (Step 4).

### Step 2 — Vamana: a flat graph built for few hops

Vamana is DiskANN's graph: no hierarchy — one flat graph with degree
bound R (max links per node, ~64), built so greedy search converges
in few hops. The builder's pruning rule is **RobustPrune**:

```
 RobustPrune(p, candidates, α, R):
   while candidates and |out(p)| < R:
     p* = closest remaining candidate; add edge p→p*
     remove every c with α·d(p*, c) ≤ d(p, c)     ← the α slack
```

α = 1 gives HNSW's Alg-4-style directional pruning (one edge per
direction). The new move: **α > 1 (≈1.2) keeps LONGER edges** — a
candidate is only pruned if the kept edge gets you *α times* closer
to it, so surviving edges shrink the distance to any target
geometrically. Each greedy hop must cut the remaining distance by
≥ α, so hop count is O(log_α of the distance ratio) — the graph
trades extra degree for provably fewer hops.

Build: two passes over random-order points (second pass with final
α), each pass: greedy search from the **medoid** (the dataset's most
central point, the fixed entry) to find candidates, RobustPrune the
visited set, add back-edges.

Levels vs slack, the design fork: HNSW buys few hops with a
hierarchy (extra RAM, layered layout); Vamana buys it with edge
slack (extra degree, flat layout — exactly what disk wants).

### Step 3 — the layout: one node's everything in one block

With hops minimized, make each hop cost exactly one read: store each
node's full vector and its neighbor list *adjacent*, in one
SSD-page-aligned block:

```
 RAM:   PQ codes for ALL points (~16-32 B each)   ← steers the walk
 SSD:   per-node block: [ full f32 vector | R neighbor ids ]
        node's data + links CO-LOCATED — one read per hop
```

The arithmetic: d·4 + R·4 bytes per block — d=128, R=64 → ~768 B —
padded to one 4 KB page. Alignment IS the schema (topic 3's
slotted-page lesson). The RAM side is the PQ trick: at ~16–32 bytes
per point, PQ codes for ALL billion points fit in ~16–32 GB — a full
in-RAM (approximate) distance oracle. The topic-13 echo is exact:
node + adjacency co-located per block = kuzu's CSR node groups;
PQ-in-RAM = the sparse index steering to the right block
(ClickHouse marks, topic 12).

### Step 4 — the search loop: PQ steers, f32 ranks, W reads in flight

Search is beam search with width W (≈4–8): pick the W best
unexpanded candidates *by PQ distance* (RAM, essentially free),
fetch their SSD blocks **as one batch of concurrent reads** —
memory-level parallelism for disks — then use the exact f32 vectors
that just arrived to rank results, and the neighbor ids in the same
blocks to extend the frontier:

```rust
// the disk loop: PQ (RAM) decides where to walk, f32 (SSD) decides the
// ranking — the approximation never touches the final order
fn search(q: &[f32], k: usize, w: usize) -> Vec<(f32, Id)> {
    let mut cands = MinHeap::from([(pq_dist(q, MEDOID), MEDOID)]);
    let mut seen = HashSet::from([MEDOID]);
    let mut results = Vec::new();
    while let Some(beam) = cands.pop_n(w) {          // W best, by PQ distance
        for blk in ssd_read_batch(&beam) {           // W reads IN FLIGHT at once
            results.push((l2(q, &blk.vector), blk.id));   // exact f32 ranks
            for &n in &blk.neighbors {               // links came in the SAME read
                if seen.insert(n) { cands.push((pq_dist(q, n), n)); }
            }
        }
        if converged(&cands, &results, k) { break; }
    }
    top_k(results, k)
}
```

The division of labor is the deep idea: PQ error only affects WHERE
YOU WALK, never the final ranking — rescoring is fused into
traversal, since the exact vector arrives in the block you had to
read anyway. W is the ef of the disk world: wider beams overlap more
SSD reads (latency hiding) but waste reads on candidates that won't
survive.

### Step 5 — the numbers to retain

- **~5 ms mean latency, 95%+ recall@1** on billion-scale SIFT, one
  64 GB machine — the headline; compare Step 1's ~60 ms strawman.
- Hop count ~O(log_α): tens of beam iterations, each one batch of W
  ~100 µs reads overlapped — the latency budget adds up to
  single-digit ms.
- ~R·4 + d·4 bytes per SSD block: R=64, d=128 → ~768 B, padded to a
  4 KB page — ~80% of each read is padding, the price of one-read
  hops.

## How to read the paper (with the concepts in hand)

- **§1–2 (motivation + Vamana)** — Steps 1–2. Read RobustPrune
  twice; the α-slack line is one condition and it carries the whole
  hop bound. The two-pass build detail matters for reproducing
  quality.
- **§3 (the SSD design)** — Steps 3–4: the block layout, PQ-in-RAM,
  beam search with batched reads. This is the section to read
  line-by-line — it's where "disk-layout discipline applied to
  graphs" lives.
- **§4 (evaluation)** — the headline numbers (Step 5); skim the
  ablations but note the beam-width and α sweeps — they're the
  paper's knobs-vs-curve section.

## Questions (answer in notes.md)

1. Count SSD reads: HNSW-on-disk (links and vectors separate) vs
   DiskANN per hop. Where did the factor go?
2. Why α > 1 provably shortens greedy walks — sketch the geometric
   argument (each hop shrinks distance by α).
3. Beam search issues W reads concurrently. Connect to topic 0's
   MLP: what's the SSD equivalent of "10 outstanding misses"?
4. Why is it fine that PQ steers but f32 ranks? What recall failure
   remains possible (PQ error > neighbor spacing → wrong REGION)?
5. M28 preview: DiskANN blocks over object storage — what breaks
   when a "read" is 50 ms S3 GET instead of 100 µs NVMe? Which knob
   moves?

## References

**Papers**
- Subramanya, Devvrit, Kadekodi, Krishnaswamy, Simhadri — "DiskANN:
  Fast Accurate Billion-point Nearest Neighbor Search on a Single
  Node" (NeurIPS 2019) — §2 Vamana + RobustPrune, §3 the SSD design;
  the eval headline numbers are in §4

**Code**
- [DiskANN](https://github.com/microsoft/DiskANN) — Microsoft's
  production implementation of the paper (optional; the paper is
  self-contained)
