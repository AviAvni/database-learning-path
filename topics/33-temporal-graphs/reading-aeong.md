# AeonG: anchor+delta history behind an MVCC front door

AeonG (VLDB 2024) is topic 13's Memgraph plus two things: a temporal
query surface bolted onto Cypher's MATCH, and a second storage tier
that catches version chains as MVCC garbage collection would otherwise
shred them. You already know the substrate — the Vertex struct, the
undo-delta N2O chains, the RWSpinLock — so read this paper as "what is
the *minimum* you must add to an MVCC graph engine to make history
queryable?" Before the PDF, this chapter builds the six ideas the
paper layers on; then a section map with a ~1.5 h budget.

This is a guide to a **paper**: **Hou et al., "AeonG: An Efficient
Built-in Temporal Support in Graph Databases," PVLDB 17(6), 2024**
([arXiv:2304.12212](https://arxiv.org/abs/2304.12212)). Every figure
below (9.74%, 5.73×, 2.57×, the Equation 1 bands, the scan bound) was
checked against that PDF and is cited to its section; the one Rust block
is an illustration of the repo's own `snapshot.rs`, marked as such. One
attribution guard-rail up front: AeonG is *built on* Memgraph and
RocksDB, so keep straight which behavior is AeonG's design and which it
inherits — the paper is explicit (§6.1) and this guide flags it at each
step.

## The problem in one sentence

MVCC engines already create every historical version of the graph and
then throw them away in GC; AeonG keeps them queryable for only **9.74%**
overhead on current-time operation, with up to **5.73×** lower storage and
**2.57×** lower temporal query latency than dedicated temporal graph DBs
(abstract; §7).

## The concepts, step by step

### Step 1 — lifespans per VERSION, in transaction time

> **In:** nothing yet — this step fixes the atomic unit of history (a
> versioned interval) that every later step timestamps, stores, and checks.
> **Out:** the predicate "legal at t" and the per-version (not per-object)
> choice that makes Step 3's three-way split necessary.

A **lifespan** ω = [st, ed) is the half-open interval during which one
version of a graph object was the current one; the current version has
ω = [t, +∞), and a version is **legal at t** iff `st ≤ t < ed`. AeonG uses
**transaction time** (when the DB *learned* the fact — the topic README's
second time axis), not **valid time** (when the fact was true in the
world): st/ed are commit timestamps, which MVCC already stamps on every
version for free (topic 8's begin_ts/end_ts). The subtle choice: the
lifespan attaches to each *version*, not to the object — contrast the
baseline T-GQL (§7.1), which gives each object one time period — so
updating a Phone vertex creates a new Phone version and re-links the Owns
edge, instead of duplicating the unchanged Customer neighbor.

Why it matters: per-version lifespans mean history costs are proportional
to *change*, not to graph size — the same bet as topic 20's delta
matrices. Hold that "cost ∝ change" promise; Step 3 is what keeps it true
for graphs specifically.

### Step 2 — the query surface: two clauses in MATCH

> **In:** the "legal at t" predicate from Step 1.
> **Out:** the two grammar forms (point and range) that Step 6's scan
> operator must evaluate, and the M33 surface this guide is really about.

Cypher is extended inside the MATCH clause with **`FOR TT AS OF t`**
(time-point: the graph as of one instant) and **`FOR TT FROM t1 TO t2`**
(time-range: every version legal anywhere in the window). `TT` names
*transaction time* from Step 1:

```
MATCH (:Customer {name:'Jack'})-[r]-(p:Phone) FOR TT AS OF t_n RETURN p.IP
```

The paper's motivating example (§1, Example 1, Fig 1) is fraud detection:
at `t_n` Jack's phone is in Singapore with a $390 balance; at `t_{n+1}`
(**one minute after `t_n`**, per §1) a $300 purchase transaction commits
in New York, and the phone's location is now New York too — so the
*current* graph looks legitimate (transaction location = phone location).
Only comparing `t_{n+1}` against `t_n` reveals the phone moved
Singapore → New York within one minute, an impossible trip that flags the
transaction. Every timestamp here is transaction time; the paper is careful
that the *current* state hides the fraud and only history exposes it.

Why it matters: this is exactly M33's `AT TIME` / `BETWEEN` surface — and
AeonG's grammar decision (scope the time clause to MATCH, not the whole
query) is one you'll have to make for FalkorDB too.

### Step 3 — three clocks per object: VP, VE, EP

> **In:** the per-version lifespan of Step 1, applied to a graph object
> that has both properties and topology.
> **Out:** three independently-versioned components, so a topology change
> doesn't fabricate a property version — the graph-specific half of
> Step 1's "cost ∝ change" promise.

Each graph object's state is split into three separately-timestamped
components (§4.1): **VP** (vertex properties), **VE** (vertex edges — the
in/out adjacency lists, i.e. topology stored *inside* the vertex for fast
neighborhood traversal), and **EP** (edge properties), each carrying its
own ω. Add an edge to Jack and only his VE gets a new version; his VP
lifespan is untouched — a topology change doesn't fake a property change.
Modification is Memgraph's paradigm verbatim: update-in-place creates the
new current version, the previous one becomes a historical version linked
in the MVCC version chain (topic 13's undo deltas, N2O ordering) — that
chain walk is *inherited*, not AeonG's invention.

Why it matters: without the VP/VE/EP split, a supernode gaining edges
would churn out full property versions on every insert. Worked: a hub
vertex with 1,000 scalar properties that receives 10,000 new edges over a
day would, without the split, write 10,000 property-versions of ~1,000
values each ≈ 10⁷ stored values; with the split, those 10,000 changes land
only in VE and the 1,000 properties keep their *one* VP version. The split
is what keeps Step 1's promise where topology and properties change at
wildly different rates.

### Step 4 — the second tier: migrate during GC, not instead of it

> **In:** the historical versions Step 3 keeps producing in the MVCC chain.
> **Out:** those versions relocated into a key-value store, off the
> transaction critical path — the reason the 9.74% headline is small.

Current storage is Memgraph's multi-version in-memory store; the
**historical storage** is **RocksDB** — the popular KV store AeonG
integrates by launching a RocksDB process at startup and, for a
distributed option, swapping in **TiKV** behind the same interface (§6.1).
It is fed by *asynchronous migration*: when MVCC GC decides a version is
reclaimable, instead of freeing it, the GC path encodes the undo delta,
puts it in the KV store, then physically deletes it — this is the paper's
**Algorithm 1** (`Migrate`: for each unreclaimed undo, `encode2KV`,
`KV_store::put`, then delete). Deferred and non-intrusive, off the
transaction critical path. The KV layout does the indexing:

```
 key   = type ('V'/'E'/'VE') + Gid + ω          value = delta or anchor
                                                 ('D' / 'A' suffix bit)

 SkipList order:   AV:42:[0,7) │ DV:42:[7,9) │ DV:42:[9,13) │ AV:42:[13,20) │ ...
                   └─ same Gid clusters, sorted by lifespan ─┘
```

Why it matters: "GC as migration" is the paper's cheapest trick — the
9.74% headline (§7.4) is low *because* history capture rides a thread that
already existed. On Wu/Pavlo's GC axis (topic 8) this is cooperative-ish:
the reaper still runs, it just changed its disposal method from `free` to
`put`.

### Step 5 — anchor+delta, with adaptive spacing

> **In:** the delta chains Step 4 wrote into RocksDB.
> **Out:** the anchor records that bound reconstruction cost, and the
> per-object interval `u` that Step 6's scan bound charges for.

Deltas alone make old versions expensive: reconstructing a version far
down a long history means replaying everything before it. So AeonG
periodically writes an **anchor** — a complete materialized state — and
reconstruction becomes seek-then-replay:

```rust
// ILLUSTRATION — not quoted from AeonG (a C++ Memgraph fork). This is the
// shape of this repo's own snapshot.rs contract: experiments/src/snapshot.rs:43
// (`at_time` = nearest anchor at-or-before t, then bounded delta replay).
// AeonG's real seek is KV_store::seeknext on the 'AV:id' prefix (§5.1, Alg 2).
fn reconstruct(kv: &Kv, gid: Gid, o1: Lifespan) -> Object {
    let (mut state, at) = kv.seek_anchor_at_or_before(gid, o1.st); // 'A'
    for d in kv.deltas_between(gid, at, o1.st) {                   // 'D'
        state.apply(d);
    }
    state // replay length bounded by the anchor interval u
}
```

AeonG's twist is **adaptive anchoring** (§4.2, **Equation 1**): the anchor
interval `u_o` per object rises with its update frequency `f(o)`, in three
bands using thresholds τ1, τ2 and a constant c:

```
u_o = τ1·c          if f(o) ≤ τ1          (low frequency)
u_o = τ2·c          if τ1 < f(o) ≤ τ2     (medium frequency)
u_o = τ2²/τ1·c      if τ2 ≤ f(o)          (high frequency)
```

Worked with the paper's defaults τ1 = 1k, τ2 = 10k, c = 1% (§7.1.3):

- low (`f ≤ 1,000`):   `u = 1,000 × 0.01 = 10` — an anchor every 10 deltas.
- medium (`1k < f ≤ 10k`): `u = 10,000 × 0.01 = 100`.
- high (`f ≥ 10k`):    `u = 10,000² / 1,000 × 0.01 = 100,000 × 0.01 = 1,000`.

So the hottest objects get `u = 1,000` — an anchor only every *thousand*
deltas, i.e. **sparser** anchors relative to their churn. Why it matters:
uniform spacing lets one hot supernode dominate anchor storage; adaptive
spacing bounds storage at the cost of longer replays (larger `u`) exactly
where updates — and thus reads of history — concentrate. That is a
deliberate, tunable regression you should argue with in notes.md.

### Step 6 — the query engine: two stores, one legal check

> **In:** the two-store layout of Step 4 and the per-object interval `u`
> of Step 5.
> **Out:** the end-to-end cost of one temporal read — the `O(ι(n) +
> log(A_v) + u)` bound that lane 3's spacing dial moves.

The scan operator (§5.1, Algorithm 2) fetches versions with the legality
predicate from Step 1 generalized to windows: a version's ω passes a query
constraint C = [t1, t2] iff `ω.st ≤ C.t2 ∧ ω.ed > C.t1` (a point query has
t1 = t2). Because migration is asynchronous (Step 4), a version old enough
to be "historical" may still sit in current storage — so every temporal
scan consults BOTH: the MVCC snapshot-visibility walk in current storage,
plus a probe of RocksDB. Anchor-based retrieval `seeknext`s the prefix
`AV:id` to land on the nearest anchor directly, skipping delta chains; the
paper proves the scan cost is

```
O( ι(n) + log(A_v) + u )
   │       │           └ u deltas replayed after the anchor (Step 5)
   │       └ SkipList seek among a vertex's A_v anchors
   └ current-version lookup: log n with a B+-tree index, n without
```

Worked (§5.1's own terms): a graph of `n = 10⁶` vertices with a B+-tree
index gives `ι(n) = log₂(10⁶) ≈ 20`; a vertex with `A_v = 256` anchors
gives `log₂(256) = 8`; a medium-churn object from Step 5 has `u = 100`. The
read touches ≈ `20 + 8 + 100 = 128` units — and `u` is the *only* term you
control, so Step 5's spacing dial is the whole read-latency story, which is
precisely what bench lane 3 plots. The expand operator (§5.2) gets the same
two-store treatment for VE topology.

## How to read the paper (with the concepts in hand)

PVLDB 17(6), ~13 pages; budget ~1.5 h.

- **§1** (10 min) — the Fig 1 fraud example (Step 2) and the two claims:
  low overhead vs current-only, low latency vs temporal-native.
- **§2** (15 min) — the model (Step 1) and the query language (Step 2).
  Pause on the per-version-vs-per-object lifespan contrast with T-GQL; it
  justifies everything in §4.
- **§3** (5 min) — architecture skim: transaction manager + hybrid
  storage + temporal query engine. You know all three boxes already.
- **§4.1** (15 min) — current storage: VP/VE/EP (Step 3). Map every
  sentence onto Memgraph's vertex.hpp from topic 13.
- **§4.2** (20 min) — **the core**: migration during GC (Step 4,
  Algorithm 1), the KV key format, anchor+delta and Equation 1's three
  bands (Step 5). Re-derive the u = 10/100/1000 defaults yourself.
- **§5** (15 min) — scan/expand with the window legal check, both-store
  consults, anchor-based retrieval and the `O(ι(n) + log(A_v) + u)` bound
  (Step 6, Algorithm 2).
- **§6** (5 min) — implementation on Memgraph v2.2.0 + RocksDB v6.14.6
  (+ TiKV); note what they had to touch vs reuse.
- **§7** (15 min) — where 5.73×/2.57×/9.74% come from: which benchmark
  (T-mgBench, T-LDBC) and which competitors (Clock-G, T-GQL) before quoting
  the numbers.

## Questions (answer in notes.md)

1. Place AeonG in Wu/Pavlo's 5-axis MVCC table (topic 8): delta version
   storage, N2O ordering, GC-as-migration — which axes does the historical
   tier *change* vs merely extend, and does index management even apply to
   the RocksDB tier?
2. M33: should FalkorDB's historical tier store matrix deltas or serialized
   per-object deltas — and what plays the anchor? (Hint: topic 20's delta
   matrices are already deltas; M30's snapshots are already anchors.)
3. After implementing `snapshot.rs` and running bench lane 3: your store
   uses one global `every`; AeonG uses per-object adaptive intervals.
   Construct the event distribution where global spacing loses worst, and
   estimate by how much using lane 3's replay_len.
4. The VP/VE/EP split gives three clocks per object. FalkorDB's topology
   lives in shared matrices, not per-object lists — what is the analogous
   split, and what goes wrong if `AT TIME` versions the whole matrix as one
   object?
5. The window legal check plus async migration means both stores are
   consulted on every temporal scan. When is the double consult worse than
   a synchronous-migration design, and why did AeonG accept it anyway?
   (Hint: whose critical path does each design tax?)

## Done when

Answer each before unfolding it.

- [ ] You can price one `FOR TT AS OF t` vertex read end-to-end and name which term the spacing dial moves.

  <details><summary>Answer</summary>

  Three added costs on top of the normal read (§5.1): (1) the
  current-version lookup `ι(n)` — `log n` with a B+-tree index, `n`
  without; (2) a SkipList seek `log(A_v)` to the nearest anchor among that
  vertex's `A_v` anchors; (3) replaying `u` deltas from the anchor to `t`.
  Total `O(ι(n) + log(A_v) + u)`. Only `u` is under your control — it *is*
  Step 5's anchor interval — so lane 3's spacing dial moves the `u` term
  (dense anchors → small `u`, fast reads, fat storage; sparse → large `u`,
  thin storage, long replays).

  </details>

- [ ] You can compute AeonG's three default anchor intervals from Equation 1.

  <details><summary>Answer</summary>

  With τ1 = 1k, τ2 = 10k, c = 1% (§7.1.3): low-frequency objects
  (`f ≤ 1,000`) get `u = τ1·c = 1,000 × 0.01 = 10`; medium
  (`1k < f ≤ 10k`) get `u = τ2·c = 100`; high (`f ≥ 10k`) get
  `u = τ2²/τ1·c = 10,000²/1,000 × 0.01 = 100,000 × 0.01 = 1,000`. The hot
  objects get the *sparsest* anchors (largest `u`), trading longer replays
  for bounded anchor storage exactly where churn — and history reads —
  concentrate.

  </details>

- [ ] You can explain why "GC as migration" is what makes the 9.74% overhead small.

  <details><summary>Answer</summary>

  MVCC already runs an asynchronous GC pass that *deletes* reclaimable
  versions (topic 8). AeonG doesn't add a capture thread; it changes that
  pass's disposal step from "free the undo delta" to "encode it, `put` it
  in RocksDB, then delete" (§4.2, Algorithm 1). History capture therefore
  rides an existing off-critical-path thread, so current-time transactions
  pay almost nothing — the paper measures up to 9.74% degradation (§7.4),
  and the smallness is *because* nothing new sits on the commit path.

  </details>

- [ ] You can say which of AeonG's behaviors are its own design vs inherited from Memgraph/RocksDB.

  <details><summary>Answer</summary>

  Inherited: the in-memory multi-version current store, undo-delta version
  chains in N2O order, and the async GC pass (Memgraph, topic 13); the
  SkipList-ordered on-disk KV index and its `seeknext` (RocksDB, §6.1).
  AeonG's own: the per-*version* lifespan model (Step 1), the VP/VE/EP
  split (Step 3), turning GC disposal into KV migration (Step 4,
  Algorithm 1), adaptive anchor+delta history with Equation 1 (Step 5), and
  the two-store temporal scan/expand with the `O(ι(n)+log(A_v)+u)` bound
  (Step 6). Attributing the 9.74% cheapness to "Memgraph being fast" (or the
  storage design to RocksDB) is the mistake §6.1 exists to prevent.

  </details>

## References

**Papers**
- Hou, Zhao, Wang, Lu, Jin, Wen, Du — "AeonG: An Efficient Built-in
  Temporal Support in Graph Databases (Extended Version)" (PVLDB
  17(6): 1515–1527, 2024) —
  [arXiv:2304.12212](https://arxiv.org/abs/2304.12212) — ~13 pages, ~1.5 h:
  read §1 (Fig 1) and §2 for Steps 1–2, §4.1 for Step 3, §4.2 (Algorithm 1,
  Equation 1) for Steps 4–5, §5 (Algorithm 2, the scan bound) for Step 6,
  and §7 for the numbers. Anchors used above: abstract/§7 (9.74%, 5.73×,
  2.57×), §4.2 (Algorithm 1, Equation 1's three bands), §5.1 (scan
  complexity `O(ι(n)+log(A_v)+u)`), §6.1 (Memgraph + RocksDB + TiKV),
  §7.1 (T-GQL, Clock-G baselines; τ1=1k, τ2=10k, c=1% defaults).
- Wu, Arulraj, Lin, Xian, Pavlo — "An Empirical Evaluation of In-Memory
  Multi-Version Concurrency Control" (VLDB 2017) — topic 8; the 5-axis
  table question 1 asks you to fill.

**Code**
- [AeonG](https://github.com/hououou/AeonG) — the Memgraph fork; diff it
  mentally against topic 13's `src/storage/v2/`.
- [memgraph](https://github.com/memgraph/memgraph) — topic 13 clone; the
  delta chains AeonG migrates instead of freeing.
- This topic's `experiments/src/snapshot.rs` (`at_time`,
  [line 43](experiments/src/snapshot.rs)) — AeonG's §4.2 storage bet in
  miniature; bench lane 3 is your private §7.
