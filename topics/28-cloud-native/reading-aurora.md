# Aurora: only the log crosses the network

Aurora is where "the log is the database" became a shipping OLTP
architecture: the writer sends storage nothing but redo records, and
six-way-replicated storage nodes materialize pages by replaying them.
This chapter extracts the quorum design, the commit path, and the
recovery story — the template every later disaggregated engine
(Socrates, Neon) either copies or argues with.

## 1. The one-sentence thesis

**The log is the database.** The only thing the writer sends to storage is
the redo log; storage nodes materialize pages by replaying it, on demand or
lazily. Everything else in the paper is consequences.

```
  classic MySQL on EBS:            Aurora:
  writer ──► data pages ─┐         writer ──► redo records ONLY
         ──► redo log    ├─►EBS           ┌──────┴──────┐ 4/6 quorum
         ──► binlog      │            AZ1 ▓▓  AZ2 ▓▓  AZ3 ▓▓   (6 copies,
         ──► double-write┘            storage nodes replay      2 per AZ)
  (each mirrored again!)              redo -> pages themselves
  ~35x more write traffic per page change (paper's Table 1: network IOs)
```

## 2. What to extract, section by section

- **§2 quorums**: 6 copies, 2 per AZ; write quorum 4/6, read quorum 3/6.
  Sized so an entire AZ + one more node can fail without losing writes
  (AZ+1 fault model). Note what the quorum is *of*: log records for a 10 GB
  *protection group* segment, not whole-database replicas.
- **§3 the log ships alone**: no checkpoints from the writer, no dirty page
  writeback, no double-write buffer. Storage does its own "compaction"
  (apply redo to pages) — the LSM shape (topic 4) hiding inside a page
  store.
- **§4.2 commit**: async — commit waits only for the 4/6 ack of the commit
  record's LSN (VDL advance), not for any page write. Group commit falls
  out naturally (topic 5's fsync batching, network edition).
- **§4.2.1 reads**: no read quorum in the common path! The writer tracks
  which segment has what LSN, reads from a known-complete replica. Read
  quorum only for crash recovery (rebuilding the VDL).
- **§6 recovery**: near-instant — no REDO pass at the writer (storage is
  always replaying); UNDO is lazy, online. Compare topic 5's ARIES phases:
  Aurora made REDO continuous and distributed.

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

## 3. Numbers worth memorizing

- 6 copies / 4-of-6 write / AZ+1 fault tolerance; 10 GB segments repaired
  in parallel (~10 s per segment on 10 Gbps).
- 35× network amplification eliminated vs MySQL-on-mirrored-EBS.
- Commit = log-quorum-ack only; recovery = seconds (no REDO replay at
  compute).

## References

**Papers**
- Verbitski et al. — "Amazon Aurora: Design Considerations for High
  Throughput Cloud-Native Relational Databases" (SIGMOD 2017) —
  12 pages, read whole
- Verbitski et al. — "Amazon Aurora: On Avoiding Distributed Consensus
  for I/Os, Commits, and Membership Changes" (SIGMOD 2018) — optional,
  for the quorum subtleties
