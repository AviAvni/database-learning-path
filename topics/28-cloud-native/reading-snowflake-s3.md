# Reading guide — Snowflake (SIGMOD '16) + Building a Database on S3 (SIGMOD '08)

**Sources:**
- Dageville et al. — "The Snowflake Elastic Data Warehouse" (SIGMOD 2016)
  — read §1-4.
- Brantner, Florescu, Graf, Kossmann, Kraska — "Building a Database on S3"
  (SIGMOD 2008) — read §1-3 + §5; it's the prescient one.

## 1. Why these two together

The 2008 paper asked "can S3 *be* the database?" eight years early and hit
every wall; Snowflake is the first system that made the answer *yes* at
scale — by changing the question (analytics, immutable files, no
fine-grained updates).

```
   2008: pages on S3, updated in place ──► eventual consistency pain,
         no atomicity, pay-per-request shock       (all catalogued honestly)
   2016: IMMUTABLE micro-partitions on S3 + metadata service for the
         mutable bit ──► every 2008 problem dissolves except latency,
         which caching + columnar scans amortize
```

## 2. Building a Database on S3 — what to extract

- The design: B-tree pages stored as S3 objects; commit = push log records
  to SQS queues; "checkpointing" merges them into pages. Read §3's
  protocols — it's WAL-shipping (topic 5) built from queues.
- §5's honest accounting: no read-your-writes (S3 was eventually consistent
  until **Dec 2020** — now strong, which retroactively fixes half the
  paper), no multi-object atomicity (fixed Nov 2024-ish by conditional
  writes / If-Match CAS — which SlateDB's fencing now leans on), and
  request costs dominating at small page sizes.

**Q1.** List the three 2008 blockers (consistency, atomic multi-page
commit, cost-per-request) and, for each, what changed: S3 strong
consistency (2020), S3 conditional PUT/CAS (2024, enabling manifests as
commit points — see slatedb guide), and bigger immutable objects
(amortize request cost). Which blocker did systems *route around* rather
than wait for? (All three — via immutability + a small strongly-consistent
metadata tier.)

## 3. Snowflake — what to extract

- **Three layers**: object storage (data) / virtual warehouses (stateless
  compute, per-customer, elastically sized) / cloud services (metadata,
  transactions, optimization — the only stateful service).
- **Micro-partitions**: ~16 MB immutable columnar files (PAX layout,
  topic 11) with per-file min/max zone maps in the metadata layer. Updates
  = rewrite files; time travel = keep old file lists — a *table version is
  a list of files*, so cloning a table = copying a list. CoW branching
  again, the same trick as Neon branches and slatedb clones, at file
  granularity.
- **Pruning, not indexes**: no B-trees; min/max metadata prunes
  micro-partitions (topic 26's BRIN-shaped one-sided filter, at cloud
  scale).
- **Warehouse-local cache** on SSD; consistent hashing assigns files to
  nodes so caches don't overlap; work stealing when skewed.

**Q2.** Snowflake's shared-data claim: any warehouse can read any table,
scaling compute without data movement. What's the concurrency price —
where do write-write conflicts get decided, and why is "metadata service
does snapshot isolation over file lists" enough for a warehouse (vs an
OLTP engine, where Aurora needed per-page LSN machinery)?

**Q3.** Consistent-hash-with-cache vs shared-nothing partitioning
(topic 15): when a Snowflake warehouse resizes, no data reshuffles — only
cache assignments change. What workload property makes "cache locality is
a hint, not a correctness requirement" true here but false for, say, a
partitioned Raft group?

**Q4 (M28).** FalkorDB analytics reads (topic 22's read replicas / BI
export shape): micro-partition thinking says "publish immutable columnar
snapshots of the graph + a version manifest" instead of replicating the
live engine. Which graph representations tolerate immutable ~16 MB chunks
well (edge lists / CSR segments, topic 2) and which don't (in-place
delta-mutated matrices)? One paragraph in notes.md.
