# DiskANN: one SSD read per hop

The paper that put billion-point ANN on SSDs without giving up
recall — topics 3/4's disk-layout discipline applied to graphs.
Before the paper, this chapter builds its three ideas one at a time:
why HNSW can't just be paged to disk, a flat graph built for
provably few hops (Vamana's α-slack pruning), a block layout that
co-locates a node's vector and links so each hop is exactly one
read, and PQ codes in RAM that steer the walk while exact f32
distances rank the results. This chapter assumes
[reading-hnsw-paper.md](reading-hnsw-paper.md) (greedy graph search,
beams, ef) and [reading-pq.md](reading-pq.md) (PQ codes, ADC).

**Three names, three things — do not blur them.** *Vamana* is the
graph-construction algorithm (§2 of the paper). *DiskANN* is the
SSD-resident system built on it (§3). *FreshDiskANN* is a **later,
separate paper** (Singh et al., 2021) about streaming inserts and
deletes; nothing in this chapter's source covers it, so if you find
yourself explaining how DiskANN handles updates, you have wandered
into a different paper.

Every claim below cites Subramanya, Devvrit, Kadekodi, Krishnaswamy
& Simhadri, *"DiskANN: Fast Accurate Billion-point Nearest Neighbor
Search on a Single Node"*, NeurIPS 2019, by section, algorithm or
figure. There is **no DiskANN clone in `resources/codebases.md`**, so
unlike the qdrant and usearch chapters this one has no `file:line`
anchors — every number here is the paper's, and the paper is the only
thing being verified against.

## The problem in one sentence

A billion 128-d vectors need ~512 GB for the vectors plus ~100 GB
for HNSW links — far beyond one machine's RAM — but naively paging
HNSW to SSD turns each query's hundreds of hops into two dependent
random reads apiece, and §3.3's own figure for an SSD round trip is
*"few hundred microseconds"*.

Make the strawman concrete before reading the fix:

```
  vectors      1e9 × 128 × 4 B                      = 512 GB
  HNSW links   1e9 × 151 B  (reading-hnsw-paper.md, §4.2.3, M=16)
                                                    = 151 GB
  ---------------------------------------------------------------
  RAM needed   663 GB    vs one machine's 64 GB      → 10× over

  paged to SSD, per hop: 1 read for the vector + 1 for the links,
  and the second cannot start until the first is parsed.
  2 × 200 µs × 300 hops                             = 120 ms/query
```

DiskANN's target for the same dataset, from the abstract: *"> 5000
queries a second with < 3ms mean latency and 95%+ 1-recall@1 on a 16
core machine"* — on a machine with 64 GB of RAM and two consumer
NVMe drives (§4: an HP z840, dual Xeon E5-2620v4, 16 cores, 2 ×
Samsung 960 EVO in RAID-0). Note the metric: **1-recall@1**, the
fraction of queries whose *single* true nearest neighbour is
returned, which is a different and stricter quantity than the
recall@10 this topic's bench reports.

## The concepts, step by step

### Step 1 — why HNSW can't just go to disk

> **In:** an HNSW index too large for RAM. **Out:** the one metric
> the redesign optimises — SSD round trips per query — and why the
> obvious paging strategy fails on it.

HNSW search is a beam of *dependent* point lookups: you cannot know
which node to read next until the current node's distances are
computed — topic 0's pointer chase, at SSD latency rather than DRAM
latency. On disk each hop needs the node's vector AND its neighbour
list, which in a RAM-designed layout live in different places.

```
 HNSW paged to SSD, per hop:
   read vector block   ~200 µs ┐  dependent — can't overlap
   read links block    ~200 µs ┘
 × ~300 hops/query  ⇒  ~120 ms/query — dead on arrival
```

The paper is explicit about the currency. §3.3 says a naive port
*"requires many rountrips to SSD (which take few hundred
microseconds) resulting in higher latencies"*, and the entire design
is organised around reducing them. It also gives the fact that makes
the fix possible: *"fetching a small number of random sectors from an
SSD takes almost the same time as one sector"* — SSDs have queue
depth, so **concurrent** reads are nearly free while **dependent**
ones are not.

Every idea below either removes reads (Steps 2–3) or overlaps them
(Step 4).

### Step 2 — Vamana: a flat graph built for few hops

> **In:** the requirement "few hops, no hierarchy". **Out:**
> RobustPrune's α parameter, the two-pass build, and an honest
> statement of what the α > 1 convergence result does and does not
> cover.

**Vamana** is DiskANN's graph: no hierarchy — one flat graph with
degree bound `R`, built so greedy search converges in few hops. The
builder's pruning rule is **RobustPrune** (Algorithm 2), and the
α-slack line is the whole idea:

```
 RobustPrune(p, V, α, R):                        # Algorithm 2
   V ← (V ∪ N_out(p)) \ {p};  N_out(p) ← ∅
   while V ≠ ∅:
     p* ← argmin_{p'∈V} d(p, p')                 # closest remaining
     N_out(p) ← N_out(p) ∪ {p*}
     if |N_out(p)| = R: break
     for p' ∈ V:
       if α · d(p*, p') ≤ d(p, p'):  remove p' from V   ← the α slack
```

At α = 1 this is HNSW's Algorithm-4-style directional pruning — keep
`p'` only if `p'` is closer to the new point than to an already-kept
neighbour. The new move is **α > 1**: `p'` survives unless the kept
edge gets you `α` times closer to it, so fewer candidates are pruned
and **longer edges are retained**. §2.2 states the intent as making
distance to the target *"decrease by a multiplicative factor of α > 1
at every node along the search path, instead of merely decreasing as
in the SNG"*.

Here is the caveat the folk version drops. §2.2's convergence claim
is conditional:

> *"if the out-neighbors of every p ∈ P are determined by
> RobustPrune(p, P \ {p}, α, n − 1), then GreedySearch(s, p, 1, 1)…
> would converge to p ∈ P in logarithmically many steps, if α > 1.
> However, this would result in [Õ(n²) work]"*

— so the logarithmic bound is proved for the version that considers
*every* point as a candidate with *no* degree bound, which is exactly
the version Vamana cannot afford. §2.2 continues that the real
algorithm *"invokes RobustPrune(p, V, α, R) for a carefully selected
V with far fewer than n − 1 nodes"*. Vamana therefore inherits the
*motivation* for the bound, not the bound. Say it that way; the
empirical support is §4.2's measurement that Vamana makes **2–3×
fewer hops** than HNSW and NSG at the same 98% 5-recall@5 with W=4.

The build (Algorithm 3, §2.3) has three details people skip:

1. The graph is **initialised to a random R-regular directed graph**,
   not to an empty one — greedy search has to have somewhere to walk
   on the first insert.
2. The fixed entry point `s` is the dataset's **medoid**.
3. There are **two passes** over a random permutation of the points:
   the first with **α = 1**, the second with the user's α ≥ 1. Each
   pass greedy-searches from `s`, RobustPrunes the visited set, and
   adds back-edges (which are themselves RobustPruned when they
   overflow R).

§2.4 places this against the neighbours: HNSW and NSG *"implicitly
use α = 1"*, and HNSW additionally restricts its pruning candidate
set V to the final search result list, where Vamana and NSG use the
whole visited set.

Levels vs slack, the design fork: HNSW buys few hops with a
hierarchy (extra RAM, layered layout); Vamana buys it with edge
slack (extra degree, flat layout — exactly what disk wants).

### Step 3 — the layout: one node's everything in one sector

> **In:** Vamana's flat graph and 512 GB of vectors. **Out:** the
> on-disk record format, the RAM-resident PQ oracle, and the real
> reason the padding is not waste.

With hops minimised, make each hop cost exactly one read. §3.2 gives
the record layout in one sentence: *"for each point i, we store its
full precision vector x_i followed by the identities of its ≤ R
neighbors. If the degree of a node is smaller than R, we pad with
zeros, so that computing the offset within the disk of the data
corresponding to any point i is a simple calculation, and does not
require storing the offsets in memory."*

```
 RAM:   PQ codes for ALL points (§3.1: "e.g., 32 bytes per data point")
 SSD:   per-node record: [ full f32 vector | ≤R neighbour ids | zero pad ]
        node's data + links CO-LOCATED — one read per hop
```

Note precisely what the padding is for: **fixed-size records so
offsets are computed rather than stored**. It is *not* "pad each node
to its own 4 KB page". Work the paper's own example from §3.5 —
degree 128, d=128:

```
  neighbour ids   4 × 128 = 512 B    (§3.5's "4*128 bytes long
                                       for degree 128 graphs")
  full f32 vector 128 × 4 = 512 B
  ------------------------------------
  record          1024 B  →  fits in one 4 KB sector, with room to
                             spare for more records
```

§3.5's argument for why this is free: *"reading 4KB-aligned disk
address into memory is no more expensive than reading 512B, and the
neighborhood of a vertex … and full-precision coordinates can be
stored on the same disk sector."* The unused remainder of a sector is
not a tax you pay for one-read hops; it is capacity the SSD's minimum
transfer size gives you whether you use it or not. Alignment IS the
schema (topic 3's slotted-page lesson) — but the schema's job here is
*offset arithmetic*, not page ownership.

The RAM side is the PQ trick (§3.1): at 32 bytes per point, codes for
all billion points are a full in-RAM approximate distance oracle.

```
  1e9 points × 32 B = 32 GB   ← fits the z840's 64 GB
  vs 1e9 × 128 × 4  = 512 GB  ← does not
```

One subtlety §3.1 states and most summaries drop: *"Vamana uses
full-precision coordinates when building the graph index"* — the
compression is a search-time device only. A graph built on PQ
distances would bake the quantization error into its topology.

The topic-13 echo is exact: node + adjacency co-located per record =
kuzu's CSR node groups; PQ-in-RAM = the sparse index steering to the
right block (ClickHouse marks, topic 12).

### Step 4 — the search loop: PQ steers, f32 ranks, W reads in flight

> **In:** the SSD index and a RAM-resident PQ oracle. **Out:**
> BeamSearch, the beam width W, and the division of labour that keeps
> the PQ error out of the final ranking.

§3.3's **BeamSearch** fetches the neighbourhoods of the W closest
unexpanded candidates *in one shot* rather than one at a time. The
candidates are chosen by PQ distance (RAM, essentially free); the
blocks that come back carry both the exact f32 vector — used for
ranking — and the neighbour ids — used to extend the frontier.

```rust
// ILLUSTRATION — not quoted from any file; this is Algorithm 1 with
// §3.3's BeamSearch modification and §3.5's implicit re-ranking, as
// Rust. There is no DiskANN clone in resources/codebases.md, so the
// authority is the paper (Alg. 1, §3.3, §3.5). The nearest real code
// you can read is the same beam WITHOUT the disk parts, in qdrant:
// lib/segment/src/index/hnsw_index/graph_layers.rs:109
// (search_on_level) and search_context.rs:32 (process_candidate).
fn search(q: &[f32], k: usize, w: usize) -> Vec<(f32, Id)> {
    let mut cands = MinHeap::from([(pq_dist(q, MEDOID), MEDOID)]);
    let mut seen = HashSet::from([MEDOID]);
    let mut results = Vec::new();
    while let Some(beam) = cands.pop_n(w) {          // W best, by PQ distance
        for blk in ssd_read_batch(&beam) {           // W reads IN FLIGHT at once
            results.push((l2(q, &blk.vector), blk.id));   // exact f32 ranks
            for &n in &blk.neighbors {               // links came in the SAME read
                if seen.insert(n) { cands.push((pq_dist(q, n), n)); }
            }
        }
        if converged(&cands, &results, k) { break; }
    }
    top_k(results, k)
}
```

The division of labour is the deep idea, and §3.5 names it: *"full
precision coordinates essentially piggyback on the cost of expanding
the neighborhoods."* PQ error affects only WHERE YOU WALK, never the
final ranking, because the exact vector arrives in the block you had
to read anyway. Re-ranking is fused into traversal rather than bolted
on — §3.5 contrasts this with the alternative of fetching all
re-ranking vectors in one shot, *"which would result in hundreds of
random disk accesses all in one shot."*

W is the `ef` of the disk world, and the paper bounds it from both
sides in §3.3: *"a small number, W (say 4, 8)"*; *"If W = 1, this
search resembles normal greedy search"*; and *"if W is too large, say
16 or more, then both compute and SSD bandwidth could be wasted."*
Work the latency arithmetic:

```
  SSD round trip (§3.3)             ≈ 200-300 µs (a "few hundred")
  target mean latency (abstract)    < 3 ms
  ⇒ rounds available per query      ≈ 3000 / 250 ≈ 12 beam iterations
  nodes visited at W = 8            ≈ 12 × 8 = 96 SSD records
  vs the strawman's 300 dependent reads at 2 reads each = 600
```

Twelve dependent round trips is the entire budget, which is why hop
count (Step 2) and reads-per-hop (Step 3) both had to be attacked.
§3.3 reports the system running at 30–40% SSD load with threads
spending 40–50% of query time in I/O, on drives capable of 500K+
random reads/s — i.e. tuned so neither the drive nor the CPU is the
sole bottleneck.

§3.4 adds one more RAM-side lever: cache all vertices within
`C = 3 or 4` hops of the fixed start point, so the first couple of
beam iterations never touch the SSD at all.

### Step 5 — the numbers to retain

> **In:** §4's evaluation. **Out:** the six figures worth quoting,
> each with the configuration that produced it — because every one of
> them is conditional on a build parameter.

| number | where | configuration |
|---|---|---|
| **> 5000 QPS, < 3 ms mean latency, 95%+ 1-recall@1** | abstract | SIFT1B, 16-core z840, 64 GB RAM, 2 × 960 EVO RAID-0 |
| **1-recall@1 of 98.68% at < 5 ms** | §4.3 | the *single* (one-shot) billion index: L=125, R=128, α=2 |
| merged index costs **≤ 20% extra latency** | §4.3 | 40 k-means shards, ℓ=2, R=64 → 348 GB, avg degree 92.1 |
| **2–3× fewer hops** than HNSW/NSG | §4.2 | measured at 98% 5-recall@5 with W=4 |
| Vamana builds DEEP1M in **149 s** vs HNSW 219 s, NSG 480 s | §4.1 | in-memory; Vamana L=125, R=70, C=3000, α=2; HNSW M=128, efC=512 |
| **> 95% 1-recall@1 in under 3.5 ms** at 32-byte codes | §4.4 | vs IVFOADC+G+P-32's plateau at 62.74% and -16's at 37.04% |

Two of these are routinely misquoted, so state them carefully.

**The "~5 ms" figure is not the headline.** The abstract's number is
`< 3 ms` mean latency at 95%+ 1-recall@1; the `< 5 ms` belongs to
§4.3's much stronger 98.68% recall point on the one-shot index. Which
one you cite changes both the latency and the recall.

**The "5%" figure is not about SSD residency.** §4.3's sentence is
about *edge locality inside the merged index*: the single index beats
the merged one *"possibly because the in- and out-edges of each node
in the merged index are limited to about ℓ/k = 5% of all points"*,
where k=40 shards and ℓ=2 assignments per point give
`ℓ/k = 2/40 = 5%`. It says nothing about what fraction of the data is
read per query.

The build costs are worth retaining too, because they are the reason
the merged construction exists at all (§4.3): the one-shot billion
index needed **~2 days on an M64-32ms with ≈1100 GB peak RAM**, while
the 40-shard merge produced a comparable index in **~5 days on the
z840 with memory staying under 64 GB**. Sharded build trades wall
clock for a machine you can actually rent.

## How to read the paper (with the concepts in hand)

| paper | step | what to extract |
|---|---|---|
| §1, §2.1 | 1 | the three desiderata; why hierarchy is the wrong answer on disk |
| §2.2, Alg. 2 | 2 | RobustPrune's α line — and the `RobustPrune(p, P\{p}, α, n−1)` qualifier on the log bound |
| §2.3, Alg. 3 | 2 | random R-regular init, the medoid, the two passes (α=1 then α), back-edges |
| §2.4 | 2 | HNSW and NSG *"implicitly use α = 1"*; the candidate-set difference |
| §3.1 | 3 | PQ in RAM at ~32 B/point; Vamana builds on full precision |
| §3.2 | 3 | the record layout and *why* it is zero-padded (computed offsets) |
| §3.3 | 4 | BeamSearch; W = 4 or 8; the "few hundred microseconds" round trip |
| §3.4 | 4 | caching everything within C = 3–4 hops of the start |
| §3.5 | 4 | implicit re-ranking; *"no more expensive than reading 512B"* |
| §4.1–4.2 | 5 | in-memory comparison, build times, the 2–3× hop reduction |
| §4.3–4.4 | 5 | the billion-scale numbers, and the ℓ/k = 5% sentence in its real context |

Read §2.2 and §3.5 twice; they are the two paragraphs the rest of the
paper rests on, and both contain a qualifier that summaries drop.

## Questions (answer in notes.md)

1. Count SSD reads: HNSW-on-disk (links and vectors separate) vs
   DiskANN per hop. Where did the factor go?
2. Why α > 1 provably shortens greedy walks — sketch the geometric
   argument (each hop shrinks distance by α).
3. Beam search issues W reads concurrently. Connect to topic 0's
   MLP: what's the SSD equivalent of "10 outstanding misses"?
4. Why is it fine that PQ steers but f32 ranks? What recall failure
   remains possible (PQ error > neighbor spacing → wrong REGION)?
5. M28 preview: DiskANN blocks over object storage — what breaks
   when a "read" is 50 ms S3 GET instead of 100 µs NVMe? Which knob
   moves?

## Done when

Answer each before unfolding it.

- [ ] You can say why HNSW does not survive being put on disk, in reads per hop rather than in generalities.
  <details><summary>Answer</summary>

  Two reads per hop, and they are *dependent*. An RAM-designed HNSW
  keeps a node's vector and its neighbour list in separate
  allocations, so a hop needs one read for the vector, then — after
  computing distances — another for the links, then the next hop's
  address is only known once those return. At §3.3's *"few hundred
  microseconds"* per SSD round trip and a few hundred hops, that is
  ~120 ms per query. DiskANN attacks both factors: Step 3 makes it
  one read per hop by co-locating vector and links in one record
  (§3.2), and Step 4 makes W of them concurrent rather than
  sequential (§3.3), exploiting the fact that *"fetching a small
  number of random sectors from an SSD takes almost the same time as
  one sector."* The budget that remains is about twelve dependent
  round trips for a 3 ms query.
  </details>

- [ ] You can explain what `α > 1` does to greedy walk length, and why that is the property Vamana is buying.
  <details><summary>Answer</summary>

  Algorithm 2 drops a candidate `p'` only when
  `α·d(p*, p') ≤ d(p, p')`. Raising α above 1 makes that test harder
  to satisfy, so more candidates survive and the retained edge set
  keeps *longer* edges. §2.2's stated intent is that distance to the
  target then *"decrease[s] by a multiplicative factor of α > 1 at
  every node along the search path"*, so the number of hops is
  logarithmic in the distance ratio rather than linear in it. **The
  honest qualifier**: §2.2 proves that only for
  `RobustPrune(p, P\{p}, α, n−1)` — every point a candidate, no
  degree bound — which costs Õ(n²) and is not what Vamana runs.
  Vamana uses *"a carefully selected V with far fewer than n − 1
  nodes"*, so it inherits the motivation, not the theorem. The
  empirical replacement is §4.2: 2–3× fewer hops than HNSW and NSG at
  98% 5-recall@5. Why this property: on SSD, hops are round trips, so
  hop count *is* latency, and Vamana buys it with degree (flat
  layout) instead of with a hierarchy (RAM).
  </details>

- [ ] You can describe the block layout and count the SSD reads per hop it achieves.
  <details><summary>Answer</summary>

  §3.2: per point, the full-precision vector followed by up to R
  neighbour ids, zero-padded to a fixed size *so that offsets are
  computed rather than stored in RAM*. One read per hop, because both
  halves of what a hop needs are in the same record. §3.5's own
  worked case is degree 128 at d=128: `4 × 128 = 512 B` of ids plus
  `128 × 4 = 512 B` of vector = 1024 B, which *"can be stored on the
  same disk sector."* The remainder of a 4 KB sector is not overhead
  charged to this design — §3.5's premise is that *"reading
  4KB-aligned disk address into memory is no more expensive than
  reading 512B"*, so it is capacity the transfer size gives you for
  free, and the implementation packs further records into it. Do not
  say "each node is padded to a 4 KB page"; the paper does not.
  </details>

- [ ] You can explain the division of labour in the search loop: PQ steers, f32 ranks, W reads in flight — and what recall failure each part is responsible for.
  <details><summary>Answer</summary>

  PQ codes live in RAM (§3.1, *"e.g., 32 bytes per data point"* — 32
  GB for a billion points) and choose which W candidates to fetch.
  The SSD read returns both the neighbour ids (frontier) and the
  exact f32 vector, so ranking is done on exact distances with *"no
  extra reads"* (§3.5) — the re-ranking piggybacks on traversal.
  Failure modes split cleanly: an f32 ranking error is impossible,
  because the final order uses exact distances; the residual risk is
  entirely in *steering*. If PQ error exceeds the spacing between
  neighbourhoods, the beam is pointed at the wrong region and the
  true neighbour is never fetched, so no amount of exact re-ranking
  recovers it. W controls how much slack the steering gets: §3.3
  suggests 4 or 8, says W=1 degenerates to plain greedy search, and
  warns that *"if W is too large, say 16 or more, then both compute
  and SSD bandwidth could be wasted."*
  </details>

- [ ] You can state the paper's headline numbers with the configuration attached, and name the two that are commonly misquoted.
  <details><summary>Answer</summary>

  Headline (abstract): **> 5000 QPS, < 3 ms mean latency, 95%+
  1-recall@1** on SIFT1B, 16-core z840 with 64 GB RAM and two
  consumer NVMe drives. The two traps: (1) the *"< 5 ms"* figure
  belongs to a different point — §4.3's one-shot index at **98.68%**
  1-recall@1 — so quoting "~5 ms" as the headline understates the
  latency claim and the recall claim at once; (2) the *"5%"* figure
  is §4.3's `ℓ/k = 2/40` **edge locality inside the merged index**,
  offered as a possible reason the merged index is slower, and has
  nothing to do with what fraction of the SSD is read per query.
  Also worth attaching: 1-recall@1 is a stricter metric than the
  recall@10 this topic's bench measures.
  </details>

- [ ] You wrote answers to all five questions in notes.md, including the M28 object-storage preview.
  <details><summary>Answer</summary>

  For question 5, the arithmetic is the point: at 250 µs per read a 3
  ms budget allows ~12 dependent rounds; at a 50 ms S3 GET the same
  budget allows zero, and even a 500 ms budget allows ten. The knobs
  that move are W (up hard, since object stores have effectively
  unbounded concurrency and §3.3's "16 or more wastes bandwidth"
  warning was about a device with 500K IOPS, not about a network),
  and §3.4's cache radius C (up, since the first hops are the ones
  you can most cheaply keep local). What breaks is the *dependency
  chain*: DiskANN's design assumes round trips are cheap enough that
  a dozen of them fit in the latency budget, and that assumption is
  the thing object storage removes.
  </details>

## References

**Papers**
- Subramanya, Devvrit, Kadekodi, Krishnaswamy, Simhadri — "DiskANN:
  Fast Accurate Billion-point Nearest Neighbor Search on a Single
  Node" (NeurIPS 2019)

| where | what it says |
|---|---|
| abstract | *"> 5000 queries a second with < 3ms mean latency and 95%+ 1-recall@1 on a 16 core machine"* |
| §2.2, Alg. 2 | RobustPrune; `if α·d(p*,p') ≤ d(p,p')` |
| §2.2 | the log-convergence result holds for `RobustPrune(p, P\{p}, α, n−1)`, which costs Õ(n²) |
| §2.3, Alg. 3 | random R-regular init; medoid start; two passes, α=1 then α |
| §2.4 | HNSW and NSG *"implicitly use α = 1"* |
| §3.1 | PQ in RAM, *"e.g., 32 bytes per data point"*; Vamana builds on full precision |
| §3.2 | vector then ≤R ids, zero-padded so offsets are computed not stored |
| §3.3 | BeamSearch; W *"(say 4, 8)"*; *"few hundred microseconds"*; *"16 or more"* wastes |
| §3.4 | cache everything within C = 3 or 4 hops of the start |
| §3.5 | *"reading 4KB-aligned disk address … no more expensive than reading 512B"*; the 4·128 B + vector sector calculation |
| §4.1 | Vamana L=125, R=70, C=3000, α=2 vs HNSW M=128, efC=512; DEEP1M build 149 s / 219 s / 480 s |
| §4.2 | Vamana makes 2–3× fewer hops, at 98% 5-recall@5, W=4 |
| §4.3 | one-shot index 98.68% 1-recall@1 at < 5 ms; merged ≤ 20% extra latency; `ℓ/k = 5%` edge locality; build costs (~2 days / 1100 GB vs ~5 days / < 64 GB) |
| §4.4 | > 95% 1-recall@1 under 3.5 ms at 32-byte codes; IVFOADC+G+P plateaus at 62.74% (-32) and 37.04% (-16) |

- Singh, Subramanya, Krishnaswamy, Simhadri — "FreshDiskANN"
  (2021) — the *separate* paper on streaming updates. Not covered
  here; do not attribute its results to the 2019 paper.

**Code**
- [DiskANN](https://github.com/microsoft/DiskANN) — Microsoft's
  production implementation. **Not pinned in
  `resources/codebases.md`**, so this chapter cites no line numbers
  from it; if you read it, treat what you find as a *different
  artifact* from the paper and record the commit you read.
