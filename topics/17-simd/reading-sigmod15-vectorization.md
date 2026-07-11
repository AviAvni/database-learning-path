# Reading guide — "Rethinking SIMD Vectorization for In-Memory Databases" (Polychroniou, Raghavan, Ross — SIGMOD '15)

The paper that turned "SIMD for databases" from folklore into a
catalog. It vectorizes the FOUR fundamental operators — selection
scan, hash probe, bloom filter, partition — and shows each is a
composition of two primitives: **selective store** (compress) and
**selective load / gather**. Read it as the spec for our
`experiments/filter.rs` and for M17's engine kernels.

## The two primitives

```
 selective STORE (compress):        selective LOAD (expand/gather):
 lanes:  a b c d e f g h            memory: p q r s ...
 mask:   1 0 1 1 0 0 1 0            mask:   1 0 1 1 ...
 memory: a c d g ────────►          lanes:  p . q r ... ◄────────
 (filter output, partition out)     (refill lanes after some finish)

 gather:  lanes = mem[idx[0..W]]    (hash probe, dictionary decode)
 scatter: mem[idx[0..W]] = lanes    (partition, hash build)
```

Every operator in the paper is a loop of: compute masks → compress
finished lanes out → refill from input. AVX-512 has all four as
instructions; NEON has none natively (hence simdjson's LUT compress
and the gather cost model below).

## 1. Selection scan (§4) — our filter.rs, their Figure

Three implementations, one selectivity sweep:

```
 branchy        │ ns/elem peaks at ~50% sel (mispredict wall)
 branchless     │ flat line — always-store, data-independent
 SIMD compress  │ flat, lower — W elems per compress
                └──────────────── selectivity →
 crossover: branchy wins BELOW ~few % and ABOVE ~95%
```

The paper's addition our README didn't have: with SIMD you compute
the mask for W lanes and use it to compress-store BOTH the values
and their RIDs (row ids) — the output of a filter in a real engine
is positions, not just values (topic 11's selection vectors).
Question: does compressing (value,rid) pairs double the cost or can
one mask drive two compresses?

## 2. Hash probe (§5) — vertical vectorization

The naive way vectorizes ONE probe's steps. The paper's way runs W
INDEPENDENT probes, one per lane:

```
 keys   = selective_load(input, done_mask)   ← refill finished lanes
 hashes = hash(keys)
 slots  = gather(table, hashes)
 done   = (slots.key == keys) | (slots == EMPTY)
 output = selective_store(matches)
 bucket += 1 where !done                     ← collided lanes probe on
```

Lanes finish at different times — the done-mask + refill pattern
keeps all W lanes busy despite divergent probe lengths. This is
hashbrown's group probing turned 90°: hashbrown = SIMD *within* one
probe, SIGMOD15 = SIMD *across* probes. Question: which does M11's
hash join want, given batch sizes of 1024 and a table that misses
L2?

## 3. The gather cost model (§3)

Measured then, still true now: a gather costs ~1 cache access PER
LANE — it parallelizes the instruction stream, not the memory
system. Gather wins only when the computation around it vectorizes;
it never fixes topic 13's pointer-chasing tax. Corollary the paper
proves: vectorized probe ≈ scalar probe when the table exceeds
cache, but is 3-6× faster in-cache. Question: FalkorDB's adjacency
lookups are gathers over CSR — in-cache or out? What does that
predict for SIMD-izing traversals (M24)?

## 4. Partition (§6) — scatter with conflict detection

Radix partition scatters each lane to `out[hist[digit(k)]++]`. Two
lanes with the SAME digit collide on the histogram slot. The paper
detects conflicts (AVX-512 `vpconflictd`; emulated before that) and
serializes only colliding lanes. Question: NEON has no conflict
detect either — sketch the scalar-fallback-inside-vector-loop shape,
and note where topic 13's software write-combining buffers make the
scatter moot.

## 5. What to steal for the experiments

- The selectivity sweep axes (their Fig: cycles/tuple vs sel%) =
  `simd_bench`'s filter output. Plot branchy/branchless/compress.
- Rigged input trick: they control selectivity EXACTLY by
  construction — our bench does the same (threshold = quantile).
- Report cycles/tuple, not GB/s, for the probe kernel (memory-bound
  kernels hide instruction wins behind bandwidth).

## Questions for notes.md

1. Why does branchless lose to branchy at 99% selectivity in their
   data (hint: store traffic — branchless writes EVERY element)?
2. Vertical probing needs W independent probes in flight. What does
   that do to the ORDER of join output, and which downstream
   operators care (topic 11's sort-sensitivity)?
3. Their bloom-filter kernel is probe-minus-refill — why do bloom
   lookups vectorize even better than hash probes (fixed iteration
   count)?
4. The paper predates AVX-512 on servers; they emulate compress via
   permutation LUTs — exactly simdjson's arm64 trick. Compare table
   sizes: 8-lane f32 LUT vs simdjson's 8-byte LUT.
5. For M17: rank the four operators by expected engine-level win in
   our Cypher pipeline (filter, probe, partition, bloom) given M11's
   profile — where does Amdahl bite first?
