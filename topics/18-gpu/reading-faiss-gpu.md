# Reading guide — Faiss GPU ("Billion-scale similarity search with GPUs", arXiv:1702.08734)

Johnson, Douze, Jégou. The 2017 paper that made GPU ANN real:
IVF-PQ (topic 14's quantization ladder) at billion scale, built
around one algorithmic contribution — k-selection that never leaves
registers — and one systems discipline: keep the index resident,
stream only queries.

## 1. The memory-tier layout (the whole system in one table)

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

## 2. The k-select problem (their §4, the real contribution)

IVF scan produces millions of distances per query; you need the
top-k WITHOUT sorting (sort is O(n log n) of HBM traffic). CPU used
a heap — serial, branchy, SIMT-hostile. Faiss: WarpSelect —

```
 each lane keeps a tiny sorted queue IN REGISTERS
 insert: compare-exchange against lane's queue (predicated, no branch)
 when any lane's queue overflows → odd-even merge network across
 the warp (warp shuffles, no shared memory), rebuild thresholds
 end: merge 32 lane-queues once → warp's top-k
```

One pass over the distances, k-select at register speed. This is
topic 17's "sorting networks beat comparison sorts at small fixed
n" scaled to warps — and CAGRA's bitonic itopk is its descendant.
Question: why do sorting NETWORKS (fixed compare-exchange schedule)
fit SIMT while heaps don't (data-independent schedule = no
divergence — the same reason branchless filter won at 50%
selectivity)?

## 3. IVF-PQ on device (topic 14 vocabulary check)

- coarse quantizer: query → nprobe nearest inverted lists (a small
  brute-force matmul — cuBLAS)
- ADC lookup tables: per query × subquantizer, built in shared
  memory (256 entries × m subquantizers)
- scan: each thread streams PQ codes, 8 table lookups per 8-byte
  code, feeds WarpSelect
- batch everything: queries × lists tiled to saturate SMs

Question: the ADC tables are per-QUERY — at what batch size does
shared memory run out, and what's the fallback (smaller tiles, or
float16 tables)? Compare CAGRA's shared-memory budget fight.

## 4. Numbers that set expectations (2017 hardware, still directive)

- brute-force k-NN on 1M×128d: ~20× over CPU (dense matmul — the
  best case; this is our l2_batch stub's ceiling shape)
- billion-scale IVF-PQ: ~8.5× over prior GPU art; k-select was the
  bottleneck they removed
- multi-GPU: shard lists (data parallel) or replicate (query
  parallel) — topic 15's scaling menu, verbatim

## 5. What transfers to M14/M18

Our M14 pipeline (PQ scan → rescore) maps 1:1: PQ scan is the
GPU-shaped half (regular, bandwidth-bound, k-select), rescore is
gather-heavy (CPU keeps it unless candidates batch well). M18's
distance-scoring flag should implement the brute-force tile first —
it's §4's 20× case and needs no index redesign.

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
