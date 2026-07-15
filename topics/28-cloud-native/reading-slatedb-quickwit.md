# SlateDB & Quickwit: born on S3

Neon and Aurora retrofit object storage under an existing engine;
SlateDB (an LSM whose ONLY disk is an object store) and Quickwit
(search over S3) were *born* there — so every S3 pathology has an
explicit, readable countermeasure in their code. This chapter builds
each countermeasure step by step — the re-priced LSM, the manifest as
the single point of truth, CAS fencing, the cache ladder, zero-copy
clones, bundles, and hedged reads — then hands you the anchors. It is
the menu M28's tiered-storage stubs are ordered from.

## The problem in one sentence

An engine whose only disk is S3 inherits four taxes at once — ~15 ms
median / ~113 ms p99 per GET, a fee per request, no atomic multi-object
operations, and no locks to keep two writers apart — and every structure
in these two codebases exists to pay one of them down.

## The concepts, step by step

### Step 1 — the LSM, re-priced: when fsync costs 50–100 ms

An **LSM** (topic 4) buffers writes in a sorted in-memory table (the
**memtable**), logs them to a **WAL** (write-ahead log) for durability,
and periodically flushes immutable sorted files (**SSTs**) that a
background **compactor** merges. SlateDB keeps that machine intact and
swaps every disk write for an S3 PUT:

```
 put ──► WAL buffer ──► WAL SSTs on S3 ──► memtable flush ──► L0 SSTs ──► runs
          (batch!)       ~50-100 ms/put             compactor (separate process,
   AwaitDurable vs no-sync = the fsync            fenced by compactor_epoch)
   trade (topic 5), now costing 100 ms                        │
                         manifest on S3, updated via CAS ◄────┘
```

The repricing bites exactly once: durability. A local fsync is ~0.1–1 ms;
a WAL-SST PUT is ~50–100 ms, so SlateDB batches many puts per WAL object
and offers `AwaitDurable` (wait for the PUT) vs no-sync (return once in
the memtable) — topic 5's fsync trade with the price multiplied by 100.
That floor is *why* Neon/Socrates-class systems keep a fast landing zone
(Q1). Reads are untouched: memtable → L0 → sorted runs, same as topic 4.

### Step 2 — the manifest: the entire database is one small object

Since S3 objects are immutable-in-practice (PUTs replace, never modify)
and there's no atomic multi-object commit, SlateDB makes every data
object (WAL SSTs, L0 SSTs, compacted runs) **immutable** and gathers the
mutable truth into one small object: the **manifest** — the list of live
SSTs plus epochs and checkpoint metadata. A state change = write new
immutable objects (add-only, harmless), then publish one new manifest
version. The manifest is the **linearization point** (the single place
where "what the database is" changes atomically) — the same move as
Snowflake's table-version file lists, at engine granularity. This is Q3's
answer taking shape: writer and compactor can race freely on *data*
objects because only the manifest CAS decides.

### Step 3 — fencing: single-writer safety from one conditional PUT

With no lock service, what stops two processes both believing they're the
writer (a deploy overlaps, a GC-paused "zombie" wakes up)? **CAS
fencing**: the manifest carries a `writer_epoch`, and S3's conditional
PUT (`If-Match`: write only if the object version hasn't changed —
compare-and-swap, the primitive the 2008 S3 paper was missing, delivered
2024) makes epoch-bumping atomic:

```rust
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

### Step 4 — the cache ladder: buying back the 15 ms

Reads pay S3 latency plus a per-GET fee, so SlateDB stacks three tiers,
each with its own granularity: an in-memory **block cache** (SST blocks,
~4 KiB); a local-disk **part cache** (objects split into fixed
`part_size_bytes` parts — our cache.rs stub's production form); and S3
itself, hit with **ranged GETs** that fetch only the blocks a lookup
needs, located via the SST's index — never the whole file. RAM → local
disk → S3: the buffer pool (topic 6) reborn as a tier ladder where a miss
costs 15 ms *and* a line item on the bill. Quickwit runs the same ladder
at different granularities — byte ranges and whole splits (Step 6).

### Step 5 — checkpoints and clones: copy the manifest, not the data

Because all data objects are immutable and the manifest is just a list
(Steps 1–2), a **checkpoint** = pin a manifest version (GC must keep its
SSTs), and a **clone** = a new database whose manifest *references the
parent's SSTs* — zero bytes copied, Neon-branch shaped, Snowflake-clone
shaped. The whole CoW-branching trilogy of this topic (page-, file-, and
SST-granularity) comes from the same two ingredients: immutable data +
one small mutable pointer.

### Step 6 — Quickwit's bundle + hotcache: one GET to open an index

Per-request economics punish small files hardest, so Quickwit packs an
entire index segment — dozens of files — into **one object** (a
**split**), and appends a **hotcache** footer: the file-offset map plus
the hottest index structures (term-dictionary front layers, field
offsets — topic 23). Opening a searchable index = one GET for the footer
(or two: tail then body); every later read is a precisely-aimed ranged
GET. The format is request-count economics made physical, and Q4 asks
what the FalkorDB-snapshot equivalent footer contains.

### Step 7 — hedged requests: amputating the tail

S3's tail is fat — p99 ~8× the median (14 ms → 113 ms in our bench) —
and no cache helps a *first* read. The fix is a **hedged request**: set a
deadline around the observed p95; if the GET hasn't answered by then,
fire a second identical GET and take whichever returns first. Since only
~5% of requests hedge, the extra cost is <10% more GETs, but the p99
collapses toward the p95 (AWS's own S3 guidance, cited in Quickwit's
`TimeoutAndRetryStorage` — our hedge.rs stub, with the deadline exposed
as `StorageTimeoutPolicy` config).

## Where each step lives in the code

SlateDB anchors (Steps 1–5):

| anchor | what it is |
|---|---|
| `db.rs:205/:842` | `get_with_options` — memtable → L0 → runs, same read path as topic 4; `:309 maybe_apply_backpressure` (Step 1) |
| `tablestore.rs:37/:348/:797/:835` | `TableStore` — SSTs as objects; `read_blocks_using_index` fetches only needed 4 KiB blocks via ranged GETs (Steps 1, 4) |
| `cached_object_store/object_store.rs:34/:198` | local part cache: objects split into `part_size_bytes` parts, cached on local disk — our cache.rs stub's production form (Step 4) |
| `db_cache/` (moka.rs, foyer.rs) | in-memory block cache layer above the part cache — a 3-level ladder: RAM → local disk → S3 (Step 4) |
| `manifest/mod.rs:824` | `writer_epoch` / `compactor_epoch` (Steps 2–3) |
| `fence.rs:105` | `fence()` — bump your epoch via **CAS on the manifest object**; a zombie writer's next manifest CAS fails. Single-writer safety WITHOUT a lease service — consensus outsourced to S3 conditional PUT (the 2008 paper's missing primitive, delivered 2024) (Step 3) |
| `checkpoint.rs:30`, `clone.rs:38` | checkpoints pin a manifest version; `create_clone` = new DB whose manifest *references the parent's SSTs* — zero-copy CoW clone, Neon-branch shaped (Step 5) |
| `manifest/invariants.rs:42` | the fencing invariant, stated as a doc'd invariant with a wall-clock-skew argument (Step 3) |

Quickwit anchors (Steps 6–7):

| anchor | what it is |
|---|---|
| `quickwit-storage/src/bundle_storage.rs:40/:131` | a split = ONE object bundling all index files + a **hotcache** footer (the file-offset map + hot bytes) — one GET bootstraps a searchable index; request-count economics drove the format (Step 6) |
| `quickwit-storage/src/timeout_and_retry_storage.rs:37/:89` | **hedged/retried GETs**: if a ranged read exceeds the timeout policy, retry aggressively (cites AWS's own S3 latency guidance) — our hedge.rs stub (Step 7) |
| `quickwit-config/src/node_config/mod.rs:608` | `StorageTimeoutPolicy` — the hedge deadline as config (Step 7) |
| `quickwit-storage/src/split_cache/mod.rs:43/:123` | whole-split local cache with explicit admit/evict policy (Step 4) |
| `quickwit-storage/src/cache/byte_range_cache.rs` | byte-range cache — quickwit caches *ranges*, slatedb caches *parts*, we cache *blocks*: same trick, three granularities (Step 4) |

The convergence table (M28's menu):

| pathology | slatedb answer | quickwit answer | our stub |
|---|---|---|---|
| 15 ms GETs | RAM+disk block/part caches | split cache + byte-range cache | cache.rs |
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

## References

**Code**
- [slatedb](https://github.com/slatedb/slatedb) `slatedb/src/` — the
  anchor table above: `db.rs`, `tablestore.rs`,
  `cached_object_store/`, `db_cache/`, `manifest/`, `fence.rs`,
  `checkpoint.rs`, `clone.rs`
- [quickwit](https://github.com/quickwit-oss/quickwit)
  `quickwit/quickwit-storage/src/` — `bundle_storage.rs`,
  `timeout_and_retry_storage.rs`, `split_cache/`,
  `cache/byte_range_cache.rs`; the storage tricks generalize
- turso's object-store backend is in flight upstream; the slatedb
  patterns are what it converges to
