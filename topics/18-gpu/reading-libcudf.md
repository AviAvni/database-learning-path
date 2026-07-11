# Reading guide — libcudf (GPU columnar operators)

Clone: [`~/repos/cudf`](https://github.com/rapidsai/cudf) (`cpp/src/`). RAPIDS' GPU DataFrame engine —
Arrow-layout columns (topic 12) with every operator rewritten under
GPU constraints: no resizable output, atomics that must be amortized,
and a memory hierarchy you manage by hand.

## Anchor map

| anchor | what it is |
|---|---|
| src/join/hash_join/ | size/retrieve split: `inner_join_size.cu` THEN `inner_join_retrieve.cu` |
| src/join/distinct_hash_join.cu | cuco-based build + cooperative-groups probe |
| src/join/hash_join/kernels_common.cuh | the probe kernel shapes |
| src/groupby/hash/compute_shared_memory_aggs.cu | per-block shared-mem aggregation + spill test |
| src/groupby/hash/compute_global_memory_aggs.cu | the global-atomics fallback |
| src/groupby/hash/compute_mapping_indices.cu | key → group index pass |
| src/join/conditional_join.cu | non-equi joins: nested loop, AST predicate on device |
| src/join/jit/ | JIT'd join predicates (topic 19 preview) |
| src/bitmask/ | validity bitmaps as first-class kernels (topic 11's null masks) |

## 1. The two-phase everything (size → retrieve)

GPU kernels can't `push`. Every variable-output operator runs twice:

```
 pass 1 (size):     each thread COUNTS its matches → total via reduce
 allocate exactly total
 pass 2 (retrieve): same probe again, write via computed offsets
```

`inner_join_size.cu` and `inner_join_retrieve.cu` are literally the
same probe loop with different epilogues. Alternatives they could
have used and didn't: atomic global cursor (contended), max-size
over-allocation (memory). Question: pass 2 recomputes all of pass
1's probes — why is recompute cheaper than remembering (HBM
bandwidth vs materializing per-thread match lists)? Compare
simdjson's over-write-under-advance: same problem, opposite answer —
why?

## 2. Cooperative-groups probing (distinct_hash_join.cu)

A single thread probing a hash table = one uncoalesced load per
step. cudf (via cuco) probes with a COOPERATIVE GROUP: a warp
fragment (e.g. 4-8 threads) loads a whole bucket window in one
coalesced transaction, ballot-votes on matches, and the group
advances together.

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

## 3. Group-by: shared memory until it spills

`compute_shared_memory_aggs.cu` sizes per-block scratch for the
output columns and BAILS to `compute_global_memory_aggs.cu` (global
atomics) when they don't fit (~few hundred groups × columns). Two
levels of the same aggregation = topic 11's partial/final split,
imposed by the ~100 KB shared-memory budget instead of by threads.
Question: high-cardinality group-by (1M groups) — neither fits.
What's the classical answer (partition by group hash first — topic
13's radix partition, now for occupancy)?

## 4. What Arrow layout buys on GPU

Columns are dense arrays + validity bitmaps — loads coalesce by
construction; nulls process as bitmask kernels (src/bitmask/) not
branches. A row-store on GPU would strand 31/32 of every
transaction. Topic 12's layout argument, with a 32× multiplier.
Question: strings. Arrow offsets+bytes means variable work per
element — find how cudf balances it (warp-per-string vs
thread-per-char kernels in src/strings/) and relate to Gunrock's
ragged-frontier problem.

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
