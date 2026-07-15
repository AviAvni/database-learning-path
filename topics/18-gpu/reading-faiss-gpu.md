# Faiss GPU: k-select that never leaves registers

Johnson, Douze & Jégou's 2017 paper made GPU ANN real: IVF-PQ
(topic 14's quantization ladder) at billion scale, built around one
algorithmic contribution — k-selection that never leaves registers —
and one systems discipline: keep the index resident, stream only
queries. It's Crystal's regime B practiced before Crystal named it.
This chapter builds the six concepts the paper assumes — the IVF-PQ
vocabulary, the residency rule, why top-k selection was the
bottleneck, and how a sorting network fixes it — then routes you to
the two sections that matter.

## The problem in one sentence

An IVF scan produces millions of candidate distances per query and
you need the 100 best; sorting them costs O(n log n) of memory
traffic and the CPU's answer — a heap — is serial and branchy, so on
a 32-lane lockstep GPU the *selection*, not the distance math, was
the bottleneck.

## The concepts, step by step

### Step 1 — the IVF-PQ vocabulary (topic 14 in five terms)

Faiss's billion-scale index compresses and partitions before it ever
computes a distance. **IVF** (inverted file): cluster all vectors
into ~√n buckets by a **coarse quantizer** (k-means centroids —
finding a query's nearest centroids is a small brute-force matrix
multiply); at query time scan only the `nprobe` nearest buckets.
**PQ** (product quantization): split each vector into m sub-vectors
(e.g. 8), quantize each to 1 byte against its own 256-entry
codebook — a 128-dim f32 vector (512 B) becomes an 8-byte code, so
1B vectors fit in 8 GB. **ADC** (asymmetric distance computation):
the query stays uncompressed; per query you precompute a 256-entry
distance table per sub-quantizer, and each candidate's distance is
just m table lookups. That turns "distance to 1M candidates" into a
memory-bandwidth problem — which is exactly what the GPU has to
sell.

### Step 2 — the residency rule: the index lives on the device

A discrete GPU's HBM (high-bandwidth memory, ~900 GB/s) is reachable
from the host only over PCIe (~16 GB/s), so Faiss places each piece
of data by its lifetime — permanent things on the fast side, the
per-query trickle over the bus:

```
 what              where              why
 PQ codes (1B×8B)  GPU HBM (8-32 GB)  scanned every query — needs bandwidth
 coarse centroids  HBM                tiny
 original vectors  CPU RAM / disk     only for optional rescore
 queries           PCIe per batch     small — the ONLY per-query transfer
```

Crystal's regime B by design: the billion-scale index lives on
device; a query batch ships kilobytes, not gigabytes. Question: our
gpu_bench shipped the DATA per call and lost everywhere — restate
Faiss's layout rule as a rule about which side of the bus each
data-lifetime class belongs on.

### Step 3 — why heaps fail on warps

A warp is 32 threads executing one instruction in lockstep. A binary
heap's insert takes a *data-dependent* path — compare, maybe swap,
maybe recurse — so 32 lanes inserting 32 different values each want
a different instruction sequence, and the warp serializes
(divergence: both paths execute, lanes masked). A **sorting
network** does the opposite: a *fixed* schedule of compare-exchange
operations that is identical no matter what the data is — every lane
executes the same instruction always, and "maybe swap" becomes a
branch-free min/max. Data-independent schedule = zero divergence —
the same reason branchless filter won at 50% selectivity in
topic 17.

### Step 4 — WarpSelect: the top-k machine (their §4, the real contribution)

The contribution: keep the whole k-selection state in **registers**
(each thread's private, fastest storage — zero memory traffic) and
communicate only by **warp shuffles** (instructions that move values
lane-to-lane without touching memory):

```
 each lane keeps a tiny sorted queue IN REGISTERS
 insert: compare-exchange against lane's queue (predicated, no branch)
 when any lane's queue overflows → odd-even merge network across
 the warp (warp shuffles, no shared memory), rebuild thresholds
 end: merge 32 lane-queues once → warp's top-k
```

```rust
// WarpSelect, one lane's view: a tiny sorted queue in REGISTERS
let mut queue = [f32::INFINITY; Q];       // lane-local register array
let mut threshold = f32::INFINITY;        // the warp's current kth-best
for d in my_stripe_of_distances {
    if d < threshold {                    // overwhelmingly false → no work
        queue.insert_sorted(d);           // predicated compare-exchange
    }
    if ballot_any_lane_full() {           // warp vote, no shared memory
        odd_even_merge_across_warp();     // fixed schedule = zero divergence
        threshold = kth_best();           // queues drain, threshold tightens
    }
}
// end: merge the 32 lane-queues once → the warp's top-k
```

One pass over the distances, k-select at register speed. The fast
path is the `if d < threshold` test: as the threshold tightens,
almost every distance fails it and costs one predicated compare.
This is topic 17's "sorting networks beat comparison sorts at small
fixed n" scaled to warps — and CAGRA's bitonic itopk is its
descendant. Question: why do sorting NETWORKS (fixed
compare-exchange schedule) fit SIMT while heaps don't
(data-independent schedule = no divergence — the same reason
branchless filter won at 50% selectivity)?

### Step 5 — the full query pipeline on device

With Steps 1–4 in hand, the whole IVF-PQ query is four stages, each
placed in the memory tier it needs:

- coarse quantizer: query → nprobe nearest inverted lists (a small
  brute-force matmul — cuBLAS)
- ADC lookup tables: per query × subquantizer, built in shared
  memory (256 entries × m subquantizers)
- scan: each thread streams PQ codes, 8 table lookups per 8-byte
  code, feeds WarpSelect — crucially *fused*: distances flow
  straight into k-select, never materialized to HBM
- batch everything: queries × lists tiled to saturate SMs

Question: the ADC tables are per-QUERY — at what batch size does
shared memory run out, and what's the fallback (smaller tiles, or
float16 tables)? Compare CAGRA's shared-memory budget fight.

### Step 6 — the numbers that set expectations (2017 hardware, still directive)

- brute-force k-NN on 1M×128d: ~20× over CPU (dense matmul — the
  best case; this is our l2_batch stub's ceiling shape)
- billion-scale IVF-PQ: ~8.5× over prior GPU art; k-select was the
  bottleneck they removed
- multi-GPU: shard lists (data parallel) or replicate (query
  parallel) — topic 15's scaling menu, verbatim

What transfers to M14/M18: our M14 pipeline (PQ scan → rescore)
maps 1:1 — PQ scan is the GPU-shaped half (regular,
bandwidth-bound, k-select), rescore is gather-heavy (CPU keeps it
unless candidates batch well). M18's distance-scoring flag should
implement the brute-force tile first — it's the ~20× case above and
needs no index redesign.

## How to read the paper (with the concepts in hand)

- **§4 (k-selection) — read carefully.** This is Steps 3–4, the
  actual contribution: the thread-queue/warp-queue split, the
  odd-even merge network, and the measured selection throughput.
  Watch for the overflow threshold t (question 1).
- **§5 (the system) — the layout table.** Step 2's residency
  discipline and Step 5's fused pipeline in the authors' words;
  the multi-GPU sharding/replication menu ends the section.
- Skim the rest: §2–3 are Step 1's IVF-PQ background (topic 14
  covered it), and the evaluation's absolute numbers are 2017
  hardware — read them as ratios, not throughputs.

## Questions for notes.md

1. WarpSelect keeps k ≤ ~1024 in registers per warp. What breaks
   at larger k, and what did they use before overflow (thread-queue
   + warp-queue two-level — find the threshold t)?
2. Faiss streams distances INTO k-select fused (no materialized
   distance array). Crystal made the same fusion argument — what's
   the HBM traffic ratio, fused vs staged, for 1M distances/query?
3. The coarse quantizer is a matmul (batch queries × centroids) —
   why does THIS piece hit near-peak FLOPs while the PQ scan is
   bandwidth-bound (arithmetic intensity of each)?
4. Their multi-GPU sharding sends every query to every shard;
   replication doesn't. Map to topic 15's read-scaling vs
   partitioning — which does recall@k prefer (shard = exact merge,
   replica = independent)?
5. For M18: l2_batch(1 query × 100K targets, dim 128) ≈ their
   brute-force case at batch 1. Predict from the roofline whether
   Metal wins BEFORE running your implementation — then check.

## References

**Papers**
- Johnson, Douze, Jégou — "Billion-scale similarity search with
  GPUs" ([arXiv:1702.08734](https://arxiv.org/abs/1702.08734), IEEE
  Trans. on Big Data 2019) — §4 (k-selection) is the real
  contribution; §5's layout table is the systems lesson
