# Socrates: durability is not availability

SQL Server rebuilt for Azure, with one architectural thesis: the tier
that makes a write durable and the tier that serves pages back have
opposite requirements, so they should be different services. This
chapter reads the four-tier decomposition and how it reuses — rather
than rewrites — the classic engine, the counterpoint to Aurora's
storage-layer rewrite.

## 1. Why read this right after Aurora

Aurora fused two jobs into its storage fleet: making writes *durable* and
making pages *available* for reads. Socrates' contribution is noticing these
have opposite requirements and splitting them:

| job | requirement | Socrates tier |
|---|---|---|
| durability | tiny, fast, sequential, SSD/NVM | **XLOG service** (the landing zone) |
| availability | big, warm, random-read, scalable | **Page servers** + XStore |

```
        compute primary ──► XLOG service (log landing zone, quorum, FAST)
           │                    │ fan-out (async)
           ▼ GetPage            ▼
        page servers (each owns a partition; RBPEX cache; replay log)
           │ backing store
           ▼
        XStore (Azure blob storage — cheap, slow, all versions)
```

- Commit latency = XLOG append only (like Aurora's 4/6, like Neon's
  safekeepers). Page servers consume the log *asynchronously* — they're
  caches, they can lag, crash, be rebuilt from XStore.
- **RBPEX** (Resilient Buffer Pool Extension): the buffer pool spilled to
  local SSD, *surviving restarts* — topic 6's buffer pool made durable-ish.
  Both compute and page servers run one.
- Snapshots/backup = XStore blob snapshots — nearly free, like Neon
  branches but coarser.

**Q1.** Socrates keeps the classic ARIES page-oriented redo (topic 5),
Aurora rearchitected around "log only". Yet both end with "compute ships
log; page service replays". What did Socrates get for *not* rewriting the
engine (hint: the paper's stated goal — reuse SQL Server code: HADR log
transport, buffer pool, etc.), and what does it pay in write amplification
between tiers?

**Q2.** The XLOG "landing zone" is small and fixed-size; the log is
truncated once page servers + XStore have consumed it. Map each stage onto
topic 5's WAL lifecycle (active tail → archived → checkpointed away) and
onto Neon: which Neon component is the landing zone, which is the
long-term log? (safekeepers; S3 via the pageserver's layer uploads.)

**Q3.** A page server is "just a cache of XStore + log replay" — so losing
one costs nothing durable. What does this do to the *tail latency* story
when a page server is cold (compare our tier_bench raw-S3 lane: p99
~113 ms)? Where does Socrates hide the misses? (RBPEX warm-up from
snapshot; requests hedged to replicas.)

**Q4 (M28).** FalkorDB single-writer translation: the XLOG/page-server
split says "durability tier ≠ serving tier". For a graph engine, the
durability tier is the AOF/replication log (topic 5); the serving tier is
materialized matrices. Does M28 need a page-server equivalent at all, or
does the compute node's own RBPEX-style local cache over object storage
suffice until read replicas (M15) enter? Write the one-paragraph answer in
notes.md.

## 2. The comparison table to carry forward

| | Aurora | Socrates | Neon |
|---|---|---|---|
| durability quorum | storage nodes (4/6) | XLOG landing zone | safekeepers (Paxos-ish) |
| page serving | same nodes | separate page servers | pageserver |
| cold tier | (internal) | XStore blobs | S3 layer files |
| engine rewrite? | storage layer yes | minimal (reuse) | none (stock Postgres + smgr hook) |
| caches | storage-side pages | RBPEX (compute AND page server) | pageserver layers + compute shared buffers |

## References

**Papers**
- Antonopoulos et al. — "Socrates: The New SQL Server in the Cloud"
  (SIGMOD 2019) — read §1-2 for the argument, §3-5 for the four
  tiers, skim performance
