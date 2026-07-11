# Reading guide — Crystal ("A Study of the Fundamental Performance Characteristics of GPUs and CPUs for Database Analytics", SIGMOD '20)

Shanbhag, Madden, Yu. The paper that settled a decade of "GPU
databases: hype?" papers by building the fairest possible comparison:
a tile-based GPU query library (Crystal) vs a state-of-the-art CPU
baseline, on Star Schema Benchmark, with the transfer question made
explicit.

## 1. The framing: two regimes, two verdicts

```
 regime A: data ships over PCIe per query (coprocessor model)
   GPU time ≈ transfer time; PCIe ~16 GB/s vs CPU membw ~100 GB/s
   → CPU WINS almost always. Full stop.
 regime B: working set resident in GPU HBM (primary-store model)
   HBM ~880 GB/s vs CPU ~100 GB/s
   → GPU wins by ~ the bandwidth ratio (they measure ~16× on SSB)
```

Everything else in the literature is confusion between A and B. Our
gpu_bench's no-crossover table is regime A in miniature — except on
unified memory the "transfer" is a staging copy + ~1.5 ms dispatch
overhead, and the bandwidth RATIO is ~1, so even regime B wouldn't
save a memory-bound scan on this Mac. Question: what DOES unified
memory save, and which operator class exploits it (arithmetic
intensity — the l2_batch stub)?

## 2. The tile-based execution model

Crystal's core idea: process a query as a sequence of BLOCK-WIDE
functions over tiles (a tile = items per thread × threads per
block), staged through shared memory:

```
 load tile → coalesced, all threads
 BlockPred: each thread evaluates predicate on its items → flags
 BlockScan: prefix-sum flags → output offsets  (compaction!)
 BlockShuffle / BlockAggregate / BlockProbe ...
 write tile → coalesced
```

It's topic 11's vectorized execution with tiles for batches and
shared memory for the L1-resident chunk — and the compaction step
is topic 17's compress, built from scan instead of vpcompress.
Question: why does GPU filter output need a prefix-scan where the
CPU used a cursor `k += mask`? (No total order across 100K threads
— the scan MAKES one.)

## 3. What they measure that people forget

- **Fused vs staged operators**: materializing intermediates to HBM
  between operators wastes the bandwidth win; Crystal fuses the
  whole SSB query into one kernel (topic 11's operator fusion,
  mandatory now).
- **Selection via scan+compact beats branch-per-thread** at mid
  selectivities — the topic 17 selectivity curve, GPU edition.
- **CPU baseline honesty**: their CPU code is AVX-vectorized and
  multi-threaded; most prior "100× GPU speedups" compared against
  scalar single-thread CPU code (topic 0's fair-benchmarking paper,
  case study #1).

## 4. The cost model worth memorizing

For a scan-shaped operator: `time = max(bytes/membw, flops/peak)`.
GPU wins iff data is resident AND the op is bandwidth-bound (ratio
~9×) or compute-bound with high intensity (ratio can be ~50×).
Neither holds for ship-per-query. Question: place these on the
roofline: sum (0.25 FLOP/byte), filter (0.25), hash probe (~1 +
random access), l2 dim=128 (~32), CAGRA search (~high + irregular).
Which two belong on a GPU at all?

## Questions for notes.md

1. SSB is denormalized-star scans. Which topic 22 benchmark shape
   would flip the verdict back to CPU even in regime B (hint:
   point lookups, topic 3)?
2. Crystal predates Apple unified memory. Rewrite their regime
   table for M-series: what replaces PCIe, what replaces HBM, and
   why does the GPU still lose our sum bench?
3. Their group-by uses atomics into a hash table when groups are
   few. At what group cardinality does that collapse, and what's
   the fallback (cudf's shared-mem vs global split)?
4. Fusing the whole query into one kernel kills operator-at-a-time
   profiling. What replaces topic 0's flamegraph on GPU (NSight /
   Metal capture — occupancy + achieved bandwidth per kernel)?
5. For M18: our engine's hot paths are graph expand (random),
   filter (streaming), distance scoring (dense). Apply §4's
   roofline to each and write the one-line go/no-go.
