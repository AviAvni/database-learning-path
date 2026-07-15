# libcudf: GPU kernels can't push

RAPIDS' GPU DataFrame engine — Arrow-layout columns (topic 12) with
every operator rewritten under GPU constraints: no resizable output,
atomics that must be amortized, and a memory hierarchy you manage by
hand. This chapter builds those constraints one at a time — why a
GPU kernel can't `push`, the two-phase pattern that replaces it, how
a warp probes a hash table together, and where aggregation spills —
then maps each idea to the exact `.cu` file that implements it. The
two-phase size/retrieve pattern and cooperative-group probing here
are the idioms every GPU-DB operator ends up using.

## The problem in one sentence

A join's output size is unknown until you compute it, but a GPU
kernel's output buffer must be allocated *before* launch and shared
by ~100,000 threads with no `Vec::push` and no cheap lock — so every
variable-output operator needs a plan for where each thread writes.

## The concepts, step by step

### Step 1 — the executor: warps, coalescing, and the no-push rule

A GPU runs kernels (device functions) over tens of thousands of
threads grouped in **warps** — bundles of 32 threads executing in
lockstep — and a warp's 32 loads become **one** memory transaction
iff adjacent threads touch adjacent addresses (**coalescing**).
Each thread block also owns ~100 KB of **shared memory** (a
software-managed scratchpad as fast as L1). Three consequences
shape every cudf operator:

- **Output must be pre-sized.** There is no allocator you'd want to
  call from 100K threads mid-kernel, and no `Vec::push`: every
  output needs its size known up front or an atomic cursor.
- **Atomics must be amortized.** A global atomic per element from
  100K threads serializes on the contended word; the idiom is
  reduce-locally-first, one atomic per block.
- **Layout is destiny.** A row-store strands 31/32 of every warp
  transaction; dense columns coalesce by construction (Step 5).

Why it matters: Steps 2–4 are the three standard escapes from these
constraints, and they recur in every GPU database ever written.

### Step 2 — two-phase everything: size, then retrieve

When output size is unknown, run the kernel **twice**: pass 1
computes only *how many* results each thread produces, a prefix scan
(running total of the per-thread counts — each thread's total-before
is exactly its write offset) turns counts into exact write
positions, the host allocates exactly, and pass 2 re-runs the same
probe and writes through the computed offsets:

```
 pass 1 (size):     each thread COUNTS its matches → total via reduce
 allocate exactly total
 pass 2 (retrieve): same probe again, write via computed offsets
```

`inner_join_size.cu` and `inner_join_retrieve.cu` are literally the
same probe loop with different epilogues:

```rust
// pass 1: the probe loop with a COUNTING epilogue
par_for i in 0..n_probe {
    count[thread_id] += table.matches(keys[i]);
}
let offsets = exclusive_scan(count);      // per-thread write positions
let out = alloc_exact(offsets.total());   // GPU output must be pre-sized

// pass 2: the SAME probe loop with a WRITING epilogue
par_for i in 0..n_probe {
    for m in table.probe(keys[i]) {       // recompute beats remembering:
        out[offsets[thread_id]] = (i, m); // HBM traffic to materialize
        offsets[thread_id] += 1;          // match lists costs more than
    }                                     // probing the table twice
}
```

The cost is doubled probe work; the payoff is zero contention and
zero over-allocation. Alternatives they could have used and didn't:
atomic global cursor (contended), max-size over-allocation
(memory). Question: pass 2 recomputes all of pass 1's probes — why
is recompute cheaper than remembering (HBM bandwidth vs
materializing per-thread match lists)? Compare simdjson's
over-write-under-advance: same problem, opposite answer — why?

### Step 3 — cooperative-groups probing: the warp is the vector register

A hash-table probe chases a random slot, then maybe the next slot,
and so on — one uncoalesced load per step if each thread probes
alone. cudf (via **cuco**, RAPIDS' GPU hash-table library) probes
with a **cooperative group**: a warp fragment of 4–8 threads loads a
whole window of adjacent buckets in one coalesced transaction, votes
on matches with a **ballot** (a warp instruction producing a bitmask
of which lanes matched), and advances together:

```
 thread-per-probe:  t0→slot17, t1→slot93, t2→slot4   (3 transactions)
 group-per-probe:   t0..t3 → slots 17,18,19,20        (1 transaction,
                    ballot → who matched)              hashbrown Group
                                                       at warp scale!)
```

This is EXACTLY topic 17's SwissTable `match_tag` — 16 control
bytes per `vceq` — with the warp playing the vector register.
Question: hashbrown shrank its NEON group to 8B; what's the
analogous tuning knob in cuco (window size vs probe length)?

### Step 4 — group-by: aggregate in shared memory until it spills

Aggregation wants one accumulator per group, updated by every
thread — a contention magnet. cudf's answer is two tiers:
`compute_shared_memory_aggs.cu` sizes per-block scratch for the
output columns and aggregates there (fast, block-local atomics),
then merges blocks' partials; when groups × columns don't fit in
the ~100 KB shared-memory budget (~a few hundred groups), it BAILS
to `compute_global_memory_aggs.cu` — atomics straight into global
memory. Two levels of the same aggregation = topic 11's
partial/final split, imposed by the memory hierarchy instead of by
threads. Question: high-cardinality group-by (1M groups) — neither
tier fits. What's the classical answer (partition by group hash
first — topic 13's radix partition, now for occupancy)?

### Step 5 — Arrow layout: coalescing and null-handling by construction

cudf columns are Arrow-format: a dense value array plus a
**validity bitmap** (one bit per row marking null/not-null). Dense
arrays mean warp loads coalesce by construction — a row-store on
GPU would strand 31/32 of every 128-byte transaction, topic 12's
layout argument with a 32× multiplier. Nulls process as bitmask
kernels (`src/bitmask/`), not per-row branches — branches diverge
warps; bit-ops don't. Question: strings. Arrow offsets+bytes means
variable work per element — find how cudf balances it
(warp-per-string vs thread-per-char kernels in src/strings/) and
relate to Gunrock's ragged-frontier problem.

### Step 6 — when the pattern doesn't fit: conditional joins and JIT'd predicates

Non-equi joins (`a.x < b.y`) can't hash — there is no key to hash
on — so `conditional_join.cu` falls back to a nested loop with the
predicate shipped to the device as an AST it interprets per pair
(same reason topic 10's planner keeps NL join). And because
interpreting an AST per pair is this topic's cardinal sin,
`src/join/jit/` JIT-compiles the predicate into the kernel at
runtime — a preview of topic 19: shader/kernel specialization *is*
query compilation.

## Where each step lives in the code

| anchor | what it is | step |
|---|---|---|
| src/join/hash_join/ | size/retrieve split: `inner_join_size.cu` THEN `inner_join_retrieve.cu` | 2 |
| src/join/hash_join/kernels_common.cuh | the probe kernel shapes | 2–3 |
| src/join/distinct_hash_join.cu | cuco-based build + cooperative-groups probe | 3 |
| src/groupby/hash/compute_shared_memory_aggs.cu | per-block shared-mem aggregation + spill test | 4 |
| src/groupby/hash/compute_global_memory_aggs.cu | the global-atomics fallback | 4 |
| src/groupby/hash/compute_mapping_indices.cu | key → group index pass | 4 |
| src/bitmask/ | validity bitmaps as first-class kernels (topic 11's null masks) | 5 |
| src/join/conditional_join.cu | non-equi joins: nested loop, AST predicate on device | 6 |
| src/join/jit/ | JIT'd join predicates (topic 19 preview) | 6 |

Reading order: the size/retrieve pair first (diff the two files —
the epilogues are the whole difference), then
`distinct_hash_join.cu` for the cooperative-group probe, then the
`groupby/hash/` trio, and `bitmask/`/`conditional_join.cu` as the
questions demand.

## Questions for notes.md

1. Count kernel launches for one `inner_join`: build + size +
   retrieve (+ mapping). At ~1.5 ms dispatch overhead each (our
   measured floor on Metal), what's the minimum batch that
   amortizes four launches?
2. The size/retrieve recompute doubles probe FLOPs. On the Crystal
   roofline, when is that free (probe is bandwidth-bound; second
   pass hits the same cache lines... does HBM have a "cache" that
   helps — L2)?
3. Why does conditional_join fall back to nested-loop + device AST
   instead of hashing (non-equi predicates can't hash — same reason
   topic 10's planner keeps NL join)?
4. cudf JIT-compiles join predicates (src/join/jit/) at runtime.
   What's the WGSL analogue for our engine (naga compiles WGSL
   strings at pipeline creation — shader specialization = topic 19's
   query compilation)?
5. For M18: our filter_count stub's one-atomic-per-workgroup is
   pass-1-only of the cudf pattern. Sketch the pass-2 (compact
   values, not count) using a workgroup prefix scan — Crystal's
   BlockScan.

## References

**Code**
- [cudf](https://github.com/rapidsai/cudf) — `cpp/src/` — the anchor
  map above: `join/hash_join/` for size/retrieve,
  `join/distinct_hash_join.cu` for cooperative-groups probing,
  `groupby/hash/` for the shared-vs-global aggregation split,
  `bitmask/` for validity-mask kernels
