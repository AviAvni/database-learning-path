# GPU vs CPU for analytics: two regimes, two verdicts

Shanbhag, Madden & Yu's Crystal paper settled a decade of "GPU
databases: hype?" papers by building the fairest possible comparison:
a tile-based GPU query library vs a state-of-the-art CPU baseline, on
Star Schema Benchmark, with the transfer question made explicit. Its
two-regime framing is the go/no-go lens for every operator M18
considers offloading. Before you open the paper, this chapter builds
the seven concepts it assumes — how a GPU executes, what its memory
rules are, why the bus dominates, and how to predict a winner with
one max() formula — then hands you a section-by-section route.

## The problem in one sentence

A discrete GPU has ~9× the memory bandwidth of a CPU (~880 vs
~100 GB/s) but sits behind a PCIe bus that moves only ~16 GB/s — so
for a scan-shaped query the GPU is either **~16× faster** or
**~6× slower** depending on one question: which side of the bus does
the data live on?

## The concepts, step by step

### Step 1 — SIMT: the GPU hides latency with thread count, not caches

A GPU is a processor that runs *tens of thousands* of threads at
once, grouped into **warps** (bundles of 32 threads that execute the
same instruction in lockstep — SIMT, "single instruction, multiple
threads"). Warps live on **SMs** (streaming multiprocessors — the
GPU's cores, a few dozen to ~100+ per chip), and each SM keeps many
warps *resident* simultaneously.

The point of all that parallelism is latency hiding. A CPU core
hides memory latency *per thread* — out-of-order execution plus big
caches keep one instruction stream busy. A GPU does the opposite:
when a warp stalls on a memory load (~hundreds of cycles), the SM
just issues instructions from a *different* resident warp. Nothing
waits as long as enough warps are resident:

```
 CPU core:  wide OoO, ~5 GHz, caches hide latency PER THREAD
 GPU SM:    32-lane warps (SIMT), latency hidden by OVERSUBSCRIPTION
            — thousands of resident threads; when a warp stalls on
            memory, another issues.
```

**Occupancy** is the fraction of the maximum resident warps you
actually achieve (limited by how many registers and how much
scratch memory each thread uses). Low occupancy = not enough warps
to hide latency = the GPU sits idle exactly like a CPU with a cache
miss. One more SIMT rule: **branch divergence** — when threads of a
warp disagree on an `if`, the warp executes *both* sides with lanes
masked off. That is topic 17's predication done by hardware, and it
is why branchy code is a GPU anti-pattern.

### Step 2 — coalescing and shared memory: the two memory rules

A warp's 32 simultaneous loads become **one** memory transaction if
and only if adjacent lanes touch adjacent addresses — this is
**memory coalescing**. If lane i reads `col[i]` (a dense column),
the hardware fetches one contiguous block; if lane i reads
`rows[i].field` (a strided row layout), it issues up to 32 separate
transactions and throws away most of every one. Coalescing is the
GPU word for topic 12's columnar-layout argument, with a 32×
multiplier attached.

The second rule: each SM has ~100 KB of **shared memory** — a
software-managed scratchpad, as fast as L1 cache but under *your*
control, visible to all threads of one **block** (a group of a few
hundred threads scheduled onto one SM; CUDA also calls it a CTA).
Shared memory is the GPU word for topic 13's cache blocking: stage
a chunk there, work on it repeatedly, write results back once.

Why it matters: every GPU-DB trick in this paper is one of —
coalesce (layout), stay resident (occupancy), amortize atomics
(reduce first), or avoid the bus (Step 3).

### Step 3 — the bus decides everything: regime A vs regime B

A discrete GPU's fast memory (**HBM** — high-bandwidth memory
soldered next to the GPU die, 400–3000 GB/s) is reachable from the
CPU only over PCIe at 16–64 GB/s. That one number splits all
GPU-database papers into two regimes:

```
 regime A: data ships over PCIe per query (coprocessor model)
   GPU time ≈ transfer time; PCIe ~16 GB/s vs CPU membw ~100 GB/s
   → CPU WINS almost always. Full stop.
 regime B: working set resident in GPU HBM (primary-store model)
   HBM ~880 GB/s vs CPU ~100 GB/s
   → GPU wins by ~ the bandwidth ratio (they measure ~16× on SSB)
```

Everything else in the literature is confusion between A and B. The
architectural corollary Crystal works out: GPU as
accelerator-of-operators (ship data per query) fails; GPU as
primary-store-with-CPU-fallback works.

Our gpu_bench's no-crossover table is regime A in miniature — except
on unified memory (Apple Silicon: CPU and GPU share one LPDDR pool,
~150–400 GB/s) the "transfer" is a staging copy + ~1.5 ms dispatch
overhead, and the bandwidth RATIO is ~1, so even regime B wouldn't
save a memory-bound scan on this Mac. Question: what DOES unified
memory save, and which operator class exploits it (arithmetic
intensity — the l2_batch stub)?

### Step 4 — tiles: vectorized execution rebuilt for blocks

Crystal's core idea is to process a query as a sequence of
BLOCK-WIDE functions over **tiles** — a tile is `items per thread ×
threads per block` elements (e.g. 4 × 256 = 1024), loaded from HBM
with coalesced accesses, staged through shared memory, and handed
from one block-wide primitive to the next:

```
 load tile → coalesced, all threads
 BlockPred: each thread evaluates predicate on its items → flags
 BlockScan: prefix-sum flags → output offsets  (compaction!)
 BlockShuffle / BlockAggregate / BlockProbe ...
 write tile → coalesced
```

This is topic 11's vectorized execution with tiles for batches and
shared memory for the L1-resident chunk. The batch size is dictated
by the hardware (threads per block × registers per thread), not
chosen by a tuning knob — same reason topic 11 picked ~1024-row
vectors to fit L1.

### Step 5 — compaction: filters need a prefix scan, not a cursor

A filter's output is smaller than its input, and on a CPU you'd
append survivors with a cursor (`out[k] = x; k += mask`). With
100,000 concurrent threads there is no shared cursor — no total
order exists among the threads, so nobody knows *where* to write.
The fix: an **exclusive prefix scan** (each element gets the sum of
all flags *before* it — which is exactly its output offset)
computed block-wide in shared memory, plus **one** atomic add per
block to claim a range of the global output:

```rust
// tile-based filter: 100K threads share no cursor — the SCAN makes the order
par_for tile in input.tiles(ITEMS_PER_THREAD * THREADS_PER_BLOCK) {
    let items = block_load(tile);                        // coalesced
    let flags = items.map(|x| pred(x) as u32);           // BlockPred
    let (offsets, total) = block_exclusive_scan(flags);  // BlockScan
    let base = atomic_add(&global_cursor, total);        // once per BLOCK
    for i in 0..ITEMS_PER_THREAD {
        if flags[i] == 1 { out[base + offsets[i]] = items[i]; }
    }
}
```

Question: why does GPU filter output need a prefix-scan where the
CPU used a cursor `k += mask`? (No total order across 100K threads
— the scan MAKES one.) The compaction step is topic 17's compress,
built from scan instead of vpcompress — and one atomic per block
instead of per element is the "amortize atomics" rule from Step 2.
Crystal also measures that selection via scan+compact beats
branch-per-thread at mid selectivities — the topic 17 selectivity
curve, GPU edition (Step 1's divergence rule, quantified).

### Step 6 — fusion: one kernel per query, or the bandwidth win evaporates

Each kernel (a GPU function launched over many blocks) reads its
input from HBM and writes its output to HBM. Run a query as five
separate operator kernels and every intermediate result makes a
round trip through HBM — at 880 GB/s that traffic eats the exact
bandwidth advantage you came for. Crystal therefore **fuses** the
whole SSB query into one kernel: tiles flow from primitive to
primitive through shared memory and registers, touching HBM only at
scan and final output. This is topic 11's operator fusion — optional
on a CPU, mandatory here. The cost: fused kernels are monolithic
(one giant kernel per query shape) and kill operator-at-a-time
profiling — see question 4.

### Step 7 — the roofline: one formula predicts the winner

For a scan-shaped operator, execution time is bounded by whichever
resource saturates first:

```
 time = max( bytes / memory_bandwidth , flops / peak_flops )
```

The ratio `flops/byte` is the operator's **arithmetic intensity**,
and it decides which term wins. GPU wins iff data is resident
(Step 3) AND the op is bandwidth-bound (ratio ~9×) or compute-bound
with high intensity (ratio can be ~50×). Neither holds for
ship-per-query. Question: place these on the roofline: sum
(0.25 FLOP/byte), filter (0.25), hash probe (~1 + random access),
l2 dim=128 (~32), CAGRA search (~high + irregular). Which two belong
on a GPU at all?

## How to read the paper (with the concepts in hand)

- **§2–3 — the tile model.** Steps 4–5 in the authors' words: the
  block-wide primitives, the shared-memory staging, and the fused
  SSB queries (Step 6). Map each primitive to its topic 11/17 CPU
  ancestor as you go.
- **§5–6 — the two-regime measurements.** Step 3 quantified: the
  ~16× regime-B win on SSB, and the transfer-inclusive numbers that
  kill regime A. This is the go/no-go table for M18.
- **CPU-baseline honesty** — read this discussion even if you never
  touch a GPU: their CPU code is AVX-vectorized and multi-threaded;
  most prior "100× GPU speedups" compared against scalar
  single-thread CPU code (topic 0's fair-benchmarking paper, case
  study #1). Any speedup claim you publish for M18 gets held to this
  standard.

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
   filter (streaming), distance scoring (dense). Apply Step 7's
   roofline to each and write the one-line go/no-go.

## References

**Papers**
- Shanbhag, Madden, Yu — "A Study of the Fundamental Performance
  Characteristics of GPUs and CPUs for Database Analytics"
  (SIGMOD 2020) — §2-3 for the tile model, §5-6 for the two-regime
  measurements; the CPU-baseline-honesty discussion is worth reading
  even if you never touch a GPU
