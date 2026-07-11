# Reading guide — YCSB (SoCC 2010) + go-ycsb generators

Cooper et al.'s "Benchmarking Cloud Serving Systems with YCSB" — the
paper that standardized KV benchmarking. Read alongside
[`~/repos/go-ycsb`](https://github.com/pingcap/go-ycsb) (the Go port; structure mirrors the Java original).

## The design: workloads = mix × distribution

```
        op mix (what)              key distribution (where)
  A 50r/50u  update-heavy          uniform     — every key equal
  B 95r/5u   read-mostly           zipfian .99 — hot head, θ=0.99
  C 100r     read-only             latest      — zipf over newest
  D 95r/5i   read-latest           scrambled   — zipf rank, fnv-
  E 95scan/5i short ranges                       hashed into space
  F 50r/50rmw read-modify-write    hotspot     — x% ops on y% keys
```

Property files: `workloads/workloada:31-36` (proportions +
`requestdistribution`). The genius is the factoring — 6 mixes × 5
distributions covers most serving systems' realities.

## The Zipfian generator (`pkg/generator/zipfian.go`)

The most-copied benchmark code in existence — our `zipf.rs` stub:

| anchor | what |
|---|---|
| :43 | `ZipfianConstant = 0.99` — why every paper says θ=0.99 |
| :92-118 | constructor: `zetan` (harmonic-ish sum, O(n)!), `eta`, `alpha = 1/(1-θ)` |
| :125-132 | `zetaStatic` — the O(n) sum; incremental recompute when item count GROWS (:135-147), full recompute (slow, warned) when it shrinks |
| :150-163 | the sampler: two fast paths (`uz < 1` → rank 0, `< 1+0.5^θ` → rank 1), else `n·(ηu − η + 1)^α` |
| `scrambled_zipfian.go` | fnv64(rank) % n — same skew, scattered hot keys |

Why scrambling matters: plain zipfian's hot keys are ids 0,1,2,… —
adjacent, so they share cache lines/pages/shards, and you accidentally
benchmark spatial locality instead of skew. Scrambled spreads them.
(Our test pins this: hottest key must not be id 0.)

## What YCSB gets wrong (know before citing)

- **Coordinated omission** (Tene): the closed-loop driver stops
  sending while an op stalls, so recorded latencies MISS the queueing
  a real open-loop client would see. p999 under load is fiction
  unless you use a target rate + intended-start-time correction.
- **Zeta is O(n) at startup** — at 1B keys the constructor takes
  minutes; ports cache zetan constants for common sizes.
- **No transactions, no scans-with-filter, values are blobs** — it
  benchmarks the KV layer only (fine for M22's graph micro-benches,
  wrong for MVCC claims).

## Questions (answer in notes.md)

1. Derive why P(rank 0) = 1/ζ(n,θ). Then: at n=1M, θ=0.99, what
   fraction of ops hit the top 100 keys? (Compute, then verify with
   the stub.)
2. Why do the two fast paths in `next()` exist — what fraction of
   draws do they absorb at θ=0.99?
3. Predict uniform → zipfian effect per workload on OUR BTreeMap
   store: A-F, which speeds UP (cache-hot head) and which barely
   moves (E's scans)? Fill the prediction table before implementing.
4. Coordinated omission: our driver records service time. Sketch the
   fix (intended arrival times at a target rate) and what p999 would
   show for workload E.
5. Workload D's "latest" distribution: why is passing a plain
   zipfian to a growing keyspace subtly wrong (hint: zetan
   staleness, go-ycsb :135)?
