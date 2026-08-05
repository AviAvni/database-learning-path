# The RUM conjecture: optimize two, pay with the third

After the B-tree and LSM papers give the triangle its concrete corners, this
short vision paper names the trade-off every storage structure lives inside:
read, update, and memory overhead cannot all be bounded at once. It doesn't
build anything — it hands you the design compass the rest of the curriculum
steers by. This chapter defines the three overheads one at a time on real
numbers, puts a structure at each corner, and only then states the conjecture in
the authors' exact words, which are narrower than the version people quote. Read
the paper *after* the two engine papers.

Citations are to the EDBT 2016 proceedings version (Athanassoulis, Kester, Maas,
Stoica, Idreos, Ailamaki, Callaghan), six pages, sections §1–§6.

## The problem in one sentence

Every index design promises fast reads, cheap updates, and a small footprint —
this six-page paper claims the promise is structurally impossible: **an access
method that sets an upper bound on two of the three overheads also sets a lower
bound on the third** (§3), and §2 proves the special case with three
constructions you can check by hand.

## The concepts, step by step

### Step 1 — read overhead (RO): everything you touched vs what you wanted

> **In:** a query that needs some specific base data, and a structure that
> keeps auxiliary data to find it faster.
> **Out:** RO as a ratio with both sides named, and its value for four
> structures on this topic's own dataset.

The paper's vocabulary first, because the definitions are relative to it (§2):

- **base data** — the actual rows/tuples the system stores.
- **auxiliary data** — everything an access method keeps *in addition*, to make
  operations faster: index nodes, filters, zone maps, sorted copies.

**Read overhead (RO)** is then, verbatim from §2, "the ratio between the total
amount of data read including auxiliary and base data, divided by the amount of
retrieved data". Note both halves: the numerator includes the index traversal,
and the denominator is what you actually wanted, not what you scanned. The
paper's own example: "when traversing a B+-Tree to access a tuple, the RO is
given by the ratio between the total data accessed (including the data read to
traverse the tree and the base data) and the base data intended to be read."

The theoretical minimum is **1.0** — §2: "implying that the base data is always
read and updated directly and no extra bit of memory is wasted".

Put numbers on it with this topic's own shootout parameters — `N` = 1,080,000
records of 100 bytes, 4,096-byte pages, so `B` = 40 tuples per block and the base
data is `N/B` = 27,000 blocks. For a **point lookup of one record**:

```text
structure          blocks read     bytes read   RO = bytes read / 100 B
─────────────────  ──────────────  ───────────  ───────────────────────
perfect hash index  1                  4,096              41
B+-tree             4                 16,384             164
levelled LSM (T=10) 17                69,632             696
sorted column       21                86,016             860
unsorted column     13,500        55,296,000         552,960
```

The block counts are Table 1's complexity column, evaluated (Step 4 shows the
substitutions). RO is what you feel as query latency: it counts the I/Os and
cache lines a lookup burns.

### Step 2 — update overhead (UO): everything you wrote vs what changed

> **In:** one logical change to one record.
> **Out:** UO as a ratio, its floor, and the number for the same five
> structures.

**Update overhead (UO)**, §2: "the ratio between the size of the physical
updates performed for one logical update, divided by the size of the logical
update" — and, crucially, "the amount of updates applied to the auxiliary data
*in addition to* the updates to the main data". So a B+-tree's UO counts the
leaf page *and* every interior node the split dirties *and* the WAL copy, over
the 100 bytes you actually meant to change.

The paper calls this "the write amplification", which is exactly the term the
LSM guide uses — RUM's contribution is generalising it to any structure, not
just merge trees. Ideal is again **1.0**.

Same dataset, one insert:

```text
structure          blocks written  bytes written  UO = bytes written / 100 B
─────────────────  ──────────────  ─────────────  ─────────────────────────
unsorted column     1                     4,096              41
perfect hash index  1                     4,096              41
levelled LSM (T=10) 1.11                  4,542              45
B+-tree             4                    16,384             164
sorted column       13,500           55,296,000         552,960
```

Note the two columns invert almost exactly: the unsorted column is UO-cheapest
and RO-worst; the sorted column is the reverse. That inversion is the conjecture
in miniature, and Step 5 makes it formal.

UO is what you feel as write throughput and SSD wear. §2 says so directly:
"storage with limited endurance (like flash-based drives) favors minimizing the
update overhead".

### Step 3 — memory overhead (MO): footprint vs live data

> **In:** a structure sitting on disk or in RAM.
> **Out:** MO as a ratio, and the gap between what the paper's model predicts
> and what this repo actually measured.

**Memory overhead (MO)**, §2: "the space overhead induced by storing auxiliary
data … the ratio between the space utilized for auxiliary and base data, divided
by the space utilized for base data". The paper also calls it "the space
amplification" — the same quantity `notes.md` reports for this topic.

Work it for a B+-tree over the same dataset. The index entry is a key plus a
child pointer; take 8 bytes each:

```text
base data    1,080,000 × 100 B                = 108.0 MB
index bytes  1,080,000 × 16 B                 =  17.3 MB   (dense)
             ÷ 0.69 expected B-tree fill      =  25.0 MB   (Comer's ln 2 result)
MO = (108.0 + 25.0) / 108.0                   = 1.23×
```

Table 1's levelled-LSM row gives a closed form instead: index size
`O(N·T/(T−1))`, so at size ratio `T = 10`, MO = 10/9 = **1.11×**.

Now compare those model figures against what this repo actually measured
([FINDINGS.md](../../FINDINGS.md) row 1), on the same 1.08 M records:

| engine | family | logical | on disk | space amp (MO) | model said |
|---|---|---|---|---|---|
| fjall | LSM | 108.0 MB | 48.4 MB | **0.45×** | 1.11× |
| redb | B-tree (CoW) | 108.0 MB | 6833.9 MB | **63.28×** | 1.23× |

A **140× spread**, and both engines miss the model by a wide margin in opposite
directions. Both misses are informative, and neither is a defect in the paper:

- **fjall lands below 1.0** because it LZ4-compresses value bytes into the
  sorted run. §5 anticipates this precisely and refuses to count it as a
  counterexample: "Orthogonally to the tension between the three overheads …
  compression is often used to reduce the amount of data to be moved. This
  tradeoff between computation (compressing/decompressing) and data size does
  not affect the fundamental nature of the RUM Conjecture." Compression buys MO
  with CPU, which is a fourth axis the triangle deliberately does not draw.
- **redb lands at 63×** because Table 1's `O(N/B)` index size assumes a settled
  tree, not one being rebuilt. `notes.md` explains the mechanism: random key
  order plus 1,080 separate durable batch commits means each commit copies every
  page on the root-to-leaf path and cannot free the old ones until a later commit
  releases them. The base-data denominator is right; the auxiliary-data numerator
  is dominated by transient copies the complexity column never modelled.

Whenever this topic needs a space-amp figure, use 0.45× and 63.28×, not a
remembered "B-trees are about 1.5×".

MO is what you feel as disk and RAM bills — and since caches hold fewer useful
entries when MO is high, bad MO quietly worsens effective RO too. §4's Figure 2
makes that vertical coupling explicit; Step 6 returns to it.

### Step 4 — one structure per corner

> **In:** the three ratios from Steps 1–3.
> **Out:** the paper's Figure 1 map with its real corner labels, and Table 1's
> complexities evaluated on this topic's `N` and `B`.

§4 (*RUM in Practice*, not §3) is where the triangle lives — Figure 1, "Popular
data structures in the RUM space". Its corners and the structures §4's prose
assigns to each:

```text
                        Read Optimized
                              ▲
                              │      Point & Tree indexes:
                              │      hash indexes, B-Trees, Tries,
                              │      Prefix B-Trees, Skiplists
                              │
                    Adaptive structures (middle region):
                    Database Cracking, Adaptive Merging,
                          Adaptive Indexing
                              │
     ●────────────────────────┴────────────────────────●
 Write Optimized                                Space Optimized

 Differential structures:                Approximate / sparse indexes:
 LSM, Partitioned B-tree (PBT),          Bloom filters, count-min sketches,
 MaSM, Stepped Merge, Positional         lossy bitmaps, approximate tree
 Differential Tree, LA-Tree, FD-Tree     indexing, ZoneMaps, SMA,
                                         Column Imprints
```

Two things to notice about the real figure that the folk version loses. First,
the corners are named by *what they optimise*, not by "RO = 1" — no real
structure sits at a vertex; the vertices are the unreachable ideals of Step 5's
propositions. Second, the middle is not empty: §4 gives it to adaptive methods,
which "balance the tradeoffs online across a larger area of the design space"
rather than sitting at one point.

Table 1 is the quantitative version, and it is worth evaluating rather than
admiring. Its parameters: `N` dataset size in tuples, `m` query result size, `B`
block size in tuples, `P` partition size, `T` the LSM level size ratio, `MEM`
memory in pages. With this topic's shootout — `N` = 1,080,000, 100-byte records,
4,096-byte pages so `B` = 40, and `T` = 10:

| structure | point query (→ RO) | insert (→ UO) | index size (→ MO) |
|---|---|---|---|
| perfect hash | `O(1)` = **1** | `O(1)` = **1** | `O(N/B)` = 27,000 blocks |
| B+-tree | `O(log_B N)` = log₄₀ 1.08e6 = 3.77 → **4** | `O(log_B N)` = **4** | `O(N/B)` = 27,000 blocks |
| levelled LSM | `O(log_T(N/B)·log_B N)` = 4.43 × 3.77 = **16.7** | `O(T/B · log_T(N/B))` = 0.25 × 4.43 = **1.11** | `O(N·T/(T−1))` = **1.11·N** |
| sorted column | `O(log₂ N)` = **20.0** | `O(N/B/2)` = **13,500** | `O(1)` |
| unsorted column | `O(N/B/2)` = **13,500** | `O(1)` = **1** | `O(1)` |

Read the LSM row against the B+-tree row: **4.2× worse point reads (16.7 vs 4),
3.6× better inserts (1.11 vs 4)**. That single pair of ratios is topic 1's
dichotomy, in the paper's own complexity model, and it is what the shootout is
measuring. §4's own summary of the table: "ZoneMaps have the smaller size — being
a sparse index, but Hash Indexes offer the fastest point queries, while B+-Trees
offer the fastest range queries."

### Step 5 — the conjecture itself, and the three propositions under it

> **In:** the three ratios, each with an ideal of 1.0.
> **Out:** the exact statement (which is about *bounds*, not about *approaching
> 1.0*), plus the three §2 propositions that motivate it, each checked on
> numbers.

The statement, §3, word for word:

> **The RUM Conjecture.** An access method that can set an upper bound for two
> out of the read, update, and memory overheads, also sets a lower bound for the
> third overhead.

Two precisions worth holding onto, because the popular paraphrase loses both.
It is about **bounds**, not about "approaching 1.0" — the claim is that bounding
any two *forces* a floor under the third, whatever values those two bounds take.
And it is a **conjecture**: §6 says the paper "shows through the RUM Conjecture
that creating the ultimate access method is infeasible", but nowhere is a proof
offered, and §5 spends its length on a research roadmap rather than on a
theorem.

§2 states a strictly weaker **Hypothesis** first — "an access method that is
optimal with respect to one of the read, update, and memory overheads, cannot
achieve the optimal value for both remaining overheads" — and then backs it with
three constructions on a deliberately trivial model: `N` fixed-size integers,
one per block, block ID = `blkID`, workload of point queries, updates, inserts
and deletes. Each construction is checkable by hand:

**Prop. 1 — `min(RO) = 1.0 ⇒ UO = 2.0 and MO → ∞.`** Store each value in the
block whose `blkID` equals the value itself. Lookup is one direct address, so
RO = 1.0 exactly. But the array is sparse: the paper's example, the relation
{1, 17}, needs 17 blocks to hold 2 values — MO = 8.5 for two elements, and
unbounded in general "since, in the general case, we cannot anticipate what would
be the maximum value ever inserted". With this topic's 8-byte keys the address
space is 2⁶⁴ blocks for 1.08 M live values, so MO ≈ 1.7 × 10¹³. UO is 2.0
because changing a value must empty the old block and fill the new one.

**Prop. 2 — `min(UO) = 1.0 ⇒ RO → ∞ and MO → ∞.`** Append every update to a log
and never reorganise. UO = 1.0 exactly. Both other overheads then grow *without
bound as updates arrive*, because every superseded version stays and every read
must consider all of them: "for minimum UO, both RO and MO perpetually increase
as updates are appended."

**Prop. 3 — `min(MO) = 1.0 ⇒ RO = N and UO = 1.0.`** Store a dense array, keep
no auxiliary data, update in place. MO = 1.0, and UO is *also* 1.0 — you touch
only the base data you meant to. The full price lands on one axis: a worst-case
point query scans everything, RO = `N`. On this topic's dataset that is 27,000
blocks, matching the "unsorted column" row of Table 1.

Prop. 3 is the one to sit with, because it is the title of the guide: it pins
**two** overheads at their theoretical optimum simultaneously, and the third
goes to `N`. That is "optimize two, pay with the third" as a construction rather
than a slogan.

Now watch the conjecture bite on a fix. Start from Prop. 3's dense array: MO =
1.0, UO = 1.0, RO = N. Fix RO by keeping the array sorted and binary-searching
it — RO drops to log₂ N = 20 — and UO immediately jumps to `N/B/2` = 13,500,
exactly Table 1's sorted-column row. Fix *that* by buffering updates in a log in
front of the sorted array and merging periodically, and you have just derived
the LSM: UO falls back to 1.11, while RO rises to 16.7 and MO rises to 1.11·N.
The improvement did not remove the cost; it moved it, every time, and it moved
it to whichever axis you were not bounding.

### Step 6 — how to use it: a compass, not a theorem

> **In:** a tuning knob, a benchmark claim, or a memory hierarchy.
> **Out:** three ways the paper says to use the triangle, and the one class of
> claim it lets you reject on sight.

**Knobs are positions, not settings.** §5 lists the parameters that move a
structure around the space by name: "the fan-out of B+-Trees, the number of
partitions in PBT, the number of sorted runs in MaSM", and, in its wishlist,
"B+-Trees that have dynamically tuned parameters, including tree height, node
size, and split condition, in order to adjust the tree size, the read cost, and
the update cost at runtime". So Bloom-filter bits per key trades MO for RO;
compaction eagerness (levelled vs tiered) trades UO for RO; page fill factor
trades MO for UO. A design review starts with "what does the workload need?" and
then *chooses where to pay*. Monkey (topic 4) turns exactly this into a formal
optimisation: allocate a fixed memory budget across per-level Bloom filters to
minimise RO at fixed MO.

**The triangle applies per level of the memory hierarchy, not once globally.**
§4's Figure 2 is the part almost nobody quotes: "The RUM tradeoffs, however,
still hold for each level individually … The RUM tradeoffs can also be viewed
vertically rather than horizontally. For example, the RO_n read and the UO_n
update overheads at memory level n can be reduced by storing more data, updates,
or meta-data, at the previous level n−1, which results, at least, in a higher
MO_{n−1}." That is a one-sentence theory of caching, buffer pools, and
memtables: every one of them buys RO and UO at level *n* by spending MO at level
*n−1*. It is also why the topic 0 latency ladder and this paper are the same
argument seen from two angles.

**What the compass rules out.** Any claim that a structure improved one overhead
with *no* movement elsewhere. Find where the cost went before believing the
benchmark — and if the answer is "compression", §5 says that is a
computation-for-space trade sitting orthogonal to the triangle, so ask what it
cost in CPU instead. fjall's 0.45× is exactly that case.

## How to read the paper (with the concepts in hand)

Six pages, roughly one hour. The real section map, since the numbering is easy
to misremember:

| § | Title | Contains |
|---|---|---|
| 1 | Introduction | The framing; the tagline "Optimize Two at the Expense of the Third" |
| 2 | The RUM Overheads | RO/UO/MO definitions; the Hypothesis; **Props. 1, 2, 3** |
| 3 | The RUM Conjecture | Twenty lines. Just the statement |
| 4 | RUM in Practice | **Figure 1** (the triangle), **Table 1**, Figure 2 (memory hierarchy), cache-oblivious methods |
| 5 | Building RUM Access Methods | Figure 3; the roadmap; the compression note |
| 6 | Summary | Two paragraphs |

Read in this order:

1. **§2** — Steps 1–3. Make sure you can restate all three definitions with the
   *denominator* named, and can re-derive Props. 1–3 on the paper's array of
   integers before moving on. This is 60% of the paper's content.
2. **§3** — Step 5. It is one paragraph; read it twice and note that it says
   "upper bound"/"lower bound", not "optimal".
3. **§4** — Step 4. Reproduce Figure 1's corners and their regions from memory,
   then evaluate Table 1 on your own `N` and `B` rather than reading the
   complexities as decoration. Do not skip Figure 2's paragraph on the memory
   hierarchy; it is the most reusable idea in the paper.
4. **§5** — Step 6, plus a grading exercise: the roadmap was written in 2016 and
   names five specific wishes (tunable B+-trees, updatable approximate indexes,
   morphing access methods, update-friendly bitmaps, log-plus-filter methods).
   Mark each as delivered, partly delivered, or not, with 2026 evidence.
5. **§6** — skim.

## Questions to answer in notes.md

1. Place this topic's shootout results on the triangle. Which measured number
   from `notes.md` is an MO, and what would you have to instrument to get RO and
   UO for fjall and redb? (Neither is currently measured — say what the lane
   would have to record.)
2. §2's Prop. 3 pins *two* overheads at 1.0 simultaneously. Reconcile that with
   the §3 conjecture: does Prop. 3 contradict it, and if not, which of the three
   quantities is the one being "bounded" in the conjecture's sense?
3. Table 1 gives levelled LSM an index size of `O(N·T/(T−1))` — 1.11·N at
   T = 10. fjall measured 0.45×. Explain the direction of the discrepancy using
   §5's compression paragraph, and say what the model would have to add to
   predict a number below 1.0.
4. The paper never names durability as an axis. Take a WAL: score it on RO, UO
   and MO using §2's definitions (is a WAL auxiliary data?), and then say
   whether "every engine carries one anyway" is a point on the triangle or
   evidence that the model is incomplete. Defend your answer from the §2 text,
   not from intuition.
5. Where does FalkorDB's matrix adjacency sit? Score it on all three axes for a
   *sparse* graph, and name the two structures later topics use to move it (see
   topics 20 and 26).

## The one-line takeaway

There is no best index, only a workload-shaped position on a three-way frontier
— "which engine is better" is an ill-posed question until the workload is named,
and any benchmark showing an improvement with no offsetting cost has simply not
measured the axis that paid.

## Done when

Answer each before unfolding it.

- [ ] You can define RO, UO and MO as ratios, naming the numerator and denominator of each, in the paper's auxiliary-vs-base-data vocabulary.

<details>
<summary>Answer</summary>

All three are stated in §2 relative to **base data** (the rows the system
stores) versus **auxiliary data** (anything an access method keeps in addition:
index nodes, filters, sorted copies).

- **RO** = total data read, *auxiliary plus base*, ÷ the amount of data actually
  retrieved. Ideal 1.0.
- **UO** = size of the physical updates performed for one logical update,
  *auxiliary plus base*, ÷ the size of the logical update. The paper calls this
  "the write amplification". Ideal 1.0.
- **MO** = space used for auxiliary plus base data ÷ space used for base data.
  The paper calls this "the space amplification". Ideal 1.0.

Ideal 1.0 means, in §2's words, "the base data is always read and updated
directly and no extra bit of memory is wasted".

</details>

- [ ] You can state the conjecture in the authors' words and say precisely what it does *not* claim.

<details>
<summary>Answer</summary>

§3: "An access method that can set an upper bound for two out of the read,
update, and memory overheads, also sets a lower bound for the third overhead."

What it does not claim: (a) that the overheads must approach 1.0 — the claim is
about *any* pair of upper bounds, not about optimal ones; (b) that this is
proven — it is a conjecture, and no proof appears in the paper, only §2's three
constructions and §4's survey; (c) that it covers every cost. §5 explicitly puts
compression outside it, as a computation-versus-size trade that "does not affect
the fundamental nature of the RUM Conjecture", and the paper never mentions
durability, concurrency or latency variance at all.

</details>

- [ ] You can reproduce §2's three propositions and check each on numbers.

<details>
<summary>Answer</summary>

On the paper's model — `N` fixed-size integers, one per block, addressed by
`blkID`:

- **Prop. 1**: `min(RO) = 1.0 ⇒ UO = 2.0 and MO → ∞`. Direct addressing, block
  ID = value. The paper's own case, the relation {1,17}, occupies 17 blocks for
  2 values (MO = 8.5); with 8-byte keys the address space is 2⁶⁴ blocks for
  1.08 M live values, MO ≈ 1.7 × 10¹³. UO = 2.0 because a value change empties
  one block and fills another.
- **Prop. 2**: `min(UO) = 1.0 ⇒ RO → ∞ and MO → ∞`. Append-only log; both other
  overheads "perpetually increase as updates are appended".
- **Prop. 3**: `min(MO) = 1.0 ⇒ RO = N and UO = 1.0`. Dense array, in-place
  updates, no auxiliary data — two overheads at the optimum at once, and a
  full scan for every point query. On this topic's dataset, RO = 27,000 blocks,
  which is Table 1's unsorted-column row.

</details>

- [ ] You can name each corner of Figure 1 by what it optimises, give two real structures per corner, and say what occupies the middle.

<details>
<summary>Answer</summary>

Figure 1's corners are **Read Optimized** (top), **Write Optimized** (bottom
left), **Space Optimized** (bottom right) — named by goal, not by "RO = 1",
because no real structure reaches a vertex.

- Read: hash indexes, B-Trees (also Tries, Prefix B-Trees, Skiplists).
- Write, which §4 calls *differential structures*: LSM, Partitioned B-tree
  (also MaSM, Stepped Merge, Positional Differential Tree, LA-Tree, FD-Tree).
- Space: Bloom filters, ZoneMaps (also count-min sketches, lossy bitmaps,
  approximate tree indexing, Small Materialized Aggregates, Column Imprints).

The middle holds **adaptive** methods — Database Cracking, Adaptive Merging,
Adaptive Indexing — which §4 says "balance the tradeoffs online across a larger
area of the design space" instead of sitting at one point.

</details>

- [ ] You can evaluate Table 1 on a concrete `N` and `B` and read topic 1's dichotomy out of two rows.

<details>
<summary>Answer</summary>

With `N` = 1,080,000 records of 100 B and 4,096-byte pages (so `B` = 40 tuples
per block), `T` = 10:

- **B+-tree**: point query `O(log_B N)` = log₄₀ 1.08e6 = 3.77 → 4 I/Os; insert
  `O(log_B N)` = 4; index size `O(N/B)` = 27,000 blocks.
- **Levelled LSM**: point query `O(log_T(N/B)·log_B N)` = 4.43 × 3.77 = 16.7
  I/Os; insert `O(T/B · log_T(N/B))` = 0.25 × 4.43 = 1.11 I/Os; index size
  `O(N·T/(T−1))` = 1.11·N.

Ratio of the two rows: the LSM pays **4.2× more I/O per point read** and
**3.6× less per insert**. That is topic 1's whole dichotomy, in complexity form,
before a single benchmark runs.

</details>

- [ ] You have placed this topic's own measured result on the triangle, and can say which axis each engine is spending and why the model missed both.

<details>
<summary>Answer</summary>

[FINDINGS.md](../../FINDINGS.md) row 1, same 108.0 MB of records: fjall
**0.45×** space amp, redb **63.28×** — a 140× spread. Both are MO measurements.

fjall is below the model's 1.11× because it LZ4-compresses value bytes; §5 puts
that trade outside the triangle ("this tradeoff between computation … and data
size does not affect the fundamental nature of the RUM Conjecture"), so the real
price is CPU, an axis the figure does not draw. redb is far above the model's
~1.23× because Table 1's `O(N/B)` index size describes a settled tree, and
`notes.md` shows this lane never lets it settle: random key order plus 1,080
durable batch commits means every commit copies the whole root-to-leaf path and
cannot free the predecessors yet. Neither number refutes the conjecture; both
show that a complexity column is not a measurement.

</details>

- [ ] You wrote answers to all five questions in notes.md, including the WAL scoring and where FalkorDB's matrix adjacency sits.

<details>
<summary>Answer</summary>

The WAL question has no clean answer inside the model, and noticing that is the
point: by §2's definition a WAL is auxiliary data, so it inflates UO (every byte
written twice) and MO (the retained tail) while doing nothing for RO — a strict
loss on the triangle. Every engine carries one anyway, which means the axis it
buys — crash durability — is simply not in the model. Say so explicitly rather
than forcing it onto a corner.

FalkorDB's matrix adjacency is read-optimised: a sparse-matrix representation of
adjacency makes traversal a linear-algebra kernel (low RO for multi-hop), pays
UO on every edge insert into a compressed matrix, and pays MO badly when the
graph is sparse and the representation is not. That MO cost is exactly why delta
matrices and roaring bitmaps appear in topics 20 and 26 — both are moves toward
the Space Optimized corner.

</details>

## References

**Papers**
- Athanassoulis, Kester, Maas, Stoica, Idreos, Ailamaki, Callaghan — "Designing
  Access Methods: The RUM Conjecture" (EDBT 2016, pp. 461–466) —
  [PDF](https://openproceedings.org/2016/conf/edbt/paper-12.pdf) — §2 for the
  definitions and Props. 1–3; §3 for the one-paragraph conjecture; §4 for
  Figure 1, Table 1 and Figure 2; §5 for the roadmap and the compression note.
  Six pages, ~1 h; read after the B-tree and LSM papers so the triangle has
  concrete corners
- O'Neil, Cheng, Gawlick, O'Neil — "The Log-Structured Merge-Tree" (1996) —
  the write-optimised corner's founding member, cited as [44]
- Dayan, Athanassoulis, Idreos — "Monkey: Optimal Navigable Key-Value Store"
  (SIGMOD 2017) — the same group turning §5's "tunable RUM balance" into an
  actual optimisation problem; topic 4 implements it

**This repo**
- [FINDINGS.md](../../FINDINGS.md) row 1 — the measured MO figures (fjall
  0.45×, redb 63.28×) this guide scores against Table 1; `./verify.sh 01`
- [notes.md](notes.md) — why redb's 63× is the adversarial case, not a defect
- [reading-lsm-paper.md](reading-lsm-paper.md) — the write-optimised corner's
  cost model, including the `K·(r+1)` write-amplification derivation
- [reading-comer-btree.md](reading-comer-btree.md) — the read-optimised corner's
  fanout and ln 2 ≈ 69% utilisation results, used in Step 3's MO arithmetic
