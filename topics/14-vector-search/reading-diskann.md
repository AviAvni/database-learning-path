# DiskANN: one SSD read per hop

The paper that put billion-point ANN on SSDs without giving up
recall — topics 3/4's disk-layout discipline applied to graphs. Three
ideas carry it: a flat graph built for provably few hops (Vamana's
α-slack pruning), a block layout that co-locates a node's vector and
links so each hop is exactly one read, and PQ codes in RAM that steer
the walk while exact f32 distances rank the results.

## 1. Why HNSW can't just go to disk

HNSW search = a beam of dependent point lookups; on disk each hop is
a random read of (vector + links) living in different places. With
~200-500 hops per query and SSD reads at ~100 µs, naive paging is
dead on arrival. DiskANN's redesign targets exactly the metric that
matters: **number of SSD round trips per query**.

## 2. Vamana: a flat graph built for few hops

No hierarchy — one graph, degree bound R, built with **RobustPrune**:

```
 RobustPrune(p, candidates, α, R):
   while candidates and |out(p)| < R:
     p* = closest remaining candidate; add edge p→p*
     remove every c with α·d(p*, c) ≤ d(p, c)     ← the α slack
```

α = 1 gives HNSW's Alg-4-style directional pruning. **α > 1 (≈1.2)
keeps LONGER edges** — each greedy hop must shrink the distance to
target by ≥ α, so hop count is O(log_α) — the graph trades extra
degree for provably fewer hops. Build: two passes over random-order
points (second pass with final α), each: greedy search from the
medoid entry point, RobustPrune the visited set, add back-edges.

Levels vs slack: HNSW buys few hops with a hierarchy (extra RAM);
Vamana buys it with edge slack (extra degree, flat layout — exactly
what disk wants).

## 3. The layout + the steering trick

```
 RAM:   PQ codes for ALL points (~16-32 B each)   ← steers the walk
 SSD:   per-node block: [ full f32 vector | R neighbor ids ]
        node's data + links CO-LOCATED — one read per hop
```

Search: beam search (width W ≈ 4-8) — pick next candidates by PQ
distance (RAM, free), fetch their SSD blocks (batched, async — MLP
for disks!), compute EXACT distances from the fetched f32 vectors to
rank results. PQ error only affects WHERE YOU WALK, not the final
ranking — rescoring fused into traversal.

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

The topic-13 echo is exact: node + adjacency co-located per block =
kuzu's CSR node groups; PQ-in-RAM = the sparse index steering to the
right block (ClickHouse marks, topic 12).

## 4. Numbers to retain

- ~5 ms mean latency, 95%+ recall@1 on billion-scale SIFT, one
  64 GB machine — the headline
- beam width W trades SSD parallelism for wasted reads (the ef of
  the disk world)
- ~R·4 + d·4 bytes per SSD block: R=64, d=128 → ~768 B — pad to one
  4 KB page, alignment IS the schema (topic 3's slotted-page lesson)

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
