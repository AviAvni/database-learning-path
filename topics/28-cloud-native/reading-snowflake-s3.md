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

Object storage is roughly an order of magnitude cheaper per stored byte
than provisioned replicated block disks and effectively infinitely
elastic, but in 2008 it offered no read-your-writes consistency, no
multi-object atomicity, tens-of-milliseconds GETs, and a per-request
bill — so the question is whether a database can live there at all, and
the two papers answer "not like this" (Brantner et al., 2008) and "yes,
like *this*" (Dageville et al., Snowflake, 2016).

```
   2008: pages on S3, updated in place ──► eventual consistency pain,
         no atomicity, pay-per-request shock       (all catalogued honestly)
   2016: IMMUTABLE columnar files on S3 + metadata service for the
         mutable bit ──► every 2008 problem dissolves except latency,
         which caching + columnar scans amortize
```

## The concepts, step by step

### Step 1 — what object storage actually is

> **In:** the "can a database live on S3?" question from the problem
> statement. **Out:** the exact primitive set S3 offers (and the ones it
> withholds), plus the price and latency shape every later step is paying
> down. Ground floor — no earlier step feeds it.

An **object store** (S3) is a key → blob service: `PUT` a whole object,
`GET` it (or a byte range) back, `LIST` keys — and nothing else. No
in-place update (a "modify" is a full re-`PUT` under the same key), no
append, no rename, and in 2008 no atomic operations across objects and
only **eventual consistency** (a GET after a PUT could return the old
version). What you get in exchange, per AWS's published pricing and
durability SLA: order-of-a-few-cents per GB-month, eleven-nines
durability, cross-AZ replication by default, unbounded capacity — and a
charge *per request* (fractions of a cent per thousand GETs), which
quietly punishes any design built from many small objects.

Latency is the other tax, and here we can use *this repo's own measured
numbers* rather than a vendor figure: the tier_bench raw-S3 lane records
**p50 14.17 ms, p99 112.99 ms** per GET, against **0.10 ms** for local
NVMe (FINDINGS row 28 / notes.md) — a **140× median gap** and a far worse
tail. Every structure below exists to keep off that hot path.

### Step 2 — the 2008 attempt: pages on S3, updated in place

> **In:** the S3 primitive set from Step 1. **Out:** the most direct
> "database on S3" design — B-tree pages as mutable objects — and the three
> structural walls it hits, all traceable to one behavior.

The 2008 paper (Brantner, Florescu, Graf, Kossmann, Kraska — "Building a
Database on S3") does the direct translation: store B-tree pages (topic
1's fixed-size disk blocks) as individual S3 objects and update them *in
place*, with commit implemented by pushing log records to SQS queues and
"checkpointing" merging them back into the page objects — WAL-shipping
(topic 5) built from queues. Its protocol discussion is worth reading, and
its cost accounting is the honest part. (This paper is closed-access; the
section numbers in older versions of this guide could not be re-verified
against a copy, so this guide cites it by idea, not by section.) Three
walls, all structural:

- **No read-your-writes:** eventual consistency means a transaction can
  fail to see its own committed page — snapshot reads are unbuildable.
- **No multi-object atomicity:** a commit touching 10 page-objects has no
  way to make them appear together; a crash mid-checkpoint leaves a
  half-updated tree.
- **Request costs dominate at small pages:** at 4–16 KB objects, the
  per-request fee rivals or exceeds the storage fee — the design bleeds
  money on bookkeeping IOs.

The 2008 design fails because every wall is hit by the same behavior:
*mutating small shared objects.*

### Step 3 — the fix: never modify an object

> **In:** the "mutating small shared objects" diagnosis from Step 2.
> **Out:** the single design move — make every data object immutable — and
> how it softens all three walls at once.

Make every data object **immutable** — written once, read many, replaced
never, only superseded — and all three walls soften together. Consistency:
an immutable object can't be stale (any copy anywhere is *the* version;
eventual consistency of overwrites stops mattering because there are no
overwrites). Atomicity: since data objects never change, the only mutable
thing left is the *list of which objects constitute the table* — one small
piece of state. Cost: immutable files can be big (megabytes, not
kilobytes), amortizing the per-request fee by three orders of magnitude.

Snowflake's data unit is exactly this: the 2016 paper stores tables as
*"large, immutable files"* laid out in **PAX / hybrid columnar** format
(topic 11) — each column's values grouped and heavily compressed, with a
header of per-column offsets so an S3 range GET fetches only the columns a
query needs. (Snowflake's *product documentation* later brands these files
"micro-partitions"; the 2016 SIGMOD paper uses neither that term nor any
fixed file size, so this guide says "immutable file" and does not quote a
byte size the paper never states.)

### Step 4 — hoist the mutable bit: a table version is a list of files

> **In:** the immutable data files from Step 3 and the "only the file list
> is mutable" observation. **Out:** how an UPDATE becomes file replacement
> plus one metadata write, and the time-travel and clone features that
> fall out for free.

With data frozen, updates become file replacement: an UPDATE rewrites the
affected files as *new* files and publishes a new **table version** —
which is nothing but a list of file names. That list lives in a small,
strongly-consistent **metadata service**, and swapping it is the atomic
commit point. Consequences fall out for free:

```
   table v41 = [f1, f2, f3]          time travel: keep old versions'
   UPDATE rewrites f2 -> f2'                       file lists around
   table v42 = [f1, f2', f3]         clone: copy the LIST (bytes: ~KB),
   commit = publish v42 (one         not the files (bytes: ~TB) —
   metadata write, atomic)           CoW branching at file granularity
```

Time travel = read an old list (the 2016 paper retains removed files for a
configurable window, *up to 90 days*, §4.4). Zero-copy **clone** (the
`CLONE` keyword, §4.4) = copy the list; both tables then reference the
same files and diverge copy-on-write thereafter. This is the same
copy-on-write move as Neon's branches and SlateDB's clones, at file
granularity — and the whole 2008 atomicity wall reduced to one
strongly-consistent metadata write.

### Step 5 — Snowflake's three layers

> **In:** the immutable-files-plus-metadata design from Steps 3–4.
> **Out:** the three architectural layers Snowflake draws that around, and
> why compute elasticity comes for free.

Snowflake's architecture (§3, Figure 1) is Steps 3–4 drawn as boxes, three
independently scalable layers: **Data Storage** holds the immutable files
in S3 (all the bytes, none of the state machine); **Virtual Warehouses**
are stateless clusters of EC2 workers, sized and spun per customer, that
read any table (shared-data: any compute can reach any data, unlike
shared-nothing where data is bound to nodes); and **Cloud Services** — the
only stateful tier — holds metadata, transactions (snapshot isolation over
file lists, per Step 4), and query optimization. Compute elasticity is
free *because* compute owns nothing durable: the paper notes there is *no
buffer pool* and workers keep only caches and temp data, so resizing a
warehouse moves no base data.

### Step 6 — pruning, not indexes

> **In:** the per-file metadata the Cloud Services tier already keeps
> (Step 5). **Out:** how Snowflake replaces B-tree indexes with min/max
> pruning, worked on a concrete clustered table.

Snowflake maintains no B-tree indexes. Instead the metadata service keeps,
per file, the **min and max value of each column** — what the literature
calls **min-max pruning**, *zone maps*, small materialized aggregates, or
data skipping (§3.3.3) — and a query skips every file whose [min, max]
range can't contain a match. Worked example: an event table loaded daily
is naturally clustered by load date, so a year is ~365 files by date. A
`WHERE date = '2016-03-01'` predicate keeps 1 file and prunes the other
364 — 364/365 ≈ **99.7%** of files eliminated before a single GET — and
that is a calculation about clustering, not a figure quoted from the
paper. It is topic 26's BRIN-shaped one-sided filter at cloud scale: it
can prove absence, never presence — good enough when scans are columnar
and the per-file cost is tens of milliseconds.

### Step 7 — clawing back the latency: caches and consistent hashing

> **In:** the tens-of-ms S3 GET latency from Step 1 and the file-set a
> query must scan after pruning (Step 6). **Out:** the caching and
> assignment machinery that keeps warm scans off S3, and why cache
> locality here is only a hint.

Files still live ~14 ms away (our measured S3 p50), so each warehouse
keeps a **local-disk cache** of the files it reads (§3.2). To keep caches
from all holding the same hot files, file→node assignment uses
**consistent hashing** over table-file names (each file hashes to a
preferred worker, so a resize remaps only a fraction of files — and no
base data moves, only cache assignments, since S3 remains the source of
truth). The paper's consistent hashing is *lazy*: on a resize it shuffles
nothing eagerly and lets LRU re-warm caches over subsequent queries. Skew
is handled by **work stealing** (idle nodes take file-scan tasks from busy
ones, reading from S3 directly). Cache locality here is a *hint*, not a
correctness requirement — the property Q3 asks you to contrast with
partitioned Raft.

### Step 8 — the epilogue: S3 grew the missing primitives

> **In:** the two 2008 consistency/atomicity walls from Step 2. **Out:**
> the dates S3 itself closed them — external AWS history, not paper
> content — and the punchline that systems had already routed around them.

The 2008 walls were eventually narrowed by S3 itself. Per AWS's own
announcements (not either paper): strong read-after-write consistency for
all S3 GETs arrived in **December 2020**, and conditional writes
(`If-Match` compare-and-swap PUTs) — the atomic-commit primitive whose
absence forced metadata services — landed in **late 2024**, which is
exactly what SlateDB's manifest fencing now leans on directly (see the
slatedb guide). But note the punchline Q1 draws out: every serious system
*routed around* all three walls with immutability + a small
strongly-consistent metadata tier years before S3 fixed any of them.

## How to read the papers (with the concepts in hand)

Read them in historical order:

1. **Brantner et al. 2008** — the direct translation (Step 2): B-tree
   pages as objects, commit via SQS queues, checkpointing back into page
   objects. Read the protocol discussion as WAL-shipping built from
   queues, and the cost accounting as the honest ledger. (Closed-access;
   cite by idea — this guide deliberately gives no section numbers it
   could not re-verify.)
2. **The 2008 cost/consistency discussion** — annotate each blocker (no
   read-your-writes, no multi-object atomicity, request cost) with its
   Step 8 fix date and its Step 3–4 workaround.
3. **Snowflake 2016, §2–4** — §2 the storage-vs-compute split, §3 the
   three layers (Step 5) and immutable files + pruning (Steps 3, 6), §4
   time travel and cloning (Step 4). Watch how each 2008 pathology is
   dissolved rather than solved.

## Questions to answer in notes.md

**Q1.** List the three 2008 blockers (consistency, atomic multi-page
commit, cost-per-request) and, for each, what changed: S3 strong
consistency (Dec 2020), S3 conditional PUT/CAS (late 2024, enabling
manifests as commit points — see slatedb guide), and bigger immutable
objects (amortize request cost). Which blocker did systems *route around*
rather than wait for? (All three — via immutability + a small
strongly-consistent metadata tier.)

**Q2.** Snowflake's shared-data claim: any warehouse can read any table,
scaling compute without data movement. What's the concurrency price —
where do write-write conflicts get decided, and why is "metadata service
does snapshot isolation over file lists" enough for a warehouse (vs an
OLTP engine, where Aurora needed per-page LSN machinery)?

**Q3.** Consistent-hash-with-cache vs shared-nothing partitioning
(topic 15): when a Snowflake warehouse resizes, no base data reshuffles —
only cache assignments change, lazily. What workload property makes "cache
locality is a hint, not a correctness requirement" true here but false
for, say, a partitioned Raft group?

**Q4 (M28).** FalkorDB analytics reads (topic 22's read replicas / BI
export shape): the immutable-file idea says "publish immutable columnar
snapshots of the graph + a version manifest" instead of replicating the
live engine. Which graph representations tolerate immutable
megabyte-scale columnar chunks well (edge lists / CSR segments, topic 2)
and which don't (in-place delta-mutated matrices)? One paragraph in
notes.md.

## Done when

Answer each before unfolding it.

- [ ] You can say what object storage actually is, in terms of which
  primitives exist.
  <details><summary>Answer</summary>

  A key → blob store with `PUT` (whole object), `GET` (whole or byte
  range), and `LIST` — no in-place update, no append, no rename, and in
  2008 no cross-object atomicity and only eventual consistency. You buy
  cheap, eleven-nines-durable, elastic capacity at the cost of a
  per-request fee and tens-of-ms latency (our bench: S3 p50 14.17 ms).

  </details>

- [ ] You can explain why the 2008 pages-on-S3 attempt failed.
  <details><summary>Answer</summary>

  It mutated small shared page-objects in place. That hit three walls at
  once: eventual consistency broke read-your-writes, there was no way to
  commit many page-objects atomically, and per-request fees dominated at
  4–16 KB objects. One behavior, three failures.

  </details>

- [ ] You can state the fix — never modify an object — and where the
  mutable bit goes instead.
  <details><summary>Answer</summary>

  Make every data file immutable (write-once, superseded never); the only
  mutable state left is the list of which files make up a table, which
  lives in a small strongly-consistent metadata service. Immutable files
  can be large (megabytes), amortizing per-request cost; Snowflake stores
  them as PAX/hybrid-columnar files (its docs later call them
  micro-partitions).

  </details>

- [ ] You can explain a table version as a list of files, and pruning as
  the replacement for indexes.
  <details><summary>Answer</summary>

  A table version is just a list of immutable file names; an UPDATE writes
  new files and atomically publishes a new list, which also gives time
  travel (old lists) and zero-copy clones (copy a list). Pruning replaces
  B-trees: per-file min/max zone maps let a query skip any file whose range
  can't hold a match — e.g. a day predicate over a year of daily files
  skips ~364/365 ≈ 99.7%.

  </details>

- [ ] You can explain how caches and consistent hashing claw back the gap,
  and check it against this topic's measured 140× median ratio.
  <details><summary>Answer</summary>

  Warehouses cache files on local disk; consistent hashing sends each file
  to a preferred worker (lazily, LRU-refilled on resize) so caches don't
  duplicate hot files, and work stealing handles skew. That hides the
  measured 140× gap (S3 p50 14.17 ms vs local 0.10 ms) on warm scans;
  locality is a performance hint, not a correctness requirement, because
  S3 stays the source of truth.

  </details>

## References

**Papers**
- Dageville et al. — "The Snowflake Elastic Data Warehouse" (SIGMOD 2016).
  Read §2 (storage vs compute), §3 (three layers, immutable files,
  pruning), §4 (time travel and cloning). Every Snowflake claim here is
  cited to a section inline; the paper states no fixed file size and does
  not use the term "micro-partition".
- Brantner, Florescu, Graf, Kossmann, Kraska — "Building a Database on S3"
  (SIGMOD 2008) — the prescient one. Closed-access and not re-verifiable
  here, so it is cited by idea rather than by section number.
- S3 consistency (Dec 2020) and conditional-write (late 2024) dates are
  from AWS's own announcements, not from either paper.
