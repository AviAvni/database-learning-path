# The RUM conjecture: optimize two, pay with the third

After the B-tree and LSM papers give the triangle its concrete corners, this
short vision paper names the trade-off every storage structure lives inside:
read, update, and memory overhead cannot all approach optimal at once. It
doesn't build anything — it hands you the design compass the rest of the
curriculum steers by. This chapter defines the three overheads one at a
time with real numbers, puts a structure at each corner, and only then
states the conjecture. Read the paper *after* the two engine papers.

## The problem in one sentence

Every index design promises fast reads, cheap updates, and a small
footprint — this 6-page paper claims the promise is structurally
impossible: push any two of the three overheads toward their ideal of 1.0
and the third acquires a floor that rises.

## The concepts, step by step

### Step 1 — read overhead (RO): how much you read vs how much you needed

Read overhead is the ratio of data a structure actually reads to the data
strictly required to answer the query — **RO = bytes read ÷ bytes needed**,
ideal 1.0. Concretely, for a point lookup of one 100-byte row among 1M
rows:

- **unsorted log**: scan ~half the file — ~50 MB read for 100 bytes needed
  ⇒ RO ≈ 500,000;
- **B+tree**: 3 page reads of 4 KB — 12 KB for 100 bytes ⇒ RO ≈ 120;
- **array indexed directly by key**: read the one entry ⇒ RO ≈ 1.

RO is what you feel as query latency: it counts the IOs and cache lines a
lookup burns.

### Step 2 — update overhead (UO): how much you write vs how much changed

Update overhead is bytes physically written per byte logically changed —
**UO = bytes written ÷ bytes updated**, ideal 1.0. For an 8-byte update:

- **append-only log**: write the 8 bytes (plus a small header) ⇒ UO ≈ 1;
- **B-tree**: rewrite the whole 4 KB page holding the entry ⇒ UO = 512 —
  before counting the WAL copy or a split;
- **sorted array**: insert in the middle shifts ~n/2 entries ⇒ UO ≈ n/2 —
  50 MB moved to add 100 bytes to a 1M-row array.

UO is what you feel as write throughput and SSD wear — it is write
amplification generalized to any structure.

### Step 3 — memory overhead (MO): footprint vs live data

Memory (space) overhead is total bytes the structure occupies per byte of
live data — **MO = bytes stored ÷ bytes of live data**, ideal 1.0.
Concretely:

- **densely packed sorted array**: no pointers, no slack ⇒ MO ≈ 1;
- **B-tree**: pages average ~69% full, plus interior nodes ⇒ MO ≈ 1.5;
- **tiered LSM**: overwritten versions linger across runs until compaction
  ⇒ MO ≈ 2 or worse — plus Bloom filters, which are *extra* bytes stored
  purely to reduce RO.

MO is what you feel as disk and RAM bills — and since caches hold fewer
useful entries when MO is high, bad MO quietly worsens effective RO too.

### Step 4 — one structure per corner

Score any structure on all three axes and a pattern appears: the classics
each pin two overheads near 1 and bleed on the third. A **sorted array** is
read- and memory-optimal (RO ≈ 1 binary search, MO ≈ 1) but update-hostile
(UO ≈ n/2). A **log** is update-optimal (UO ≈ 1) but read-hostile (RO ≈ n)
and MO grows with dead versions. The **B+tree** buys good reads with page
slack (MO) and page-granularity writes (UO); the **LSM** buys good updates
with multi-component reads (RO) and lingering versions (MO). The paper's §3
maps them onto a triangle — reproduce it:

```
                       RO = 1 (read-optimal)
                            ▲
                  B+tree ●  │  ● hash index
                            │
             LSM leveled ●  │  ● sorted array (static)
                            │
        LSM tiered ●        │        ● bitmap/bloom (approximate)
                            │
   log ●────────────────────┴────────────────────● compressed archive
 UO = 1 (update-optimal)                   MO = 1 (space-optimal)
```

Topic 1's dichotomy is just two dots on this map: B-tree near the read
corner, LSM stretched along the update edge (leveled closer to reads,
tiered closer to updates).

### Step 5 — the conjecture itself

The conjecture: an access method can push any two of RO, UO, MO toward 1.0,
but the third then has a hard lower bound that *grows* as the other two
approach 1. It is not a proven theorem — hence "conjecture"; the paper is
explicit about this — but no counterexample has shown up, and every fix
you try demonstrates it. Watch it happen: the sorted array has RO ≈ 1 and
MO ≈ 1, so the conjecture says updates must hurt — UO ≈ n/2, check. Fix UO
by buffering updates in a log in front of the array, and you've just
invented an LSM — and RO (check every buffer) and MO (dead versions) rise
on cue. The improvement didn't remove the cost; it moved it.

### Step 6 — how to use it: a compass, not a theorem

The practical payoff is that every tuning knob is a *position on the
triangle*, not a setting with a correct value. Bloom filter bits/key trades
MO for RO. Compaction eagerness (leveled vs tiered) trades UO for RO. Page
fill factor trades MO for UO. So a design review starts with "what does the
workload need?" and then *chooses where to pay* — and Monkey (topic 4)
turns exactly this into a formal optimization problem, allocating memory
across Bloom filters to minimize RO at fixed MO. What the compass rules
out: any claim that a structure improved one overhead with *no* movement
elsewhere — find where the cost went before believing the benchmark.

## How to read the paper (with the concepts in hand)

1. **§1–2** — the RO/UO/MO definitions, i.e. Steps 1–3; make sure you can
   compute all three for a plain sorted array (RO≈1, UO≈n/2 shifts, MO≈1)
   and a log (UO≈1, RO≈n, MO grows) before moving on.
2. **§3 (the map)** — Step 4: the paper places real structures on the
   triangle. Reproduce the diagram from memory.
3. **§4 (moving on the map)** — Steps 5–6, the punchline for this
   curriculum: knobs are *positions*, not settings. Bloom bits/key trades
   MO for RO. Compaction eagerness trades UO for RO. Page fill factor
   trades MO for UO. Monkey (topic 4) turns this into an actual
   optimization problem.
4. **§5 (research directions)** — skim; grade its 2016 predictions with 2026
   hindsight (adaptive/learned indexes, versioned data — how did they age?).

## Questions to answer in notes.md

1. Place your engine_shootout results on the triangle: which measured number is RO,
   UO, MO for fjall and redb?
2. Where does FalkorDB's matrix adjacency sit? (Dense-ish matrix: MO poor for sparse
   graphs — that's why delta matrices + roaring exist, topics 20/26.)
3. What's the RUM position of a WAL by itself? Why does *every* engine carry one
   anyway? (Durability isn't in the triangle — it's an orthogonal axis the paper
   deliberately excludes.)

## The one-line takeaway

There is no best index, only a workload-shaped position on a three-way frontier —
"which engine is better" is an ill-posed question until the workload is named.

## References

**Papers**
- Athanassoulis, Kester, Maas, Stoica, Idreos, Ailamaki, Callaghan —
  "Designing Access Methods: The RUM Conjecture" (EDBT 2016) —
  [PDF](https://stratos.seas.harvard.edu/files/stratos/files/rum.pdf) —
  ~6 pages, 1 h; read after the B-tree and LSM papers so the triangle
  has concrete corners
