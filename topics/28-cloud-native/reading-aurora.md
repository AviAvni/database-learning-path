# Aurora: only the log crosses the network

Aurora is where "the log is the database" became a shipping OLTP
architecture: the writer sends storage nothing but redo records, and
six-way-replicated storage nodes materialize pages by replaying them.
This chapter builds the machine step by step — what a database writes,
why lifting that onto cloud storage fans it into several mirrored write
streams, and how quorums, LSNs, and a durability watermark replace both
checkpoints and 2PC — then hands you a section-by-section route through
the paper. It is the template every later disaggregated engine
(Socrates, Neon) either copies or argues with.

## The problem in one sentence

Run MySQL on network-replicated cloud block storage and one logical page
change fans out into **five distinct write streams** (Aurora's Figure 2,
§3.1: the redo log, the binlog, the modified data page, the double-write
buffer, and the FRM metadata file) — each one made durable synchronously,
each mirrored again by the storage layer and across availability zones —
so the bottleneck stops being disk IOPS and becomes network bytes, and
the question becomes: what is the *minimum* that must cross the network?

## The concepts, step by step

### Step 1 — what a database actually writes: pages, the buffer pool, and the redo log

> **In:** a table and an UPDATE statement. **Out:** the two things every
> classic engine puts on disk for that update — a modified **page** and a
> tiny **redo record** — and the size asymmetry between them that the rest
> of the chapter exploits. Nothing here is produced by an earlier step;
> this is the ground floor.

A classic engine stores tables as fixed-size **pages** (16 KB blocks in
MySQL/InnoDB), caches hot pages in an in-memory **buffer pool** (topic 6),
and — before modifying any page — appends a **redo record** to a
**write-ahead log (WAL)**: a small note saying "on page 8,312, change
bytes X..Y to Z". The **WAL rule** (topic 5): the record must be durable
*before* the page write counts as committed, so a crash can replay
("REDO") the log over old pages and reconstruct the new ones.

Note the asymmetry that everything below exploits. A redo record is
~50–200 bytes; the page it describes is 16 KB = 16,384 bytes. That is
**80×–320× smaller** (16,384 / 200 ≈ 82; 16,384 / 50 ≈ 328), and the page
is *derivable* from old page + redo record. So the log carries the same
information as the page write at under two percent of the bytes — which is
the entire reason the next steps can throw the page writes away.

### Step 2 — lift that to cloud storage naively and count the writes

> **In:** the page + redo record from Step 1. **Out:** a count of how many
> times those bytes cross the network when you drop a stock engine onto
> mirrored cloud block storage, and the observation that all but the redo
> stream are redundant.

Put the same engine on network-attached, mirrored block storage (Amazon
EBS) and every write stream MySQL emits crosses the network, and the
storage tier mirrors each again. Aurora's Figure 2 (§3.1, "The Burden of
Amplified Writes") enumerates them for a mirrored MySQL replica: the
**redo log**, the **binlog** (MySQL's second, *logical* log used for
replication), the **modified data page**, the **double-write buffer** (a
torn-page-protection area where each page is written *twice*), and the
**FRM metadata** file. Five streams, issued sequentially and
synchronously, then mirrored and shipped cross-AZ:

```
  classic MySQL on EBS:            Aurora:
  writer ──► data page   ─┐        writer ──► redo records ONLY
         ──► redo log     │
         ──► binlog       ├─►EBS          ┌──────┴──────┐ 4/6 quorum
         ──► double-write │           AZ1 ▓▓  AZ2 ▓▓  AZ3 ▓▓   (6 copies,
         ──► FRM metadata ┘           storage nodes replay      2 per AZ)
   (each mirrored + cross-AZ)         redo -> pages themselves
   five write streams per change      (Figure 2, §3.1)
```

Every byte of that traffic except the redo records is *redundant* — the
pages, the double-write copies, even the binlog are all re-derivable from
the redo log (Step 1). That observation is the whole paper. (Aurora does
not put a single "35×" on this fan-out; the 35 that shows up later, in
Step 6, is a *measured throughput* result, not the amplification factor —
keep the two apart.)

### Step 3 — the thesis: the log is the database

> **In:** the redundancy finding from Step 2. **Out:** the design move that
> deletes every redundant stream — ship only the log, let storage
> materialize pages — and the name for what the writer keeps.

**The log is the database.** In the paper's words (§3.2, "Offloading Redo
Processing to Storage"): *"the log is the database, and any pages that the
storage system materializes are simply a cache of log applications."* The
only thing the writer sends to storage is the redo log; storage nodes
materialize pages by replaying it, on demand or lazily. The paper is blunt
about what disappears: *"the only writes that cross the network are redo
log records. No pages are ever written from the database tier, not for
background writes, not for checkpointing, and not for cache eviction."*
No checkpoints from the writer, no dirty-page writeback, no double-write
buffer — storage does its own "compaction" (apply redo to pages) in the
background, near the data.

Squint and that is the **LSM** shape (topic 4) hiding inside a page store:
ship small sorted deltas, merge them into the big structure
asynchronously, close to where it lives. What the writer keeps is the
buffer pool — pages near the compute are now a *cache* of a log prefix,
not the authoritative copy.

### Step 4 — quorums and protection groups: surviving an AZ + one more

> **In:** the redo stream from Step 3, which is now the *only* durable
> state and therefore must not be lost. **Out:** the replication scheme
> (six copies, a 4/6 write quorum, a 3/6 read quorum) and the fault it is
> engineered to survive, worked against the arithmetic.

Replicate the log six ways or lose it. Aurora divides the database volume
into 10 GB **segments** (§2.2, "Segmented Storage"), each replicated as a
**protection group (PG)** of 6 copies, 2 per **availability zone** (AZ —
an isolated datacenter; one region has ≥ 3). A write succeeds on a
**quorum** (a minimum acknowledging subset) of **4 of 6**; a
recovery-time read needs **3 of 6**.

Work the two quorum rules (§2.1) on the real numbers. With V = 6 copies,
write quorum Vw = 4, read quorum Vr = 3:

- **Overlap:** Vr + Vw > V, i.e. 3 + 4 = 7 > 6. Any read set of 3 and any
  write set of 4 share at least 7 − 6 = **1 node**, so a read can never
  miss an acknowledged write.
- **Write self-consistency:** Vw > V/2, i.e. 4 > 3. Two write quorums
  overlap (4 + 4 = 8 > 6), so two conflicting writes can't both commit.

The sizes come from the fault model, and the two directions are
deliberately asymmetric. Lose an entire AZ (2 copies) *plus* one more node
(1 copy): 6 − 3 = 3 copies survive — still enough for a 3/6 read, so reads
stay available and no acknowledged write is lost (that is the "AZ+1"
promise). A 4/6 *write*, though, needs four live copies, which survives
the loss of a whole AZ (6 − 2 = 4 remain) but not AZ+1. So Aurora's
guarantee is read availability and no data loss through AZ+1, and write
availability through the loss of a single AZ. Small segments are
the repair story: re-replicating one 10 GB copy at 10 Gbps takes
10 GB ÷ (10 Gbps = 1.25 GB/s) = **8 s of transfer, ~10 s with overhead**
(the paper's figure), and thousands of segments repair in parallel, so the
window in which a second fault is fatal stays tiny. Note what the quorum
is *of*: log records for one 10 GB segment, not whole-database replicas.

### Step 5 — LSN and VDL: one monotonic counter instead of 2PC

> **In:** the per-segment quorum writes from Step 4, which acknowledge
> independently and out of order. **Out:** the single monotonic counter
> (LSN) and durability watermark (VDL) that turn "is this transaction
> committed?" into a point-on-a-line test, with no distributed vote.

Every redo record carries an **LSN** (log sequence number — a monotonic
byte-position in the log, assigned by the single writer). One
transaction's records can span several protection groups, which smells
like a distributed-atomicity problem needing **two-phase commit** (2PC:
all participants vote to *prepare*, then a coordinator decides commit or
abort — two network round trips and a blocking window). Aurora skips it
with a watermark: the **VDL** (Volume Durable LSN) is the highest LSN
below which *every* record has reached its 4/6 quorum.

Worked example (the paper's own sketch, §4.1): suppose records up to LSN
1007 have been issued but the record at 1001 is still short a quorum while
1002–1007 are fully acked. The VDL is **1000**, not 1007 — durability
stops at the first gap. Rules: a transaction is durable **iff** its commit
record's LSN ≤ VDL; on recovery, everything *above* the VDL is truncated.
That truncation is the design's analogue of **presumed abort** (an
incomplete tail is treated as "never happened") — obtained without any
prepare/commit rounds, because a single ordered log makes "what is
decided" a point on a line instead of a vote. ("Presumed abort" and, in
Step 6, "group commit" are our names for the analogy; Aurora reuses the
mechanisms, not the ARIES terminology.)

### Step 6 — commit and reads: what waits, what doesn't

> **In:** the VDL rule from Step 5 and the per-segment completeness
> bookkeeping the writer already keeps. **Out:** what a commit actually
> waits for (a log-quorum ack, never a page write) and why normal reads
> need no quorum at all — plus the one measured number worth carrying.

Commit is asynchronous (§4.2.2, "Commits"): the worker registers the
transaction's commit LSN and moves on; the acknowledgment fires later,
when the VDL advances past it. No page write is ever on the commit path,
and many transactions ride one quorum round — topic 5's **group commit**
(batching many fsyncs into one), in a network edition.

Reads are even better (§4.2.3, "Reads"): **no read quorum in the common
path.** The writer continuously tracks which segment has acknowledged
which LSN, so it directs each page read to one replica it *knows* is
complete for that LSN. The 3/6 read quorum is used only during crash
recovery, to rebuild the VDL when that bookkeeping is lost. Read replicas
get the same log stream and apply it to their buffer pools with **≤ 20 ms
lag** (§4.2.4) — but must not serve reads above the durable LSN.

The measured payoff (Table 1, §4.2.1, SysBench write-only, 100 GB, 30 min,
r3.8xlarge):

| configuration | transactions (30 min) | IOs / transaction |
|---|---|---|
| Mirrored MySQL | 780,000 | 7.4 |
| Aurora | 27,378,000 | 0.95 |

Work it: 27,378,000 ÷ 780,000 = **35.1× more transactions**, at
7.4 ÷ 0.95 = **7.8× fewer IOs per transaction** — and, the paper stresses,
this is *"despite amplifying writes six times"* for replication. So the
famous "35×" is throughput, and it comes from *removing* the Step 2 write
streams, not from any 35-fold amplification. (27,378,000 ÷ 1800 s ≈
15,200 committed transactions per second.)

### Step 7 — recovery: REDO already ran

> **In:** the continuous background redo-apply on storage nodes (Step 3)
> and the VDL (Step 5). **Out:** why crash recovery costs seconds instead
> of scaling with the checkpoint interval, and which ARIES phase survives.

Crash recovery in a classic engine is the expensive part: replay all redo
since the last checkpoint (seconds to minutes), then undo losers (topic
5's ARIES phases). In Aurora (§4.3, "Recovery"), storage nodes are
*always* replaying redo — REDO became continuous and distributed — so
there is no replay pass at the writer: establish the VDL (one 3/6 quorum
read per protection group), truncate above it, open for business. The
paper reports recovery *"generally under 10 seconds"* even after a crash
that was processing over 100,000 write statements per second. **UNDO**
(rolling back uncommitted transactions' visible effects) still exists but
runs lazily, online, after the database is already serving. The cost
profile flipped: recovery time no longer scales with the checkpoint
interval, so there is no checkpoint-frequency tuning knob at all.

## How to read the paper (with the concepts in hand)

- **§2.1 quorums, §2.2 segments** — Step 4 in the authors' words: 6 copies,
  2 per AZ, 4/6 write, 3/6 read, the AZ+1 argument, and 10 GB segments as
  the unit of repair. Check what the quorum is *of* (segment log records,
  not DB replicas).
- **§3.1 the burden, §3.2 the log ships alone** — Steps 2–3: Figure 2's
  five write streams, then "the log is the database" — no checkpoints, no
  dirty-page writeback, no double-write buffer; storage replays redo
  itself. Watch for the LSM shape hiding inside the page store.
- **§4.1 the log marches forward** — Step 5: LSN, VCL/CPL/VDL, and why a
  monotonic log replaces 2PC.
- **§4.2.2 commit** — Step 6's async commit: wait only for the 4/6 ack of
  the commit record's LSN (VDL advance), never a page write. Group commit
  falls out naturally.
- **§4.2.3 reads** — Step 6: no read quorum in the common path; the
  writer's completeness bookkeeping replaces it. Read quorums appear only
  in recovery (rebuilding the VDL). (This is §4.2.3, *not* §4.2.1 —
  §4.2.1 is "Writes", where Table 1 lives.)
- **§4.3 recovery** — Step 7: near-instant, because REDO is continuous at
  the storage tier and UNDO is lazy. Compare topic 5's ARIES phases
  one-for-one. (§6 is the performance evaluation, not recovery.)

## Numbers worth memorizing

- 6 copies / 4-of-6 write / 3-of-6 read / AZ+1 fault tolerance; 10 GB
  segments repaired in parallel (~10 s per segment on 10 Gbps; §2.2).
- Table 1 (§4.2.1): 35× more transactions (27,378,000 vs 780,000) at
  7.7× fewer IOs/transaction (0.95 vs 7.4), despite 6× replication
  amplification — the win is *removing* the five write streams, not an
  amplification ratio.
- Replica lag ≤ 20 ms (§4.2.4). Commit = log-quorum-ack only; recovery
  generally < 10 s, independent of checkpoint interval (§4.3).

## Questions to answer in notes.md

**Q1.** Why is 4/6 write + 3/6 read correct (Vw + Vr > V, and Vw > V/2)
but the paper still insists normal reads avoid quorums? What specifically
makes quorum reads expensive here — latency, or the loss of the "which
replica is complete" bookkeeping the writer already maintains?

**Q2.** The paper brags about avoiding 2PC. But there IS a multi-node
atomicity problem: one transaction's redo spans multiple protection
groups. How does the monotonic LSN + VDL rule replace the prepare/commit
round trips? What's the equivalent of "presumed abort"? (Everything above
VDL is truncated on recovery.)

**Q3 (the trade).** Storage replays redo, so pages near the writer are
always warm — but read replicas apply the same log to their buffer pools
with ≤ 20 ms lag and must NOT serve reads above the durable LSN. Map this
onto topic 15's replication-lag taxonomy: is an Aurora read replica sync,
async, or something the taxonomy doesn't name?

**Q4 (M28).** FalkorDB translation: the "redo record" for a graph is the
delta-matrix batch (topic 27's tick). If storage nodes could *apply* delta
matrices, compute would ship only deltas and storage would materialize
adjacency. What operation must the storage tier then support that S3
doesn't — and is that why Aurora runs its own storage fleet while Neon
keeps S3 behind a pageserver?

## Done when

Answer each before unfolding it.

- [ ] You can count the write streams a naive lift-and-shift of a
  page-based engine to cloud storage produces, and name them.
  <details><summary>Answer</summary>

  Five, from Figure 2 / §3.1 of a mirrored MySQL replica: the redo log,
  the binlog, the modified data page, the double-write buffer, and the FRM
  metadata file — each issued synchronously, then mirrored and shipped
  cross-AZ by the storage layer. All but the redo log are re-derivable
  from it.
  </details>

- [ ] You can state the thesis — only the log crosses the network — and
  what it removes.
  <details><summary>Answer</summary>

  "The log is the database" (§3.2): the writer sends storage nothing but
  redo records, and pages are "simply a cache of log applications." It
  removes writer-side checkpoints, dirty-page writeback, and the
  double-write buffer — storage applies redo to pages itself, in the
  background.
  </details>

- [ ] You can explain the quorum and protection-group scheme and what
  failure it survives.
  <details><summary>Answer</summary>

  6 copies of each 10 GB segment, 2 per AZ across 3 AZs (a protection
  group). Write quorum 4/6, read quorum 3/6; 4 + 3 > 6 guarantees overlap,
  4 > 6/2 keeps writes consistent. It preserves no-data-loss and read
  availability through the loss of an AZ plus one more node (AZ+1), and
  write availability through the loss of one AZ; 10 GB segments re-replicate
  in ~10 s so the double-fault window is tiny.
  </details>

- [ ] You can explain LSN and VDL and why one monotonic counter replaces
  2PC.
  <details><summary>Answer</summary>

  Every redo record gets a monotonic LSN from the single writer. The VDL
  is the highest LSN below which every record has a 4/6 quorum — durability
  stops at the first gap (1000, not 1007, if 1001 is missing). A
  transaction is durable iff its commit LSN ≤ VDL; recovery truncates
  above the VDL. A point on a line replaces a distributed vote, so there
  are no prepare/commit round trips.
  </details>

- [ ] You can say what waits at commit and what does not, and why REDO has
  already run at recovery.
  <details><summary>Answer</summary>

  Commit waits only for the 4/6 quorum ack that advances the VDL past the
  commit LSN — never for a page write, and often batched with other
  commits (group commit). Recovery is fast because storage nodes replay
  redo continuously, so there is no writer-side replay pass: establish the
  VDL, truncate above it, serve; UNDO runs lazily online. Reported < 10 s.
  </details>

- [ ] You have this topic's measured latency gap to argue against.
  <details><summary>Answer</summary>

  From FINDINGS row 28 / notes.md: local NVMe p50 0.10 ms vs raw S3 p50
  14.17 ms and p99 112.99 ms — a 140× median gap and a far worse tail.
  Aurora's answer is to keep pages materialized on its own storage fleet
  rather than read them from an object store on the hot path.
  </details>

## References

**Papers**
- Verbitski et al. — "Amazon Aurora: Design Considerations for High
  Throughput Cloud-Native Relational Databases" (SIGMOD 2017) — 12 pages,
  read whole. Every number in this guide is from this paper; sections
  cited inline (§2.1–2.2 durability, §3.1–3.2 the log thesis, §4.1–4.3
  the log/commit/read/recovery machinery, Table 1 in §4.2.1).
- Verbitski et al. — "Amazon Aurora: On Avoiding Distributed Consensus
  for I/Os, Commits, and Membership Changes" (SIGMOD 2018) — optional,
  for the quorum subtleties. No figure in this guide depends on it; treat
  its claims as follow-up reading, not as sources for the numbers above.
