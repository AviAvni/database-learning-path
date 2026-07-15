# YCSB: six mixes, five distributions, one Zipfian generator

Cooper et al.'s SoCC 2010 paper standardized KV benchmarking by
factoring a workload into an operation mix times a key
distribution — and its θ=0.99 Zipfian generator is the skew behind
nearly every KV paper since (our `zipf.rs` stub reimplements it
from the go-ycsb port). Before pointing you at the paper and the
generator source, this chapter builds the ideas one at a time: the
factoring, Zipf's law, how you actually *sample* from it in O(1),
why the hot keys must be scattered, and the trap (coordinated
omission) to know before citing any YCSB number.

## The problem in one sentence

Two serving systems can each claim "1M ops/s" while one was measured
on uniform reads and the other on a 50%-write workload where 10% of
the keys absorb most of the traffic — YCSB exists so that a KV
benchmark names its operation mix and its key distribution, the two
knobs that change the number by 4× (our own A–F runs span 1.11 to
4.40 Mops/s on the same store).

## The concepts, step by step

### Step 1 — the factoring: workload = op mix × key distribution

A serving workload (the traffic a key-value store — a database
exposing only get/put/scan on keys — actually sees) is characterized
by two independent choices: *what* operations arrive (the **mix**)
and *which* keys they touch (the **distribution**). YCSB's core
contribution is treating these as orthogonal axes:

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
distributions covers most serving systems' realities, and any
published "workload B, zipfian" is exactly reproducible. Our
measured spread on one BTreeMap store (uniform keys): A 2.88, B
4.15, C 3.72, D 4.40, E 1.11, F 2.85 Mops/s — the *mix alone* is a
4× lever before skew even enters.

### Step 2 — Zipf's law: what θ=0.99 means

A **Zipfian distribution** models popularity skew: the k-th most
popular key is requested with probability proportional to `1/k^θ`,
where θ (theta) tunes how brutal the skew is — θ=0 is uniform,
θ→1 concentrates traffic in a tiny head. Real access logs (web
pages, videos, social profiles) fit this shape, which is why YCSB
defaults to it.

The probabilities must sum to 1, so each is divided by the
normalizing constant **zetan** = Σ 1/i^θ over all n keys (the
generalized harmonic sum). Concretely at θ=0.99, n=1M: the single
hottest key gets ~7% of all requests, and a few hundred keys absorb
the majority of traffic. YCSB pinned θ=**0.99** — just under 1,
where the math stays finite-friendly — and because everyone copied
the generator, "zipfian 0.99" is now the de-facto meaning of
"skewed" in every KV paper. Cost of ignoring it: a cache-friendly
hot head makes skewed reads *faster* than uniform, so
uniform-vs-zipfian is not a fairness detail, it's the experiment.

### Step 3 — sampling in O(1): the inverse-CDF trick

Drawing a Zipf sample naively means walking the cumulative
probabilities until they exceed a random `u` — O(n) per draw.
YCSB's generator instead inverts the cumulative distribution
analytically (an **inverse-CDF** sampler: map uniform `u ∈ [0,1)`
straight to a rank with one `pow()`), paying the O(n) cost once,
at construction, to compute zetan. The most-copied benchmark code
in existence — our `zipf.rs` stub — lives in
`pkg/generator/zipfian.go`:

| anchor | what |
|---|---|
| :43 | `ZipfianConstant = 0.99` — why every paper says θ=0.99 |
| :92-118 | constructor: `zetan` (harmonic-ish sum, O(n)!), `eta`, `alpha = 1/(1-θ)` |
| :125-132 | `zetaStatic` — the O(n) sum; incremental recompute when item count GROWS (:135-147), full recompute (slow, warned) when it shrinks |
| :150-163 | the sampler: two fast paths (`uz < 1` → rank 0, `< 1+0.5^θ` → rank 1), else `n·(ηu − η + 1)^α` |
| `scrambled_zipfian.go` | fnv64(rank) % n — same skew, scattered hot keys |

The sampler, transcribed (zipfian.go:150-163):

```rust
fn next(&mut self, rng: &mut Rng) -> u64 {
    let u = rng.f64();
    let uz = u * self.zetan;             // zetan = Σ 1/i^θ — O(n), computed ONCE
    if uz < 1.0 { return 0; }            // fast path: THE hottest key
    if uz < 1.0 + 0.5f64.powf(self.theta) { return 1; }
    // general case: inverse-CDF approximation, rank from one pow()
    let rank = (self.n as f64
        * (self.eta * u - self.eta + 1.0).powf(self.alpha)) as u64;
    rank                                  // alpha = 1/(1-θ)
}
```

The two fast paths exist because ranks 0 and 1 are so hot they're
worth special-casing before the `pow()` (question 2 asks what
fraction of draws they absorb). The costs to notice: zetan is
**O(n) at startup** — at 1B keys the constructor takes minutes, so
ports cache zetan constants for common sizes — and a *growing*
keyspace (workload D) makes zetan stale (question 5).

### Step 4 — scrambling: same skew, scattered hot keys

Plain Zipfian's hot keys are ranks 0, 1, 2, … — which the generator
returns *as key ids*, so the hottest keys are **adjacent**. Adjacent
hot keys share cache lines, pages, and shards, and suddenly you're
benchmarking spatial locality instead of skew. The fix is one hash:

```rust
fn next_scrambled(&mut self, rng: &mut Rng) -> u64 {
    fnv64(self.next(rng)) % self.n       // same skew, hot keys NOT ids 0,1,2…
}
```

Rank → fnv64 hash → key id: the popularity *distribution* is
unchanged, but the hot keys land anywhere in the keyspace. This is
YCSB's default "zipfian". (Our test pins the property: the hottest
key must not be id 0.)

### Step 5 — coordinated omission: the closed-loop lie

YCSB's driver is **closed-loop**: each client thread issues an
operation, waits for it to finish, then issues the next. When one
operation stalls for 100 ms, the thread sends *nothing* during the
stall — so the stall appears **once** in the histogram, while a
real open-loop client (requests arriving on a schedule, regardless
of responses) would have had dozens of requests queue up behind it,
each experiencing most of that 100 ms. Tene named this
**coordinated omission** (topic 0): the benchmark and the system
coordinate to omit the worst latencies. Recorded p999 under load is
fiction unless you use a target rate plus intended-start-time
correction (measure each op from when it *should* have started).
Our driver records service time only — question 4 asks you to
sketch the fix.

### Step 6 — what YCSB deliberately doesn't test

YCSB benchmarks the KV layer and nothing above it: **no
transactions** (no operation spans two keys), **no
scans-with-filter**, and values are opaque blobs (no schema, no
secondary indexes). That's fine for M22's graph micro-benches — a
graph engine's adjacency reads are close to KV reads — and wrong
for any MVCC/isolation claim (topic 8's territory; see TPC-C in
reading-oltpbench-tpcc.md for contention that spans keys). Citing
YCSB for a transactional system measures its cheapest path.

## How to read the paper (with the concepts in hand)

SoCC 2010, ~12 pages; the design half aged well, the eval half didn't:

- **§1–2** Motivation (cloud serving stores circa 2010) — skim;
  the system list (Cassandra, HBase, PNUTS, "sharded MySQL") is a
  time capsule.
- **§3–4 — read carefully.** The workload-design sections: the
  mix × distribution factoring (Step 1), the distribution zoo
  (Step 2), and the tiered "performance vs scaling" methodology.
  This is the reusable part.
- **§5–6** The 2010 measurements — skim for method, ignore the
  numbers; every system in them has been rewritten since.
- Then read the generator source with Step 3's anchor table:
  constructor first (zetan, eta, alpha), then the sampler's two
  fast paths, then `scrambled_zipfian.go` (Step 4). It's ~200
  lines total.
- Nothing in the paper covers coordinated omission (Step 5) — it
  predates Tene's talk; carry that critique with you.

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

## References

**Papers**
- Cooper, Silberstein, Tam, Ramakrishnan, Sears — "Benchmarking
  Cloud Serving Systems with YCSB" (SoCC 2010) — §3-4 (the
  mix×distribution factoring); the eval section is dated

**Code**
- [go-ycsb](https://github.com/pingcap/go-ycsb)
  `pkg/generator/zipfian.go`, `scrambled_zipfian.go`,
  `workloads/workloada` — the Go port; structure mirrors the Java
  original
