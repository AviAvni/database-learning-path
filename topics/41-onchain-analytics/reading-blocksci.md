# BlockSci: when the graph database is the wrong answer

This is the guide in topic 41 that is really about databases. BlockSci's authors wanted to run
scientific analyses over the whole Bitcoin blockchain and found the obvious tools — general graph
databases — hundreds of times too slow. Their diagnosis is a chain of design decisions worth
memorising, because it applies to any workload with the same shape: the data is *append-only*, so
the snapshots you analyse are *static*, so the ACID machinery of a transactional database is
*unnecessary*, so the right engine is an **in-memory analytical database** with a layout tuned
for sequential access. They then benchmark that claim against Neo4j, Memgraph and RedisGraph —
which makes Table 3 the one published measurement in this book where FalkorDB's own ancestor is a
baseline. Read it as a specification for what a graph engine has to do to win these queries back.

## The problem in one sentence

**The Bitcoin blockchain is 260 GB of append-only graph-structured data that researchers want to
scan end to end, repeatedly — and the systems built for graph *traversal* lose that workload by
two to three orders of magnitude.**

## The concepts, step by step

### Step 1 — The chain of design decisions

> BlockSci's design starts with the observation that blockchains are append-only databases;
> further, the snapshots used for research are static. Thus, the ACID properties of transactional
> databases are unnecessary. This makes an in-memory analytical database the natural choice.

Each link is doing work. *Append-only* means no in-place updates except length-preserving ones
(an existing output being marked spent). *Static snapshots* means no concurrent writers during
analysis. *No ACID* means no write-ahead log, no MVCC versions, no lock manager — all the
machinery topics 5, 8 and 9 build, deleted because the workload does not need it.

And a claim you should push back on before accepting:

> We conjecture that the use of a traditional, distributed transactional database for blockchain
> analysis has infinite COST (Configuration that Outperforms a Single Thread), in the sense that
> no level of parallelism can outperform an optimized single-threaded implementation.

COST is McSherry, Isard and Murray's metric, and "infinite COST" is a strong claim: not "slower"
but "cannot be fixed by adding machines". The justification is that blockchain data is
graph-structured and therefore hard to partition — which is topic 36's vertex-cut problem
arriving from the other side.

### Step 2 — The transaction record, and 19% deliberate duplication

Figure 2's layout, with everything sized to the byte:

```
   Transaction header               Input / Output entry (128 bits each)
   ┌──────────────────┬─────┐       ┌──────────────────┬─────┐
   │ Real size        │  32 │       │ Spent/spending tx│  32 │
   │ Base size        │  32 │       │ Address ID       │  32 │
   │ Locktime         │  32 │       │ Value            │  60 │
   │ Input count      │  16 │       │ Address type     │   4 │
   │ Output count     │  16 │       └──────────────────┴─────┘
   │ Inputs  …128 each│     │
   │ Outputs …128 each│     │
   └──────────────────┴─────┘
```

Note the 60-bit value and 4-bit address type packed into one 64-bit word, and the 32-bit ids —
which is where Table 4's fourth row comes from: moving to 64-bit ids would take the Bitcoin
transaction graph from **50.09 GB to 69.26 GB**.

The important decision is that inputs and outputs are stored **inline with the transaction**, not
in normalized side tables:

> The layout stores both inputs and outputs as part of a transaction, resulting in a small amount
> of duplication (a space cost of about 19%), but resulting in a significant speedup for
> sequential iteration compared to a normalized layout.

Table 4 prices the alternatives on the Dec 2019 chain (489M transactions, 1.198B inputs, 1.302B
outputs): current 50.09 GB, normalized 40.50 GB, fee-cached 54.00 GB. Normalizing saves 19% "but
it leads to a steep drop in performance for typical queries such as max-fee." That is topic 12's
argument, in a security paper.

### Step 3 — The snapshot illusion

Three properties that look contradictory:

1. The transactions table is updated on disk as new blocks arrive.
2. The table is memory-mapped and shared between all running instances.
3. Each instance sees a snapshot that never changes unless it explicitly reloads.

They coexist because the append-only property means the state at any past block height is
**reconstructible from the current state**: a `chain` object records the height at
initialization, the analysis library intercepts accesses to outputs spent in later blocks and
rewrites them as unspent, and accesses past the recorded height are prevented. Cheap MVCC for a
data structure that only grows — worth comparing to what topic 8 has to build when updates are
arbitrary.

### Step 4 — Memory mapping buys parallelism for free

> Memory mapping also allows multithreaded parallel processing with no additional effort. Recall
> that if a file is mapped into memory by multiple processes, they use the same physical memory
> for the file. The file has only one writer (the parser); it is not modified by the analysis
> library. Thus, synchronization between different analysis instances isn't necessary.

One writer, many readers, no synchronisation, and the disk layout is identical to the memory
layout so "loading the blockchain simply involves memory-mapping this file... no new memory needs
to be allocated to enable object-oriented access to the data". Loading Bitcoin takes about **4
minutes**; a parallel pass over every transaction, input and output takes **0.9 seconds** on a
16-vCPU instance.

The contrast the paper draws is sharp: "With a disk-based database, analyses tend to be I/O
bound, with little or no benefit from multiple CPUs, whereas BlockSci is CPU-bound, and
performance scales roughly linearly with the number of virtual CPUs."

### Step 5 — The parser, and two distribution facts worth stealing

Parsing is the hard part, because it is inherently sequential and stateful — you must link each
input to the output it spends, and each output to an address id. Two measured facts about
Bitcoin drive the optimisation, and both are the kind of thing you should look for in your own
workload:

- **88% of inputs spend outputs created in the last 4000 blocks.** Recency, so a small cache wins.
- **Only 8.6% of Bitcoin addresses are used more than once, but those account for 51% of all
  occurrences.** A heavy tail, so caching *multi-use* addresses specifically captures half the
  traffic in a fraction of the space.

The resulting three-tier structure: a **bloom filter** of all seen addresses (negatives are
always correct, so it eliminates lookups for addresses that cannot exist), a **multi-use address
cache** that never evicts, and a **RocksDB** key-value store with LRU for the rest. Parsing to
block 610,695 takes **5.5 hours**, and "incremental updates are essentially instantaneous".

### Step 6 — Address linking is union-find, and it takes minutes

> These heuristics create links (edges) in a graph of addresses. By iterating over all
> transactions and applying the union-find algorithm on the contained addresses we generate
> clusters of addresses... Clustering takes only a few minutes, allowing the analyst to recompute
> and compare clusters with different heuristics.

This is Meiklejohn's Heuristic 1 and 2 (the other guide) at production scale, and BlockSci's
cluster-size distribution is the empirical evidence for the collapse this topic's lane 3
reproduces:

```
   ~474 million clusters total
   ~380 million are single addresses
   ~93 million have 2–20,000 addresses
     809 have over 20,000 addresses
       1 SUPERCLUSTER with over 17,000,000 addresses
```

and the paper's own verdict on that last line: "it is likely that the supercluster above is a
result of such a collapse."

### Step 7 — Table 3, and how to read it

25 million transactions, block height 262,176, average of five runs, in **seconds**:

| query | BlockSci C++ (ST) | (MT) | Fluent (ST) | Neo4j w/o idx | Neo4j w/ idx | RedisGraph | Memgraph |
|---|---|---|---|---|---|---|---|
| Tx locktime > 0 | 0.31 | 0.03 | 1.37 | 7.84 | 0.05 | 1.85 | 16.44 |
| Max output value | 0.46 | 0.03 | 3.91 | 26.63 | 24.55 | 4.48 | 40.08 |
| Calculate fee | 0.57 | 0.03 | 2.79 | 302.73 | 303.69 | — | 187.02 |
| Satoshi Dice address | 0.49 | N/A | 0.54 | 0.95 | 0.99 | 2.56 | 45.91 |
| Zero-conf outputs | 5.47 | 0.32 | 18.17 | 192.01 | 207.41 | 1488.93 | 59.96 |
| Locktime change | 7.57 | 0.45 | 18.21 | 208.95 | 213.59 | — | 122.98 |

`—` means did not finish. The summary: "2–16× compared to the best results for graph traversal,
and hundreds times faster for many sequential queries."

Read the *rows*, not the totals, and the picture is not "graph databases are slow":

- **`Satoshi Dice address`** is a point lookup plus a local expansion. Neo4j with an index does it
  in 0.95 s against BlockSci's 0.49 s — a factor of two, on the query a graph database is *for*.
- **`Calculate fee`** and **`Locktime change`** are full scans with arithmetic over every input
  and output. Those are the 300×–500× losses, and they are lost on layout: a property graph pays
  a pointer chase per input where BlockSci pays a sequential read.
- **`Tx locktime > 0`** with a Neo4j index (0.05 s) actually *beats* BlockSci single-threaded
  (0.31 s), because an index turns a scan into a lookup.

The honest reading is that this is a benchmark of *storage layout on scan-shaped queries*, and the
authors say as much: "we deem this a reasonable compromise: while BlockSci aims to be a
general-purpose tool, analysts may decide to ignore data irrelevant to their goals when choosing
a different database." For M41 that is a design brief, not a verdict — the question is whether a
graph engine can keep a columnar side-store for exactly these queries, which is topic 32's HTAP
question with a different label.

### Step 8 — The interface tax

Table 2 measures the same anomalous-fee query through three Python paradigms:

```
   pure Python ............ (single-threaded: too slow to report) / 18 hours multithreaded
   C++ builtin helper ..... 6 min 59 s / 58.6 s
   fluent interface ....... 38.3 s / 8.7 s
```

The fluent interface is a lazily-evaluated internal DSL — `chain.blocks.txes.where(lambda tx:
tx.fee > 10**7).to_list()` — that compiles method chains down to C++. It is **7–11× faster than
the helper method** and only **3–5× slower than hand-written single-threaded C++**. That is a
query planner in miniature, and the reason it exists is topic 10's: the gap between what a user
writes and what the machine should run is worth closing automatically.

## How to read the paper (with the concepts in hand)

- **§1 Introduction.** The three pain points and the COST conjecture. Note the "hundred times
  more memory than required" remark about cloud instances — the whole design assumes vertical
  scaling wins.
- **§2.1 Recording and importing.** Skim. The interesting line is *why* Monero is unsupported:
  its mixins add edges the transaction-graph model cannot express.
- **§2.2 Parser.** Read against Step 5. Find the 88%-recency and 8.6%/51% address statistics and
  ask what the equivalents are in a workload you know.
- **§2.3 BlockSci Data + Figures 2, Table 4.** The record layout and the memory-layout table. Work
  out the 50.09 GB from `24 N_tx + 16 N_in + 16 N_out` yourself.
- **§2.4 Analysis library.** The snapshot illusion (three contradictory properties and their
  resolution) and the memory-mapping-gives-parallelism argument. This is the best page in the
  paper.
- **§2.5 Programmer interface + Table 2.** The fluent DSL. Read the three query formulations.
- **§2.6.1–2.6.2 + Tables 1, 3.** The runtimes and the graph-database comparison. Read Table 3 by
  row as in Step 7; do not stop at the summary sentence.
- **§2.6.5 + Table 4.** Memory layouts and the 19% normalization trade.
- **§3.1–3.2.** Two applications worth skimming for the graph reasoning: multisig usage leaking
  access-control structure, and using multisig type-matching as a *new* change-address heuristic
  that "allows identifying change addresses even though previously known heuristics do not allow
  such a determination" — a direct extension of the other guide's Definition 4.3.
- **After the paper.** Do exercise 7: re-implement this topic's `Chain` with the Figure 2 record
  in one flat byte buffer, memory-map it, and re-time lane 2. Then decide how much of BlockSci's
  advantage is layout and how much is the absence of transaction machinery.

## Questions to answer in notes.md

1. Walk the chain "append-only ⟹ static snapshots ⟹ no ACID ⟹ in-memory analytical". Which link
   breaks first for a graph database serving live writes, and what does that cost you?
2. The paper conjectures *infinite* COST for a distributed transactional database on this
   workload. State the strongest version of that claim and one workload shape that would refute
   it.
3. In Table 3, Neo4j-with-index beats single-threaded BlockSci on `Tx locktime > 0` (0.05 vs
   0.31 s) and loses `Calculate fee` by 500×. Characterise precisely which query shapes go each
   way, and write the rule as a one-line predicate on the query plan.
4. The inline input/output layout costs 19% space and is kept anyway. Compute the break-even:
   how much faster does the sequential scan have to be for that to pay, given Table 1's numbers?
5. BlockSci's supercluster has over 17 million addresses; Meiklejohn's had 1.6 million on a
   smaller chain. Is the growth evidence that clustering got worse, that the chain got bigger, or
   that collapse is superlinear? Design a measurement that would distinguish the three, using
   lane 3's knobs.

## Done when

- [ ] You can recite the four-link design chain and say what each link deletes.
- [ ] You can draw the Figure 2 transaction record from memory, with bit widths.
- [ ] You can explain the snapshot illusion in three sentences.
- [ ] You can read Table 3 by row and say which queries a graph engine legitimately loses and why.
- [ ] You can name the two Bitcoin distribution facts the parser exploits.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Kalodner, Möser, Lee, Goldfeder, Plattner, Chator, Narayanan. *BlockSci: Design and applications
  of a blockchain analysis platform.* USENIX Security 2020 —
  [PDF](https://www.usenix.org/system/files/sec20-kalodner.pdf).
- Code: [citp/BlockSci](https://github.com/citp/BlockSci).
- McSherry, Isard, Murray. *Scalability! But at what COST?* HotOS 2015 — the metric §1 invokes.
- Local exercise: topic README exercise 7 — the Figure 2 record over this topic's synthetic chain.
- Topic 12 (columnar storage) — the same locality argument; topic 32 (HTAP) — the scan-vs-traverse
  split this table is really about; topic 36 (sharding) — why graph data resists partitioning.
