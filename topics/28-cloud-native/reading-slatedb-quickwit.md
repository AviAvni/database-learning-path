# SlateDB & Quickwit: born on S3

Neon and Aurora retrofit object storage under an existing engine;
SlateDB (an LSM whose ONLY disk is an object store) and Quickwit
(search over S3) were *born* there — so every S3 pathology has an
explicit, readable countermeasure in their code. This chapter builds
each countermeasure step by step — the re-priced LSM, the manifest as
the single point of truth, CAS fencing, the cache ladder, zero-copy
clones, bundles, and hedged reads — then hands you the anchors. It is
the menu M28's tiered-storage stubs are ordered from. Every anchor was
re-verified against the pinned clones (slatedb `323ed1b`, quickwit
`a5ad540`).

## The problem in one sentence

An engine whose only disk is S3 inherits four taxes at once — tens-of-ms
GETs (our measured raw-S3 p50 14.17 ms, p99 112.99 ms), a fee per
request, no atomic multi-object operations, and no locks to keep two
writers apart — and every structure in these two codebases exists to pay
one of them down.

## The concepts, step by step

### Step 1 — the LSM, re-priced: when a durable write costs ~100 ms

> **In:** an LSM engine (topic 4) and the S3 latency envelope from the
> problem statement. **Out:** the one operation whose cost inverts when
> every disk write becomes an S3 PUT — durability — and the two knobs
> SlateDB adds to cope. Ground floor for the manifest steps that follow.

An **LSM** (topic 4) buffers writes in a sorted in-memory table (the
**memtable**), logs them to a **WAL** (write-ahead log) for durability,
and periodically flushes immutable sorted files (**SSTs**) that a
background **compactor** merges. SlateDB keeps that machine intact and
swaps every disk write for an S3 PUT:

```
 put ──► WAL buffer ──► WAL SSTs on S3 ──► memtable flush ──► L0 SSTs ──► runs
          (batch!)      durable-write tax          compactor (separate process,
   AwaitDurable vs no-sync = the fsync           fenced by compactor_epoch)
   trade (topic 5), now costing ~100 ms                       │
                         manifest on S3, updated via CAS ◄─────┘
```

The repricing bites exactly once: durability. A local fsync is our
measured **0.10 ms** (local NVMe p50); a durable S3 write sits in the
**14–113 ms** band (our S3 p50 14.17 ms, p99 112.99 ms) — that is **~140×
worse at the median and ~1,130× at the tail** (14.17 / 0.10 ≈ 142;
112.99 / 0.10 ≈ 1,130). So SlateDB batches many puts into one WAL object
and offers `AwaitDurable` (wait for the PUT) vs no-sync (return once the
put is in the memtable) — topic 5's fsync trade with the price multiplied
a hundredfold. That floor is *why* Neon/Socrates-class systems keep a fast
landing zone instead (Q1). Reads are untouched: memtable → L0 → sorted
runs, same as topic 4.

### Step 2 — the manifest: the entire database is one small object

> **In:** the immutable SSTs written in Step 1 and S3's lack of
> multi-object atomicity. **Out:** the single small object that holds all
> mutable truth — the manifest — and why it is the one place the database
> state changes.

Since S3 objects are immutable-in-practice (PUTs replace, never modify)
and there's no atomic multi-object commit, SlateDB makes every data object
(WAL SSTs, L0 SSTs, compacted runs) **immutable** and gathers the mutable
truth into one small object: the **manifest** — the list of live SSTs plus
epochs and checkpoint metadata. A state change = write new immutable
objects (add-only, harmless), then publish one new manifest version. The
manifest is the **linearization point** (the single place where "what the
database is" changes atomically) — the same move as Snowflake's
table-version file lists, at engine granularity. This is Q3's answer
taking shape: writer and compactor can race freely on *data* objects
because only the manifest CAS decides.

### Step 3 — fencing: single-writer safety from one conditional PUT

> **In:** the manifest-as-linearization-point from Step 2. **Out:** how a
> single conditional PUT on that manifest gives single-writer safety with
> no lock service, and how a stale writer is forced to die.

With no lock service, what stops two processes both believing they're the
writer (a deploy overlaps, a GC-paused "zombie" wakes up)? **CAS fencing:**
the manifest carries a `writer_epoch`, and S3's conditional PUT (`If-Match`:
write only if the object version hasn't changed — compare-and-swap, the
primitive the 2008 S3 paper was missing, delivered by AWS in late 2024)
makes epoch-bumping atomic:

```rust
// ILLUSTRATION — not literal slatedb code. The real epoch bump is
// fence.rs:105 (async fn fence), which CASes the manifest object; the
// writer_epoch field is manifest/mod.rs:824.
fn fence(store: &ObjectStore) -> Result<Writer> {
    loop {
        let (m, version) = store.get_manifest()?;      // versioned read
        let me = m.writer_epoch + 1;                    // claim the next epoch
        let next = m.with_writer_epoch(me);
        // CAS: PUT if-match version — S3 rejects concurrent writers
        match store.put_manifest_if_version(&next, version) {
            Ok(_) => return Ok(Writer { epoch: me }),   // fenced in; any zombie's
            Err(Conflict) => continue,                  //   next CAS sees a newer
        }                                               //   epoch and MUST die
    }
}
// every later state change re-CASes the manifest carrying `epoch`,
// so a paused writer can never publish after being fenced.
```

Consensus outsourced to S3's conditional PUT: no leases, no election
timeouts (a stalled writer blocks nobody — but is only *detected* when it
next tries to CAS, Q2). The compactor runs as a separate process with its
own `compactor_epoch`, fenced the same way.

### Step 4 — the cache ladder: buying back the latency

> **In:** the tens-of-ms GET latency from Step 1 and a read that must
> locate blocks inside an SST. **Out:** the three-tier cache ladder (each
> tier a different granularity) that keeps warm reads off S3, in both
> codebases.

Reads pay S3 latency plus a per-GET fee, so SlateDB stacks three tiers,
each with its own granularity: an in-memory **block cache** (SST blocks,
~4 KiB); a local-disk **part cache** (objects split into fixed
`part_size_bytes` parts — our cache.rs stub's production form); and S3
itself, hit with **ranged GETs** that fetch only the blocks a lookup needs,
located via the SST's index — never the whole file. RAM → local disk → S3:
the buffer pool (topic 6) reborn as a tier ladder where a miss costs our
measured ~14 ms *and* a line item on the bill. Quickwit runs the same
ladder at different granularities — byte ranges and whole splits (Step 6).

### Step 5 — checkpoints and clones: copy the manifest, not the data

> **In:** the immutable data objects (Step 1) and the manifest pointer
> (Step 2). **Out:** why a checkpoint and a clone are both metadata-only
> operations, completing the topic's copy-on-write trilogy.

Because all data objects are immutable and the manifest is just a list
(Steps 1–2), a **checkpoint** = pin a manifest version (GC must keep its
SSTs), and a **clone** = a new database whose manifest *references the
parent's SSTs* — zero bytes copied, Neon-branch shaped, Snowflake-clone
shaped. The whole CoW-branching trilogy of this topic (page-, file-, and
SST-granularity) comes from the same two ingredients: immutable data +
one small mutable pointer.

### Step 6 — Quickwit's bundle + hotcache: one GET to open an index

> **In:** the per-request fee from Step 1 and an index made of dozens of
> small files. **Out:** the single-object packaging (a split) and the
> footer (hotcache) that turn "open a searchable index" into one GET.

Per-request economics punish small files hardest, so Quickwit packs an
entire index segment — dozens of files — into **one object** (a **split**),
and appends a **hotcache** footer: the file-offset map plus the hottest
index structures (term-dictionary front layers, field offsets — topic 23).
Opening a searchable index = one GET for the footer (or two: tail then
body); every later read is a precisely-aimed ranged GET. The format is
request-count economics made physical, and Q4 asks what the
FalkorDB-snapshot equivalent footer contains.

### Step 7 — hedged requests: amputating the tail

> **In:** the fat S3 tail from Step 1 (our p99 is ~8× the median).
> **Out:** the one technique that collapses that tail — fire a second GET
> when the first is slow — and its cost, worked on the measured numbers.

S3's tail is fat: our bench measures p99 112.99 ms against p50 14.17 ms,
so the tail is 112.99 / 14.17 ≈ **8× the median** — and no cache helps a
*first* read. The fix is a **hedged request**: set a deadline around the
observed p95 (our notes.md records S3 p95 27.18 ms); if the GET hasn't
answered by then, fire a second identical GET and take whichever returns
first. Since only ~5% of requests cross p95 and hedge, the extra load is
under ~10% more GETs, but the p99 collapses toward the p95 — our bench
pays the tail down from ~113 ms to ~30–35 ms (a hedge costs the p95
deadline plus a fresh ~14 ms sample, so it floors just above the 27 ms
p95, not exactly at it). This is AWS's own S3 guidance, cited
in Quickwit's `TimeoutAndRetryStorage` (our hedge.rs stub, with the
deadline exposed as `StorageTimeoutPolicy` config).

## Where each step lives in the code

All line numbers verified against the pinned clones (`slatedb@323ed1b`,
`quickwit@a5ad540`).

SlateDB anchors (Steps 1–5, paths under `slatedb/src/`):

| anchor | what it is |
|---|---|
| `db.rs:205` / `db.rs:882` | `get_with_options` — memtable → L0 → runs, same read path as topic 4; `:205` is the crate-internal impl, `:882` the public entry; `:309 maybe_apply_backpressure` (Step 1) |
| `tablestore.rs:37/:348/:797/:835` | `TableStore` (`:37`) — SSTs as objects; `write_sst` (`:348`); `read_blocks`/`read_blocks_using_index` (`:797`/`:835`) fetch only needed 4 KiB blocks via ranged GETs (Steps 1, 4) |
| `cached_object_store/object_store.rs:34/:198` | local part cache: `part_size_bytes` field (`:34`) — objects split into parts on local disk; `cached_head` (`:198`) — our cache.rs stub's production form (Step 4) |
| `db_cache/` (moka.rs, foyer.rs) | in-memory block cache layer above the part cache — a 3-level ladder: RAM → local disk → S3 (Step 4) |
| `manifest/mod.rs:824` | `writer_epoch` / `compactor_epoch` fields (Steps 2–3) |
| `fence.rs:105` | `fence()` — bump your epoch via **CAS on the manifest object**; a zombie writer's next manifest CAS fails. Single-writer safety WITHOUT a lease service — consensus outsourced to S3 conditional PUT (the 2008 paper's missing primitive, delivered by AWS in late 2024) (Step 3) |
| `checkpoint.rs:30`, `clone.rs:38` | `create_checkpoint` pins a manifest version; `create_clone` = new DB whose manifest *references the parent's SSTs* — zero-copy CoW clone, Neon-branch shaped (Step 5) |
| `manifest/invariants.rs:42` | the fencing invariant, stated as a doc'd invariant with a wall-clock-skew argument (Step 3) |

Quickwit anchors (Steps 6–7, paths under `quickwit/quickwit-storage/src/`
and `quickwit/quickwit-config/src/`):

| anchor | what it is |
|---|---|
| `bundle_storage.rs:40/:131` | `BundleStorage` (`:40`) — a split = ONE object bundling all index files; `BundleStorageFileOffsets` (`:131`) is the **hotcache** file-offset map — one GET bootstraps a searchable index (Step 6) |
| `timeout_and_retry_storage.rs:37/:89` | `TimeoutAndRetryStorage` (`:37`, header links AWS's S3 latency guidance) and its `get_slice` retry loop (`:89`) — **hedged/retried GETs** — our hedge.rs stub (Step 7) |
| `node_config/mod.rs:608` | `StorageTimeoutPolicy` — the hedge deadline as config (Step 7) |
| `split_cache/mod.rs:43/:123` | `SearchSplitCache` (`:43`) — whole-split local cache; `evict` (`:123`) is its explicit eviction policy (Step 4) |
| `cache/byte_range_cache.rs` | byte-range cache — quickwit caches *ranges*, slatedb caches *parts*, we cache *blocks*: same trick, three granularities (Step 4) |

The convergence table (M28's menu):

| pathology | slatedb answer | quickwit answer | our stub |
|---|---|---|---|
| tens-of-ms GETs | RAM+disk block/part caches | split cache + byte-range cache | cache.rs |
| fat tail | retries in object_store client | TimeoutAndRetryStorage hedging | hedge.rs |
| per-request $ | big SSTs, block-granular ranged GETs | one-object bundles + hotcache | (block granularity) |
| no rename/atomicity | manifest CAS + epochs | immutable splits + metastore | — |
| cheap copies | checkpoint/clone over shared SSTs | splits shared by reference | branch.rs |

## Questions to answer in notes.md

**Q1.** Walk the write path and find every place latency is bought back:
WAL batching (many puts per WAL SST), `AwaitDurable` opt-out, memtable
serving reads before flush. Then state the residual: what is the *floor*
on durable-commit latency for an S3-only LSM, and why do Neon/Socrates
class systems refuse to pay it (they keep a fast landing zone)?

**Q2.** Fencing: writer A stalls (GC pause), writer B fences with
epoch+1, A wakes and tries to CAS the manifest. Trace why A's write MUST
fail and what A must do (die). Compare topic 15's Raft leadership — what
replaces the election timeout, and what's the availability cost of having
no leases (a stalled writer blocks nothing, but detection is lazy)?

**Q3.** Compaction runs as a separate process with its own epoch. Why is
"compactor and writer race" safe when both only ever *add* objects and
CAS the manifest — which single object is the linearization point for the
entire database state?

**Q4.** The hotcache: quickwit appends the "what's where + hottest
structures" bytes at the END of the bundle so one GET (or two: tail then
body) opens an index. Which topic 23 structures make it into the hotcache
(term dictionary FSTs' first layers, field offsets), and what's the
FalkorDB analogue for a graph snapshot object — what belongs in the footer
so a reader can route its *second* GET precisely (matrix block index /
offsets, label→matrix directory, node-count header)?

## Done when

Answer each before unfolding it.

- [ ] You can re-price the LSM when a durable write costs ~100 ms and say
  which decisions invert.
  <details><summary>Answer</summary>

  Only durability reprices: a local fsync is ~0.10 ms (our local p50) but
  a durable S3 write is 14–113 ms (our S3 p50/p99) — ~140× worse at the
  median, ~1,130× at the tail. So SlateDB batches many puts per WAL SST
  and lets callers choose `AwaitDurable` vs no-sync; reads (memtable → L0 →
  runs) are unchanged.
  </details>

- [ ] You can explain why the manifest is the whole database and what that
  buys.
  <details><summary>Answer</summary>

  All data objects (WAL/L0/run SSTs) are immutable, so the only mutable
  truth is one small object — the manifest — listing live SSTs, epochs, and
  checkpoints. Publishing a new manifest version is the single atomic state
  change (the linearization point), which is what lets writer and compactor
  add objects concurrently without a lock.
  </details>

- [ ] You can explain single-writer fencing from one conditional PUT.
  <details><summary>Answer</summary>

  The manifest carries a `writer_epoch`; claiming it means reading the
  versioned manifest and doing an `If-Match` (CAS) PUT of epoch+1. Two
  writers can't both win the CAS, and every later state change re-CASes
  carrying the epoch, so a fenced/zombie writer's next manifest write fails
  and it must die. Consensus is outsourced to S3's conditional PUT — no
  leases, no election timeout.
  </details>

- [ ] You can describe the cache ladder that buys back the latency,
  against this topic's measured S3 p50 of 14.17 ms.
  <details><summary>Answer</summary>

  Three tiers at three granularities: an in-memory block cache (~4 KiB SST
  blocks), a local-disk part cache (`part_size_bytes` parts), and S3 hit
  with ranged GETs located via the SST index. A miss costs the measured
  ~14 ms plus a per-GET fee, so warm reads stay in RAM/local disk; Quickwit
  runs the same ladder over byte-ranges and whole splits.
  </details>

- [ ] You can explain checkpoints and clones as copying the manifest, not
  the data.
  <details><summary>Answer</summary>

  Because data objects are immutable, a checkpoint just pins a manifest
  version (GC must retain its SSTs) and a clone just writes a new manifest
  that references the parent's SSTs — zero bytes copied. Same
  copy-on-write shape as Neon branches and Snowflake clones, at SST
  granularity.
  </details>

- [ ] You can explain hedged requests and predict their effect on the
  measured p99 of 112.99 ms before implementing `hedge.rs`.
  <details><summary>Answer</summary>

  Set a deadline near the observed p95 (our S3 p95 27.18 ms); if a GET is
  slower, fire a second identical GET and take the first to return. Only
  ~5% of requests cross p95, so it costs under ~10% more GETs but pulls the
  p99 down from ~113 ms toward the ~27 ms p95 — our bench measures the
  hedged p99 at ~30–35 ms (a rescued straggler pays the p95 deadline plus a
  fresh sample). The fat tail (≈8× the median) is amputated without hurting
  the median.
  </details>

## References

**Code**
- [slatedb](https://github.com/slatedb/slatedb) `slatedb/src/`, pinned at
  `323ed1b` — anchors above: `db.rs`, `tablestore.rs`,
  `cached_object_store/`, `db_cache/`, `manifest/`, `fence.rs`,
  `checkpoint.rs`, `clone.rs`. Every file:line re-verified at that commit
  with `tools/pinned-source.py`.
- [quickwit](https://github.com/quickwit-oss/quickwit)
  `quickwit/quickwit-storage/src/` (and `quickwit-config/`), pinned at
  `a5ad540` — `bundle_storage.rs`, `timeout_and_retry_storage.rs`,
  `split_cache/`, `cache/byte_range_cache.rs`, `node_config/mod.rs`; the
  storage tricks generalize.
- turso's object-store backend is in flight upstream; the slatedb patterns
  are what it converges to.
- S3's conditional-write (CAS) primitive shipped in AWS's late-2024
  announcement, not in either 2008/2016 paper.
