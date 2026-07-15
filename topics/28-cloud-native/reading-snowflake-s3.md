# Snowflake and the 2008 S3 paper: immutability dissolves the walls

A pair of papers, eight years apart, that bracket the "database on
object storage" question: one catalogues every pathology honestly, the
other quietly ticks the whole checklist by making the data immutable
and hoisting the mutable bit into a small metadata service. This
chapter builds the ideas step by step — what object storage actually
is, why the 2008 in-place design hit every wall, and how immutability
plus a tiny metadata tier dissolves each one — then routes you through
both papers. Q1 tracks which pathologies S3 itself has since fixed.

## The problem in one sentence

Object storage is ~10× cheaper per stored byte than provisioned
replicated disks and infinitely elastic, but in 2008 it offered no
read-your-writes consistency, no multi-object atomicity, ~15 ms GETs, and
a per-request bill — so the question is whether a database can live there
at all, and the two papers answer "not like this" (2008) and "yes, like
*this*" (2016).

```
   2008: pages on S3, updated in place ──► eventual consistency pain,
         no atomicity, pay-per-request shock       (all catalogued honestly)
   2016: IMMUTABLE micro-partitions on S3 + metadata service for the
         mutable bit ──► every 2008 problem dissolves except latency,
         which caching + columnar scans amortize
```

## The concepts, step by step

### Step 1 — what object storage actually is

An object store (S3) is a key → blob service: `PUT` a whole object,
`GET` it (or a byte range) back, `LIST` keys — and nothing else. No
in-place update (a "modify" is a full re-PUT under the same key), no
append, no rename, and in 2008 no atomic operations across objects and
only *eventual consistency* (a GET after a PUT could return the old
version). What you get in exchange: ~$0.023/GB·month, 11-nines
durability, cross-AZ replication by default, unbounded capacity — and a
price *per request* (~$0.0004 per 1000 GETs), which quietly punishes any
design built from many small objects. Latency is the other tax: ~15 ms
median, ~100 ms+ tail per GET (topic 28 README §0), vs ~0.1 ms for local
NVMe.

### Step 2 — the 2008 attempt: pages on S3, updated in place

The 2008 paper does the direct translation: store B-tree pages (topic 1's
fixed-size disk blocks) as individual S3 objects and update them in
place, with commit implemented by pushing log records to SQS queues and
"checkpointing" merging them into the page objects — WAL-shipping
(topic 5) built from queues. §3's protocols are worth reading; §5's
accounting is the honest part. Three walls, all structural:

- **No read-your-writes**: eventual consistency means a transaction can
  fail to see its own committed page — snapshot reads are unbuildable.
- **No multi-object atomicity**: a commit touching 10 page-objects has no
  way to make them appear together; a crash mid-checkpoint leaves a
  half-updated tree.
- **Request costs dominate at small pages**: at 4–16 KB objects, the
  per-request fee exceeds the storage fee — the design bleeds money on
  bookkeeping IOs.

The 2008 design fails because every wall is hit by the same behavior:
*mutating small shared objects*.

### Step 3 — the fix: never modify an object

Make every data object **immutable** — written once, read many, replaced
never, only superseded — and all three walls soften at once. Consistency:
an immutable object can't be stale (any copy anywhere is *the* version;
eventual consistency of overwrites stops mattering because there are no
overwrites). Atomicity: since data objects never change, the only mutable
thing left is the *list of which objects constitute the table* — one
small piece of state. Cost: immutable files can be big (megabytes, not
kilobytes), amortizing the per-request fee by 1000×. Snowflake's unit is
the **micro-partition**: an immutable, ~16 MB, columnar file (PAX layout,
topic 11) covering some slice of rows.

### Step 4 — hoist the mutable bit: a table version is a list of files

With data frozen, updates become file replacement: an UPDATE rewrites the
affected micro-partitions as *new* files and publishes a new **table
version** — which is nothing but a list of file names. That list lives in
a small, strongly-consistent **metadata service**, and swapping it is the
atomic commit point. Consequences fall out for free:

```
   table v41 = [f1, f2, f3]          time travel: keep old versions'
   UPDATE rewrites f2 -> f2'                       file lists around
   table v42 = [f1, f2', f3]         clone: copy the LIST (bytes: ~KB),
   commit = publish v42 (one         not the files (bytes: ~TB) —
   metadata write, atomic)           CoW branching at file granularity
```

Time travel = read an old list. Zero-copy clone = copy a list. This is
the same copy-on-write move as Neon's branches and SlateDB's clones, at
file granularity — and the whole 2008 atomicity wall reduced to one
strongly-consistent metadata write.

### Step 5 — Snowflake's three layers

Snowflake's architecture is Steps 3–4 drawn as boxes: **object storage**
holds the immutable micro-partitions (all the bytes, none of the state
machine); **virtual warehouses** are stateless compute clusters, sized
and spun per customer, that read any table (shared-data: any compute can
reach any data, unlike shared-nothing where data is bound to nodes); and
**cloud services** — the only stateful tier — holds metadata,
transactions (snapshot isolation over file lists, per Step 4), and query
optimization. Compute elasticity is free *because* compute owns nothing:
resizing a warehouse moves no data.

### Step 6 — pruning, not indexes

Snowflake has no B-trees at all. Instead the metadata service keeps
per-micro-partition **zone maps** (min/max values per column per file),
and a query skips every file whose min/max range can't contain a match —
"pruning". On data naturally clustered by load time (event tables), a
`WHERE date = ...` predicate prunes 99%+ of files before a single GET is
issued. It's topic 26's BRIN-shaped one-sided filter at cloud scale: it
can prove absence, never presence — good enough when scans are columnar
and the per-file cost is 15 ms.

### Step 7 — clawing back the 140×: caches and consistent hashing

Micro-partitions still live 15 ms away, so each warehouse keeps a
**local SSD cache** of the files it reads. To keep caches from all
holding the same hot files, file→node assignment uses **consistent
hashing** (each file hashes to a preferred node, so a resize remaps only
a fraction of files — and no data moves, only cache assignments, since
S3 remains the source of truth). Skew is handled by **work stealing**
(idle nodes take file-scan tasks from busy ones, reading from S3
directly). Cache locality here is a *hint*, not a correctness
requirement — the property Q3 asks you to contrast with partitioned Raft.

### Step 8 — the epilogue: S3 grew the missing primitives

The 2008 walls were eventually fixed by S3 itself: strong read-after-write
consistency arrived in **Dec 2020** (retroactively fixing half the 2008
paper), and conditional writes (`If-Match` compare-and-swap PUTs) landed
**~Nov 2024** — the atomic-commit primitive whose absence forced
metadata services, and which SlateDB's manifest fencing now leans on
directly (see the slatedb guide). But note the punchline of Q1: every
serious system *routed around* all three walls with immutability + a
small strongly-consistent metadata tier years before S3 fixed them.

## How to read the papers (with the concepts in hand)

Read them in historical order:

1. **Brantner et al. 2008, §1–3** — the direct translation (Step 2):
   B-tree pages as objects, commit via SQS queues. Read §3's protocols
   as WAL-shipping built from queues.
2. **2008 §5** — the honest accounting: no read-your-writes, no
   multi-object atomicity, request costs. Annotate each with its Step 8
   fix date and its Step 3–4 workaround.
3. **Snowflake 2016, §1–4** — the three layers (Step 5),
   micro-partitions and file-list versioning (Steps 3–4), pruning
   (Step 6), warehouse caches and work stealing (Step 7). Watch how each
   2008 pathology is dissolved rather than solved.

## Questions to answer in notes.md

**Q1.** List the three 2008 blockers (consistency, atomic multi-page
commit, cost-per-request) and, for each, what changed: S3 strong
consistency (2020), S3 conditional PUT/CAS (2024, enabling manifests as
commit points — see slatedb guide), and bigger immutable objects
(amortize request cost). Which blocker did systems *route around* rather
than wait for? (All three — via immutability + a small strongly-consistent
metadata tier.)

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

## References

**Papers**
- Dageville et al. — "The Snowflake Elastic Data Warehouse"
  (SIGMOD 2016) — read §1-4
- Brantner, Florescu, Graf, Kossmann, Kraska — "Building a Database on
  S3" (SIGMOD 2008) — read §1-3 + §5; it's the prescient one
