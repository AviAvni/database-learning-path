# SIMD for databases: two primitives, four operators

Polychroniou, Raghavan & Ross's SIGMOD '15 paper turned "SIMD for
databases" from folklore into a catalog. It vectorizes the FOUR
fundamental operators — selection scan, hash probe, bloom filter,
partition — and shows each is a composition of two primitives:
**selective store** (compress) and **selective load / gather**.
Before the paper, this chapter builds those primitives and each
operator's shape step by step. Read it as the spec for our
`experiments/filter.rs` and for M17's engine kernels.

## The problem in one sentence

Database operators branch on data (does this row pass? did this
probe hit?), and a mispredicted branch costs ~15 cycles — the paper
shows that recasting the four core operators as branch-free lane
operations wins up to 3–6× in cache, and tells you exactly when it
doesn't (out-of-cache gathers).

## The concepts, step by step

### Step 1 — lanes, masks, and the operator question

SIMD (single instruction, multiple data — one instruction operating
on a vector of W values, its **lanes**; W=4 f32s on 128-bit NEON,
16 on 512-bit AVX-512) is trivial for `a[i] + b[i]`. Database
operators are harder because each lane wants to *do something
different*: keep this row, drop that one, probe another bucket. The
paper's framing: express every such divergence as a **mask** (a
bitmap with one bit per lane, produced by a vector comparison) and
find instructions that consume masks instead of branching on them.
The whole catalog reduces to two such instructions.

### Step 2 — the two primitives: selective store and selective load/gather

**Selective store** (also "compress") writes only the masked lanes
to memory, packed contiguously. **Selective load** ("expand") fills
only the masked lanes from memory, leaving the rest untouched. Add
their indexed cousins — **gather** (lanes = `mem[idx[i]]`, W loads
from arbitrary places) and **scatter** (the reverse) — and every
operator in the paper is a loop of: compute masks → compress
finished lanes out → refill from input:

```
 selective STORE (compress):        selective LOAD (expand/gather):
 lanes:  a b c d e f g h            memory: p q r s ...
 mask:   1 0 1 1 0 0 1 0            mask:   1 0 1 1 ...
 memory: a c d g ────────►          lanes:  p . q r ... ◄────────
 (filter output, partition out)     (refill lanes after some finish)

 gather:  lanes = mem[idx[0..W]]    (hash probe, dictionary decode)
 scatter: mem[idx[0..W]] = lanes    (partition, hash build)
```

AVX-512 has all four as instructions; NEON has none natively (hence
simdjson's LUT compress and the gather cost model below). That gap
is why the paper (written pre-AVX-512) emulates compress via
permutation LUTs — exactly the trick your NEON kernels will use.

### Step 3 — selection scan (§4): three shapes, one selectivity sweep

The filter kernel — keep elements passing a predicate — comes in
three shapes, and **selectivity** (the fraction of elements kept)
decides the winner. Branchy code mispredicts worst at 50%
selectivity (the branch is a coin flip, ~15 cycles per miss);
branchless always-store code is flat; compress does W elements per
instruction:

```
 branchy        │ ns/elem peaks at ~50% sel (mispredict wall)
 branchless     │ flat line — always-store, data-independent
 SIMD compress  │ flat, lower — W elems per compress
                └──────────────── selectivity →
 crossover: branchy wins BELOW ~few % and ABOVE ~95%
```

The paper's addition our README didn't have: with SIMD you compute
the mask for W lanes and use it to compress-store BOTH the values
and their RIDs (row ids — the positions of surviving rows, which is
what a real engine's filter actually outputs; topic 11's selection
vectors). Question: does compressing (value,rid) pairs double the
cost or can one mask drive two compresses?

### Step 4 — hash probe (§5): W independent probes, refilled as they finish

The naive way to vectorize a hash-table probe vectorizes ONE probe's
steps (horizontal). The paper's way is **vertical vectorization**:
run W *independent* probes, one per lane. The complication is that
probes finish at different times (some hit immediately, some
collide and probe on) — solved by the done-mask + refill pattern,
built entirely from Step 2's primitives:

```
 keys   = selective_load(input, done_mask)   ← refill finished lanes
 hashes = hash(keys)
 slots  = gather(table, hashes)
 done   = (slots.key == keys) | (slots == EMPTY)
 output = selective_store(matches)
 bucket += 1 where !done                     ← collided lanes probe on
```

```rust
// vertical probing: W INDEPENDENT probes in flight, refilled as they finish
loop {
    keys = selective_load(keys, input, done);  // finished lanes take new keys
    let slot = gather(table, hash(keys) + bucket);  // ~1 cache access PER LANE
    let hit   = slot.key.simd_eq(keys);
    let empty = slot.simd_eq(EMPTY);
    selective_store(out, hit, slot.val);       // compress matched lanes out
    done   = hit | empty;
    bucket = done.select(ZERO, bucket + 1);    // collided lanes probe on
}
```

This is hashbrown's group probing turned 90°: hashbrown = SIMD
*within* one probe (compare 16 control bytes of one bucket at once),
SIGMOD15 = SIMD *across* probes (W separate lookups in flight).
Question: which does M11's hash join want, given batch sizes of 1024
and a table that misses L2?

### Step 5 — the gather cost model (§3): parallel instructions, serial memory

The paper's most durable measurement: a gather costs ~1 cache access
PER LANE — the instruction is one opcode, but the memory system
still performs W independent loads. Gather parallelizes the
*instruction stream*, not the *memory system*. Consequences: gather
wins only when the computation around it vectorizes; it never fixes
topic 13's pointer-chasing tax. Corollary the paper proves:
vectorized probe ≈ scalar probe when the table exceeds cache, but is
3–6× faster in-cache. Question: FalkorDB's adjacency lookups are
gathers over CSR — in-cache or out? What does that predict for
SIMD-izing traversals (M24)?

### Step 6 — partition (§6): scatter needs conflict detection

Radix partition (distributing rows into buckets by some digits of
their key) scatters each lane to `out[hist[digit(k)]++]`. New
hazard: two lanes in the SAME vector with the same digit collide on
the histogram slot — both would read the same counter, write the
same address, and lose one row. The paper detects conflicts
(AVX-512 `vpconflictd`; emulated before that) and serializes only
the colliding lanes. Question: NEON has no conflict detect either —
sketch the scalar-fallback-inside-vector-loop shape, and note where
topic 13's software write-combining buffers make the scatter moot.

### Step 7 — what to steal for the experiments

- The selectivity sweep axes (their Fig: cycles/tuple vs sel%) =
  `simd_bench`'s filter output. Plot branchy/branchless/compress.
- Rigged input trick: they control selectivity EXACTLY by
  construction — our bench does the same (threshold = quantile).
- Report cycles/tuple, not GB/s, for the probe kernel (memory-bound
  kernels hide instruction wins behind bandwidth).

## How to read the paper (with the concepts in hand)

- **§3 — read carefully.** The gather cost model (Step 5) is the
  section that ages best; everything else depends on whether you
  believe it.
- **§4 (selection)** — Step 3; compare their crossover points with
  the ones your `simd_bench` selectivity sweep produces.
- **§5 (hash probe)** — Step 4; keep the vertical-vs-horizontal
  contrast (SIGMOD15 vs hashbrown) in mind throughout.
- **§6 (partition)** — Step 6; skim the conflict-detection emulation
  unless you're implementing it.
- The bloom-filter kernel is probe-minus-refill — read it as a
  simplified §5.
- Skim the AVX-512 forecast knowing it came true: the "future
  hardware" they emulate with LUTs is now `vpcompressd` in polars
  (reading-polars-compute.md).

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

## References

**Papers**
- Polychroniou, Raghavan, Ross — "Rethinking SIMD Vectorization for
  In-Memory Databases" (SIGMOD 2015) — §3 the gather cost model,
  §4 selection, §5 probe, §6 partition; skim the AVX-512 forecast
  knowing it came true
