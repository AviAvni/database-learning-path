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
out if you split them into separate services, and answers with the thesis
its abstract states outright: *separating the log and storage tiers
separates durability (implemented by the log) from availability
(implemented by the storage tier).*

## The concepts, step by step

### Step 1 — durability and availability are different jobs

> **In:** a committing transaction and the pages it will later be read
> back from. **Out:** the two properties a classic engine bundles into one
> storage layer — durability and availability — and the fact that their
> hardware wants are opposite, which is the wedge every later step drives
> in.

**Durability** means an acknowledged write survives crashes;
**availability** means the data can actually be read back quickly right
now. A classic engine bundles them: the **write-ahead log** (WAL — the
append-only file every change hits before the data pages do, topic 5)
provides durability, and the buffer pool + data files provide
availability. The insight is that their hardware wants differ completely:

| job | requirement | Socrates tier |
|---|---|---|
| durability | tiny, fast, sequential, SSD/NVM | **XLOG service** (the landing zone) |
| availability | big, warm, random-read, scalable | **Page servers** + XStore |

The log tail is small and must be made durable at local-storage speed;
the page set is terabytes that must be read at random. Provisioning one
tier for both means paying NVMe prices for terabytes *or* paying blob
latency on every commit. Concretely against this topic's own bench: a
durable append wants local-NVMe-class latency (our measured local p50 is
**0.10 ms**), while the cold page floor is object-store-class (our raw-S3
p50 is **14.17 ms** — a 140× gap). No single medium is priced right for
both. Split them and each tier is sized, priced, and replicated for
exactly one job.

### Step 2 — the four tiers

> **In:** the durability-vs-availability split from Step 1. **Out:** the
> four named services SQL Server is decomposed into, and the one-way data
> flow that connects them.

Socrates decomposes SQL Server into a pipeline of four tiers (§4.2,
"Socrates Architecture Overview" — the paper lists them as the realization
of six design goals: separation of compute and storage, tiered scaled-out
storage, bounded operations, separation of *log* from compute and storage,
pushdown of functions into storage, and reuse of existing components):

```
        compute primary ──► XLOG service (log landing zone, quorum, FAST)
           │                    │ fan-out (async)
           ▼ getPage(id,LSN)    ▼
        page servers (each owns a ~128 GB partition; RBPEX cache; replay log)
           │ backing store
           ▼
        XStore (Azure blob storage — cheap, slow, all versions)
```

**Compute** runs the query engine and the buffer pool — one **Primary**
handles all read/write transactions, any number of **Secondaries** serve
read-only and stand by for failover (§4.2). **XLOG** is a small, fast
service whose only job is landing log records durably. **Page servers**
each own a partition of the database and answer `getPage(pageId, LSN)`
requests. **XStore** (Azure's blob/object storage) holds everything,
cheaply and forever. Data flows one way — log lands fast, fans out
asynchronously, settles into blobs — and each hop trades latency for
capacity and cost.

### Step 3 — the XLOG landing zone: commit latency is one small append

> **In:** log records emitted by the Primary in Step 2. **Out:** why a
> commit's latency collapses to a single small durable append, and the
> lifecycle that keeps that fast tier from filling up.

Commit latency = XLOG append only. The **landing zone (LZ)** is, in the
paper's words (§4.3, "XLOG Service"), a storage area the Primary *"writes
log blocks synchronously and directly to… for lowest possible commit
latency"*; it is *"meant to be fast (possibly expensive) but small"* and
*"organized as a circular buffer."* That is the same move as Aurora's 4/6
log quorum and Neon's safekeepers: put *only the log tail* on premium
storage. Because the LZ is small and circular, the log must move on: an
XLOG process called **destaging** copies each block to a fixed-size local
SSD cache (for fast re-reads) and to XStore for long-term retention (the
paper calls that archive *LT*, and keeps log records ~30 days for
point-in-time recovery). That three-stop path — active tail in the LZ →
SSD cache → XStore archive — is exactly topic 5's WAL lifecycle (active
tail → archived → checkpointed away), rebuilt as separate services. The
payoff: commits never wait for blob storage (our raw-S3 p50 is 14.17 ms),
only for a local-SSD-class quorum append.

### Step 4 — page servers are caches, and caches are disposable

> **In:** the durable log tail from Step 3 and the XStore floor from
> Step 2. **Out:** why a page server holds nothing you can't rebuild, and
> what losing one actually costs.

A page server consumes the log stream *asynchronously* and applies it to
its partition's pages, serving `getPage(pageId, LSN)` to compute — but it
holds nothing durable: its state is always reconstructible as "XStore
snapshot + replay the log since." So a page server can lag, crash, or be
rebuilt from XStore without any data loss — losing one costs *warm-up
time*, not data. Partitioning makes this scale: the paper reasons that
*"a good partition size for a Page Server is 128 GB,"* so a database of
hundreds of TB spreads across thousands of page servers (256 TB ÷ 128 GB
= **2,000** page servers). That separation is the thesis in action:
durability lives in XLOG + XStore; page servers provide only availability,
so they can be scaled out (one per partition, plus replicas) and treated
as cattle. The cost: a cold page server serves misses at XStore latency
(compare our tier_bench raw-S3 lane: **p99 ~113 ms**) until its cache
re-warms.

### Step 5 — RBPEX: the buffer pool that survives a restart

> **In:** a page server (or compute node) whose cache would otherwise be
> cold after a restart. **Out:** the one property Socrates adds to the
> classic buffer pool — persistence — and why it is worth the complexity
> given the miss cost measured in Step 4.

**RBPEX** (Resilient Buffer Pool Extension, §3.3) is topic 6's buffer pool
spilled to local SSD *and made restart-survivable*: the cache's contents
persist across process restarts, so a rebooted node comes back warm
instead of paying thousands of cold misses against XStore. Both compute
nodes and page servers run one. This is the cache tier of topic 28's
ladder (RAM → local SSD → object store) with one extra property —
persistence — bought because the miss cost below it is roughly 140× a
local read (our 14.17 ms S3 p50 against 0.10 ms local p50). Snapshots and
backup fall out of the same tiering: XStore blob snapshots are nearly
free, like Neon branches but coarser-grained.

### Step 6 — reuse over rewrite: the engineering thesis

> **In:** the four tiers (Steps 2–5), now standing where Aurora put a
> single rewritten storage engine. **Out:** what Socrates buys by
> *rearranging* stock SQL Server instead of rewriting it, and the price it
> pays in bytes on the wire.

Where Aurora rewrote its storage engine around "log only", Socrates' sixth
design goal (§4.1.6, "Reuse Components") is to *reuse* SQL Server's
existing machinery — the page-oriented redo of its ARIES-derived recovery
(topic 5; the undo half is modernized by Accelerated Database Recovery,
§3.2), the HADR log-transport code (its existing replication stack), the
buffer pool — and rearrange it into tiers. Both designs end at the same
sentence ("compute ships log; a page service replays it"), but Socrates
gets there with far less new engine code, at the price of extra write
amplification between tiers: the log lands in XLOG, is shipped to page
servers, applied to pages, and those pages are written again to XStore —
the same bytes traverse more hops than in Aurora's fused design. (That
per-hop amplification is our reading of the architecture, not a figure the
paper quotes.) That reuse-vs-rewrite trade is the durable lesson for
anyone retrofitting an existing engine (M28: FalkorDB keeps its AOF and
matrices; the tiers are the new part).

## How to read the paper (with the concepts in hand)

- **§1 + §4.1** — the argument: the abstract states the thesis (durability
  ≠ availability), and §4.1 lays out the six design goals, including
  §4.1.4 "Low Log Latency" and §4.1.6 "Reuse Components". Read these
  carefully; they carry the whole design. (§2 is a survey of prior DBaaS
  including HADR — background, not the thesis.)
- **§4.2–4.7 + §3.3** — the four tiers of Step 2, one by one: §4.2
  overview and compute; §4.3 XLOG and the landing-zone lifecycle (Step 3);
  §4.4 the Primary and `getPage(pageId, LSN)`; §4.6 page servers as
  rebuildable 128 GB-partition caches (Step 4); §4.7 XStore as the durable
  floor; and §3.3 RBPEX (Step 5), the restart-survivable cache used in
  both compute and storage tiers.
- **§7 (performance)** — skim; the architecture, not the numbers, is what
  transfers.

The comparison table to carry forward:

| | Aurora | Socrates | Neon |
|---|---|---|---|
| durability quorum | storage nodes (4/6) | XLOG landing zone | safekeepers (Paxos-ish) |
| page serving | same nodes | separate page servers | pageserver |
| cold tier | (internal) | XStore blobs | S3 layer files |
| engine rewrite? | storage layer yes | minimal (reuse) | none (stock Postgres + smgr hook) |
| caches | storage-side pages | RBPEX (compute AND page server) | pageserver layers + compute shared buffers |

## Questions to answer in notes.md

**Q1.** Socrates keeps SQL Server's page-oriented redo (topic 5), Aurora
rearchitected around "log only". Yet both end with "compute ships log;
page service replays". What did Socrates get for *not* rewriting the
engine (hint: §4.1.6's stated goal — reuse SQL Server code: HADR log
transport, buffer pool, etc.), and what does it pay in write amplification
between tiers?

**Q2.** The XLOG "landing zone" is small and organized as a circular
buffer; the log is destaged to an SSD cache and to XStore once consumed.
Map each stage onto topic 5's WAL lifecycle (active tail → archived →
checkpointed away) and onto Neon: which Neon component is the landing
zone, which is the long-term log? (safekeepers; S3 via the pageserver's
layer uploads.)

**Q3.** A page server is "just a cache of XStore + log replay" — so losing
one costs nothing durable. What does this do to the *tail latency* story
when a page server is cold (compare our tier_bench raw-S3 lane: p99
~113 ms)? Where does Socrates hide the misses? (RBPEX warm-up from
snapshot; requests served by replicas.)

**Q4 (M28).** FalkorDB single-writer translation: the XLOG/page-server
split says "durability tier ≠ serving tier". For a graph engine, the
durability tier is the AOF/replication log (topic 5); the serving tier is
materialized matrices. Does M28 need a page-server equivalent at all, or
does the compute node's own RBPEX-style local cache over object storage
suffice until read replicas (M15) enter? Write the one-paragraph answer in
notes.md.

## Done when

Answer each before unfolding it.

- [ ] You can state why durability and availability are different jobs,
  and what that separation permits.
  <details><summary>Answer</summary>

  Durability = an acked write survives crashes (wants a tiny, fast,
  sequential append target); availability = data can be read back quickly
  now (wants a big, warm, random-access, scalable cache). Splitting them
  lets each tier be sized, priced, and replicated for one job instead of
  overpaying for both — the thesis of the abstract: the log tier gives
  durability, the storage tier gives availability.

  </details>

- [ ] You can name the four tiers and what each owns.
  <details><summary>Answer</summary>

  Compute (query engine + buffer pool; one Primary, many Secondaries);
  XLOG service (the small fast landing zone that makes log durable); Page
  servers (each owns a ~128 GB partition, replays log, answers
  `getPage(pageId, LSN)`); XStore (Azure blob storage — the cheap, durable,
  all-versions floor).

  </details>

- [ ] You can explain why commit latency reduces to one small append to
  the XLOG landing zone.
  <details><summary>Answer</summary>

  The Primary writes log blocks synchronously and directly to the landing
  zone — a small, fast, circular buffer — and a commit is durable once that
  append is acknowledged. Everything else (fan-out to page servers,
  destaging to the SSD cache and XStore) happens asynchronously, off the
  commit path, so commits never wait on blob storage.

  </details>

- [ ] You can explain why page servers are caches and therefore
  disposable.
  <details><summary>Answer</summary>

  A page server's entire state is reconstructible as "XStore snapshot +
  replay the log since," so it holds nothing durable. It can lag, crash,
  or be rebuilt with no data loss — the only cost is cache warm-up time.
  That is what lets Socrates scale them out one-per-partition and treat
  them as cattle.

  </details>

- [ ] You can explain what RBPEX preserves across a restart and why that
  matters given this topic's measured cold-S3 tail.
  <details><summary>Answer</summary>

  RBPEX (Resilient Buffer Pool Extension) is a local-SSD buffer-pool
  extension whose contents *persist across restarts*, so a rebooted node
  comes back warm. It matters because a miss falls through to XStore, and
  our bench measures that floor at S3 p50 14.17 ms / p99 112.99 ms versus
  0.10 ms local — a cold cache would pay that tail thousands of times.

  </details>

## References

**Papers**
- Antonopoulos et al. — "Socrates: The New SQL Server in the Cloud"
  (SIGMOD 2019). Read §1 and §4.1 for the argument and design goals,
  §4.2–4.7 for the four tiers (XLOG §4.3, `getPage` §4.4, page servers and
  the 128 GB partition §4.6, XStore §4.7), §3.3 for RBPEX; skim §7
  (performance). Sections are cited inline above.
