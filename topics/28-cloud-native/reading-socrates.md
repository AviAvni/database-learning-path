# Socrates: durability is not availability

SQL Server rebuilt for Azure, with one architectural thesis: the tier
that makes a write durable and the tier that serves pages back have
opposite requirements, so they should be different services. This
chapter builds that split step by step — the two jobs Aurora fused, the
four tiers Socrates pulls them apart into, and how the classic engine
gets reused rather than rewritten — the counterpoint to Aurora's
storage-layer rewrite. Read it right after the Aurora chapter.

## The problem in one sentence

Aurora's storage fleet does two jobs with opposite requirements — making
a commit durable (needs a *tiny, fast, sequential* append target) and
serving page reads (needs a *big, warm, random-access, scalable* cache) —
and one fleet sized for both overpays for each; Socrates asks what falls
out if you split them into separate services.

## The concepts, step by step

### Step 1 — durability and availability are different jobs

**Durability** means an acknowledged write survives crashes;
**availability** means the data can actually be read back quickly right
now. A classic engine bundles them: the write-ahead log (WAL — the
append-only file every change hits before the data pages do, topic 5)
provides durability, and the buffer pool + data files provide
availability. The insight is that their hardware wants differ completely:

| job | requirement | Socrates tier |
|---|---|---|
| durability | tiny, fast, sequential, SSD/NVM | **XLOG service** (the landing zone) |
| availability | big, warm, random-read, scalable | **Page servers** + XStore |

The log tail is a few GB that must be written in ~1 ms; the page set is
terabytes that must be read at random. Provisioning one tier for both
means paying NVMe prices for terabytes or millisecond-commit penalties on
cheap storage. Split them and each tier is sized, priced, and replicated
for exactly one job.

### Step 2 — the four tiers

Socrates decomposes SQL Server into a pipeline of four services:

```
        compute primary ──► XLOG service (log landing zone, quorum, FAST)
           │                    │ fan-out (async)
           ▼ GetPage            ▼
        page servers (each owns a partition; RBPEX cache; replay log)
           │ backing store
           ▼
        XStore (Azure blob storage — cheap, slow, all versions)
```

Compute runs the query engine and the buffer pool; **XLOG** is a small,
fast quorum service whose only job is landing log records durably; **page
servers** each own a partition of the database and answer `GetPage`
requests; **XStore** (Azure's blob/object storage) holds everything,
cheaply and forever. Data flows one way — log lands fast, fans out
asynchronously, settles into blobs — and each hop trades latency for
capacity and cost.

### Step 3 — the XLOG landing zone: commit latency is one small append

Commit latency = XLOG append only. The **landing zone** is a small,
fixed-size, fast-storage buffer where log records become durable the
moment a quorum acknowledges them — the same move as Aurora's 4/6 log
quorum and Neon's safekeepers: put *only the log tail* on premium
storage. Because it's fixed-size, the log must be **truncated** (old
records discarded from the landing zone) once page servers and XStore
have consumed them — which is exactly topic 5's WAL lifecycle (active
tail → archived → checkpointed away), rebuilt as three separate services.
The payoff: commits never wait for blob storage's ~15 ms+, only for a
local-SSD-class quorum append.

### Step 4 — page servers are caches, and caches are disposable

A page server consumes the log stream *asynchronously* and applies it to
its partition's pages, serving `GetPage(page_id)` to compute — but it
holds nothing durable: its state is always reconstructible as "XStore
snapshot + replay the log since". So a page server can lag, crash, or be
rebuilt from XStore without any data loss — losing one costs *warm-up
time*, not data. That separation is the thesis in action: durability
lives in XLOG + XStore; page servers provide only availability, so they
can be scaled out (one per partition) and treated as cattle. The cost:
a cold page server serves misses at XStore latency (compare our
tier_bench raw-S3 lane: p99 ~113 ms) until its cache re-warms.

### Step 5 — RBPEX: the buffer pool that survives a restart

**RBPEX** (Resilient Buffer Pool Extension) is topic 6's buffer pool
spilled to local SSD *and made restart-survivable*: the cache's contents
persist across process restarts, so a rebooted node comes back warm
instead of paying thousands of cold misses against XStore. Both compute
nodes and page servers run one. This is the cache tier of topic 28's
ladder (RAM → local SSD → object store) with one extra property —
persistence — bought because the miss cost below it is 100× a local read.
Snapshots and backup fall out of the same tiering: XStore blob snapshots
are nearly free, like Neon branches but coarser-grained.

### Step 6 — reuse over rewrite: the engineering thesis

Where Aurora rewrote its storage engine around "log only", Socrates'
stated goal is to *reuse* SQL Server's existing machinery — the ARIES
page-oriented redo (topic 5), the HADR log-transport code (its existing
replication stack), the buffer pool — and rearrange it into tiers. Both
designs end at the same sentence ("compute ships log; a page service
replays it"), but Socrates gets there with far less new engine code, at
the price of extra write amplification between tiers: the log lands in
XLOG, is shipped to page servers, applied to pages, and those pages are
written again to XStore — the same bytes traverse more hops than in
Aurora's fused design. That reuse-vs-rewrite trade is the durable lesson
for anyone retrofitting an existing engine (M28: FalkorDB keeps its AOF
and matrices; the tiers are the new part).

## How to read the paper (with the concepts in hand)

- **§1–2** — the argument: Step 1's table in prose, plus the goals
  (reuse SQL Server code, separate durability from availability). Read
  these carefully; they carry the thesis.
- **§3–5** — the four tiers of Step 2, one by one: compute + RBPEX
  (Step 5), XLOG and the landing-zone lifecycle (Step 3), page servers
  as rebuildable caches (Step 4), XStore as the durable floor.
- **Performance sections** — skim; the architecture, not the numbers, is
  what transfers.

The comparison table to carry forward:

| | Aurora | Socrates | Neon |
|---|---|---|---|
| durability quorum | storage nodes (4/6) | XLOG landing zone | safekeepers (Paxos-ish) |
| page serving | same nodes | separate page servers | pageserver |
| cold tier | (internal) | XStore blobs | S3 layer files |
| engine rewrite? | storage layer yes | minimal (reuse) | none (stock Postgres + smgr hook) |
| caches | storage-side pages | RBPEX (compute AND page server) | pageserver layers + compute shared buffers |

## Questions to answer in notes.md

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

## References

**Papers**
- Antonopoulos et al. — "Socrates: The New SQL Server in the Cloud"
  (SIGMOD 2019) — read §1-2 for the argument, §3-5 for the four
  tiers, skim performance
