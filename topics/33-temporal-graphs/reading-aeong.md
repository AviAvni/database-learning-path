# AeonG: anchor+delta history behind an MVCC front door

AeonG (VLDB 2024) is topic 13's memgraph plus two things: a temporal
query surface bolted onto Cypher's MATCH, and a second storage tier
that catches version chains as MVCC garbage collection would otherwise
shred them. You already know the substrate — the Vertex struct, the
undo-delta N2O chains, the RWSpinLock — so read this paper as "what is
the *minimum* you must add to an MVCC graph engine to make history
queryable?" Before the PDF, this chapter builds the six ideas the
paper layers on; then a section map with a ~1.5h budget.

## The problem in one sentence

MVCC engines already create every historical version of the graph and
then throw them away in GC; AeonG keeps them queryable for only 9.74%
overhead on current-time operation, with up to 5.73× lower storage and
2.57× lower temporal query latency than dedicated temporal graph DBs.

## The concepts, step by step

### Step 1 — lifespans per VERSION, in transaction time

A **lifespan** ω=[st, ed) is the half-open interval during which one
version of a graph object was the current one; the current version has
ω=[t, +∞), and a version is "legal at t" iff st ≤ t < ed. AeonG uses
**transaction time** (when the DB learned the fact — topic 33 README's
second axis), not valid time: st/ed are commit timestamps, which MVCC
already stamps on every version for free (topic 8's begin_ts/end_ts).
The subtle choice: the lifespan attaches to each *version*, not to the
object (contrast T-GQL, which gives each object one period) — so
updating a Phone vertex creates a new Phone version and re-links the
Owns edge, instead of duplicating the unchanged Customer neighbor. Why
it matters: per-version lifespans mean history costs are proportional
to *change*, not to graph size — the same bet as topic 20's delta
matrices.

### Step 2 — the query surface: two clauses in MATCH

Cypher is extended inside the MATCH clause with `FOR TT AS OF t`
(time-point: the graph as of one instant) and `FOR TT FROM t1 TO t2`
(time-slice: every version legal anywhere in the window):

```
MATCH (:Customer name:'Jack')-[r]-(p:Phone) FOR TT AS OF t_n RETURN p.IP
```

The paper's motivating example (§1 Fig 1) is fraud detection: Jack's
phone IP moves Singapore→New York within one minute of a $300
transaction — the current graph shows nothing wrong, only history
reveals the impossible travel. Why it matters: this is exactly M33's
`AT TIME` / `BETWEEN` surface — AeonG's grammar decision (scope the
time clause to MATCH, not the whole query) is one you'll have to make
for FalkorDB too.

### Step 3 — three clocks per object: VP, VE, EP

Each graph object's state is split into three separately-timestamped
components: **VP** (vertex properties), **VE** (vertex edges — the
in/out topology lists), and **EP** (edge properties), each carrying
its own ω. Add an edge to Jack and only his VE gets a new version; his
VP lifespan is untouched — a topology change doesn't fake a property
change. Modification is memgraph's paradigm verbatim: update-in-place
creates the new current version, the previous one becomes a historical
version linked in the MVCC version chain (topic 13's undo deltas,
N2O). Why it matters: without the VP/VE/EP split, a supernode gaining
edges would churn out full property versions on every insert — the
split is what keeps Step 1's "cost ∝ change" promise for graphs, where
topology and properties change at wildly different rates.

### Step 4 — the second tier: migrate during GC, not instead of it

Current storage is memgraph's multi-version in-memory store; the
**historical storage** is a key-value store fed by *asynchronous
migration*: when MVCC GC decides a version is reclaimable, instead of
freeing it, the GC thread encodes the undo delta, puts it in the KV
store, then physically deletes it (Algorithm 1) — deferred and
non-intrusive, off the transaction critical path. The KV layout does
the indexing:

```
 key   = type ('V'/'E'/'VE') + Gid + ω          value = delta or anchor
                                                 ('D' / 'A' suffix bit)

 SkipList order:   AV:42:[0,7) │ DV:42:[7,9) │ DV:42:[9,13) │ AV:42:[13,20) │ ...
                   └─ same Gid clusters, sorted by lifespan ─┘
```

Why it matters: "GC as migration" is the paper's cheapest trick — the
9.74% headline number is low *because* history capture rides a thread
that already existed. On Wu/Pavlo's GC axis this is cooperative-ish:
the reaper still runs, it just changed its disposal method.

### Step 5 — anchor+delta, with adaptive spacing

Deltas alone make old versions expensive: reconstructing a version far
down a long history means replaying everything before it. So AeonG
periodically writes an **anchor** — a complete materialized state —
and reconstruction becomes seek-then-replay:

```rust
// reconstruct version o1: nearest anchor at-or-before, then deltas
fn reconstruct(kv: &Kv, gid: Gid, o1: Lifespan) -> Object {
    let (mut state, at) = kv.seek_anchor_at_or_before(gid, o1.st); // 'A'
    for d in kv.deltas_between(gid, at, o1.st) {                   // 'D'
        state.apply(d);
    }
    state // replay length bounded by the anchor interval
}
```

This is topic 5's checkpoint-vs-redo trade wearing graph clothes, and
it is *exactly* the contract of this topic's `snapshot.rs` stub —
`at_time(t)` = nearest anchor + bounded replay, with bench lane 3
pricing the spacing. AeonG's twist is **adaptive anchoring**
(Equation 1): the anchor interval u_o per object rises with its update
frequency f(o), in three bands (low: τ1·c, medium: τ2·c, high:
τ2²/τ1·c) — hot objects get *sparser* anchors relative to their churn.
Why it matters: uniform spacing lets one hot supernode dominate anchor
storage; adaptive spacing bounds storage at the cost of longer replays
exactly where updates (and thus reads of history) concentrate — a
deliberate, tunable regression you should argue with in notes.md.

### Step 6 — the query engine: two stores, one legal check

The scan operator fetches versions with the legality predicate from
Step 1 generalized to windows (Equation 2: ω.st ≤ C.t2 ∧ ω.ed > C.t1,
where C=[t1,t2] is the query's time constraint — a point query has
t1=t2). Because migration is asynchronous (Step 4), a version old
enough to be "historical" may still sit in current storage — so every
temporal scan consults BOTH: the MVCC snapshot-visibility walk in
current storage, plus a probe of the KV store. Anchor-based retrieval
probes prefix "AV:id:C" to land on the nearest anchor directly,
skipping delta chains; scan cost is O(ι(n) + log(A_v) + u) — index
lookup, seek among anchors, then u = average anchor interval of delta
replay. The expand operator gets the same treatment for VE. Why it
matters: u is the *only* term you control — Step 5's spacing dial is
the whole read-latency story, which is precisely what lane 3 plots.

## How to read the paper (with the concepts in hand)

PVLDB 17(6), ~13 pages; budget ~1.5h.

- **§1** (10 min) — the Fig 1 fraud example (Step 2) and the two
  claims: low overhead vs current-only, low latency vs temporal-native.
- **§2** (15 min) — the model (Step 1) and the query language
  (Step 2). Pause on the per-version-vs-per-object lifespan contrast
  with T-GQL; it justifies everything in §4.
- **§3** (5 min) — architecture skim: transaction manager + hybrid
  storage + temporal query engine. You know all three boxes already.
- **§4.1** (15 min) — current storage: VP/VE/EP (Step 3). Map every
  sentence onto memgraph's vertex.hpp from topic 13.
- **§4.2** (20 min) — **the core**: migration during GC (Step 4), KV
  key format, anchor+delta and Equation 1's three bands (Step 5).
- **§5** (15 min) — scan/expand with Equation 2, both-store consults,
  anchor-based retrieval and the complexity bound (Step 6).
- **§6** (5 min) — implementation on memgraph; note what they had to
  touch vs reuse.
- **§7** (15 min) — where 5.73×/2.57×/9.74% come from; check which
  benchmark and which competitors before quoting the numbers.

## Questions (answer in notes.md)

1. Place AeonG in Wu/Pavlo's 5-axis MVCC table (topic 8): delta
   version storage, N2O ordering, GC-as-migration — which axes does
   the historical tier *change* vs merely extend, and does index
   management even apply to the KV tier?
2. M33: should FalkorDB's historical tier store matrix deltas or
   serialized per-object deltas — and what plays the anchor? (Hint:
   topic 20's delta matrices are already deltas; M30's snapshots are
   already anchors.)
3. After implementing `snapshot.rs` and running bench lane 3: your
   store uses one global `every`; AeonG uses per-object adaptive
   intervals. Construct the event distribution where global spacing
   loses worst, and estimate by how much using lane 3's replay_len.
4. The VP/VE/EP split gives three clocks per object. FalkorDB's
   topology lives in shared matrices, not per-object lists — what is
   the analogous split, and what goes wrong if `AT TIME` versions the
   whole matrix as one object?
5. Equation 2's legal check plus async migration means both stores are
   consulted on every temporal scan. When is the double consult worse
   than a synchronous-migration design, and why did AeonG accept it
   anyway? (Hint: whose critical path does each design tax?)

## Done when

You can state, without the paper, what one `FOR TT AS OF t` vertex
read costs end-to-end — visibility walk in current storage, KV anchor
seek, u deltas replayed — and predict which term lane 3's spacing dial
moves.

## References

**Papers**
- Hou, Zhao, Wang, Lu, Jin, Wen, Du — "AeonG: An Efficient Built-in
  Temporal Support in Graph Databases (Extended Version)" (PVLDB
  17(6): 1515–1527, 2024) — [arXiv:2304.12212](https://arxiv.org/abs/2304.12212)
- Wu, Arulraj, Lin, Xian, Pavlo — "An Empirical Evaluation of
  In-Memory Multi-Version Concurrency Control" (VLDB 2017) — topic 8;
  the 5-axis table question 1 asks you to fill

**Code**
- [AeonG](https://github.com/houououu/AeonG) — the memgraph fork; diff
  it mentally against topic 13's `src/storage/v2/`
- [memgraph](https://github.com/memgraph/memgraph) — topic 13 clone;
  the delta chains AeonG migrates instead of freeing
- This topic's `experiments/src/snapshot.rs` — AeonG's §4.2 storage
  bet in miniature; bench lane 3 is your private §7
