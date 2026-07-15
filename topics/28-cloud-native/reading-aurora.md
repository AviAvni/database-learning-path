# Aurora: only the log crosses the network

Aurora is where "the log is the database" became a shipping OLTP
architecture: the writer sends storage nothing but redo records, and
six-way-replicated storage nodes materialize pages by replaying them.
This chapter builds the machine step by step — what a database writes,
why lifting that onto cloud storage multiplies it 35×, and how quorums,
LSNs, and a durability watermark replace both checkpoints and 2PC —
then hands you a section-by-section route through the paper. It is the
template every later disaggregated engine (Socrates, Neon) either
copies or argues with.

## The problem in one sentence

Run MySQL on network-replicated cloud block storage and one logical page
change fans out into **~35 network IOs** (the paper's Table 1: data page +
redo log + binlog + double-write buffer, each mirrored again by the
storage layer) — the bottleneck is no longer disk IOPS, it's network
bytes, so the question becomes: what is the *minimum* that must cross the
network?

## The concepts, step by step

### Step 1 — what a database actually writes: pages, the buffer pool, and the redo log

A classic engine stores tables as fixed-size **pages** (16 KB blocks in
MySQL), caches hot pages in an in-memory **buffer pool**, and — before
modifying any page — appends a **redo record** to a **write-ahead log
(WAL)**: a small note saying "on page 8,312, change bytes X..Y to Z"
(~50–200 bytes, vs the 16 KB page it describes). The WAL rule: the record
must be durable *before* the page write counts as committed, so a crash
can replay ("REDO") the log over old pages and reconstruct the new ones.
Note the asymmetry that everything below exploits: the log entry is two
orders of magnitude smaller than the page, and the page is *derivable*
from old page + log.

### Step 2 — lift that to cloud storage naively and count the writes

Put the same engine on network-attached, mirrored block storage (EBS) and
every one of those write streams — pages, redo, binlog (MySQL's second,
logical log for replication), plus the double-write buffer (a
torn-page-protection area where each page is written *twice*) — crosses
the network, and the storage tier mirrors each again:

```
  classic MySQL on EBS:            Aurora:
  writer ──► data pages ─┐         writer ──► redo records ONLY
         ──► redo log    ├─►EBS           ┌──────┴──────┐ 4/6 quorum
         ──► binlog      │            AZ1 ▓▓  AZ2 ▓▓  AZ3 ▓▓   (6 copies,
         ──► double-write┘            storage nodes replay      2 per AZ)
  (each mirrored again!)              redo -> pages themselves
  ~35x more write traffic per page change (paper's Table 1: network IOs)
```

Every byte of that traffic except the redo records is *redundant* — the
pages are just materializations of the log (Step 1). That observation is
the whole paper.

### Step 3 — the thesis: the log is the database

**The log is the database.** The only thing the writer sends to storage is
the redo log; storage nodes materialize pages by replaying it, on demand
or lazily. No checkpoints from the writer, no dirty-page writeback, no
double-write buffer — storage does its own "compaction" (apply redo to
pages) in the background. Squint and that's the LSM shape (topic 4)
hiding inside a page store: ship small sorted deltas, merge them into the
big structure asynchronously, near the data. What the writer keeps is the
buffer pool — pages near the compute are now a *cache* of a log prefix,
not the authoritative copy.

### Step 4 — quorums and protection groups: surviving an AZ + one more

Replicate the log six ways or lose it. Aurora divides the database volume
into 10 GB **segments**, each replicated as a **protection group** of 6
copies, 2 per **availability zone** (AZ — an isolated datacenter; one
region has ≥3). A write succeeds on a **quorum** (a minimum subset) of
4/6; a recovery-time read needs 3/6. Because 4 + 3 > 6, any read quorum
overlaps any write quorum — you can't read a state that misses an
acknowledged write. The sizes come from the fault model: lose an entire
AZ (2 copies) *plus* one more node and 4/6 writes still have 3 survivors
— readable — while 3/6 reads still succeed ("AZ+1"). Small segments are
the repair story: re-replicating 10 GB at 10 Gbps takes **~10 s**, and
thousands of segments repair in parallel, so the window where a second
fault is fatal stays tiny. Note what the quorum is *of*: log records for
one 10 GB segment, not whole-database replicas.

### Step 5 — LSN and VDL: one monotonic counter instead of 2PC

Every redo record carries an **LSN** (log sequence number — a monotonic
byte-position in the log, assigned by the single writer). One
transaction's records span multiple protection groups, which smells like
a distributed-atomicity problem needing **two-phase commit** (2PC: all
participants vote to prepare, then a coordinator decides commit or
abort — two network round trips and a blocking window). Aurora skips it
with a watermark: the **VDL** (volume durable LSN) is the highest LSN
below which *every* record has reached its 4/6 quorum. Rules: a
transaction is durable iff its commit record's LSN ≤ VDL; on recovery,
everything *above* the VDL is truncated. That truncation is the
"presumed abort" of the design — incomplete tail = never happened —
without any prepare/commit rounds, because a single ordered log makes
"what is decided" a *point on a line* instead of a vote.

### Step 6 — commit and reads: what waits, what doesn't

Commit is asynchronous: the worker registers the transaction's commit LSN
and moves on; the acknowledgment fires when the VDL advances past it. No
page write is ever on the commit path, and many transactions ride one
quorum round — topic 5's group commit (batching many fsyncs into one),
network edition. Reads are even better: **no read quorum in the common
path**. The writer continuously tracks which segment has acknowledged
which LSN, so it directs each page read to one replica it *knows* is
complete for that LSN. The 3/6 read quorum is used only during crash
recovery, to rebuild the VDL when that bookkeeping is lost. Replicas get
the same log stream and apply it to their buffer pools with ≤ 20 ms lag —
but must not serve reads above the durable LSN.

### Step 7 — recovery: REDO already ran

Crash recovery in a classic engine is the expensive part: replay all redo
since the last checkpoint (seconds to minutes), then undo losers
(topic 5's ARIES phases). In Aurora, storage nodes are *always* replaying
redo — REDO became continuous and distributed — so there is no replay
pass at the writer: establish the VDL (one 3/6 quorum read per protection
group), truncate above it, open for business in seconds. UNDO (rolling
back uncommitted transactions' visible effects) still exists but runs
lazily, online, after the database is already serving. The cost profile
flipped: recovery time no longer scales with checkpoint interval, so
there's no checkpoint-frequency tuning knob at all.

## How to read the paper (with the concepts in hand)

- **§2 quorums** — Step 4 in the authors' words: 6 copies, 2 per AZ,
  4/6 write, 3/6 read, the AZ+1 argument, and 10 GB segments as the unit
  of repair. Check what the quorum is of (segment log records, not DB
  replicas).
- **§3 the log ships alone** — Step 3: no checkpoints, no dirty-page
  writeback, no double-write buffer; storage replays redo itself. Watch
  for the LSM shape hiding inside the page store.
- **§4.2 commit** — Step 6's async commit: wait only for the 4/6 ack of
  the commit record's LSN (VDL advance), never a page write. Group commit
  falls out naturally.
- **§4.2.1 reads** — Step 6: no read quorum in the common path; the
  writer's completeness bookkeeping replaces it. Read quorums appear only
  in recovery (rebuilding the VDL).
- **§6 recovery** — Step 7: near-instant, because REDO is continuous at
  the storage tier and UNDO is lazy. Compare topic 5's ARIES phases
  one-for-one.

## Numbers worth memorizing

- 6 copies / 4-of-6 write / AZ+1 fault tolerance; 10 GB segments repaired
  in parallel (~10 s per segment on 10 Gbps).
- 35× network amplification eliminated vs MySQL-on-mirrored-EBS.
- Commit = log-quorum-ack only; recovery = seconds (no REDO replay at
  compute).

## Questions to answer in notes.md

**Q1.** Why is 4/6 write + 3/6 read correct (W+R > N) but the paper still
insists reads avoid quorums? What specifically makes quorum reads expensive
here — latency, or the loss of the "which replica is complete" bookkeeping?

**Q2.** The paper brags about avoiding 2PC. But there IS a multi-node
atomicity problem: one transaction's redo spans multiple protection groups.
How does the monotonic LSN + VDL (volume durable LSN) rule replace the
prepare/commit round trips? What's the equivalent of "presumed abort"?
(Everything above VDL is truncated on recovery.)

**Q3 (the trade).** Storage replays redo, so pages near the writer are
always warm — but replicas apply the same log to their buffer pools with
≤ 20 ms lag and must NOT serve reads above the durable LSN. Map this onto
topic 15's replication lag taxonomy: is an Aurora read replica sync,
async, or something the taxonomy doesn't name?

**Q4 (M28).** FalkorDB translation: the "redo record" for a graph is the
delta matrix batch (topic 27's tick). If storage nodes could *apply* delta
matrices, compute would ship only deltas and storage would materialize
adjacency. What operation must the storage tier then support that S3
doesn't — and is that why Aurora runs its own storage fleet while Neon
keeps S3 behind a pageserver?

## References

**Papers**
- Verbitski et al. — "Amazon Aurora: Design Considerations for High
  Throughput Cloud-Native Relational Databases" (SIGMOD 2017) —
  12 pages, read whole
- Verbitski et al. — "Amazon Aurora: On Avoiding Distributed Consensus
  for I/Os, Commits, and Membership Changes" (SIGMOD 2018) — optional,
  for the quorum subtleties
