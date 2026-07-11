# Reading guide — SlateDB & Quickwit: built S3-first (code walk)

**Sources:**
- `~/repos/slatedb/slatedb/src/` — an LSM whose ONLY disk is an object store
- `~/repos/quickwit/quickwit/quickwit-storage/src/` — search over S3;
  the storage tricks generalize
- (turso's object-store backend is in flight upstream; the slatedb patterns
  are what it converges to)

Neon/Aurora retrofit object storage under an existing engine. These two were
*born* on S3 — so every S3 pathology has an explicit, readable countermeasure.

## 1. SlateDB — the LSM from topic 4, re-priced for S3

```
 put ──► WAL buffer ──► WAL SSTs on S3 ──► memtable flush ──► L0 SSTs ──► runs
          (batch!)       ~50-100 ms/put             compactor (separate process,
   AwaitDurable vs no-sync = the fsync            fenced by compactor_epoch)
   trade (topic 5), now costing 100 ms                        │
                         manifest on S3, updated via CAS ◄────┘
```

| anchor | what it is |
|---|---|
| `db.rs:205/:842` | `get_with_options` — memtable → L0 → runs, same read path as topic 4; `:309 maybe_apply_backpressure` |
| `tablestore.rs:37/:348/:797/:835` | `TableStore` — SSTs as objects; `read_blocks_using_index` fetches only needed 4 KiB blocks via ranged GETs |
| `cached_object_store/object_store.rs:34/:198` | local part cache: objects split into `part_size_bytes` parts, cached on local disk — our cache.rs stub's production form |
| `db_cache/` (moka.rs, foyer.rs) | in-memory block cache layer above the part cache — a 3-level ladder: RAM → local disk → S3 |
| `manifest/mod.rs:824` | `writer_epoch` / `compactor_epoch` |
| `fence.rs:105` | `fence()` — bump your epoch via **CAS on the manifest object**; a zombie writer's next manifest CAS fails. Single-writer safety WITHOUT a lease service — consensus outsourced to S3 conditional PUT (the 2008 paper's missing primitive, delivered 2024) |
| `checkpoint.rs:30`, `clone.rs:38` | checkpoints pin a manifest version; `create_clone` = new DB whose manifest *references the parent's SSTs* — zero-copy CoW clone, Neon-branch shaped |
| `manifest/invariants.rs:42` | the fencing invariant, stated as a doc'd invariant with a wall-clock-skew argument |

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

## 2. Quickwit — search's answers to the same pathologies

| anchor | what it is |
|---|---|
| `quickwit-storage/src/bundle_storage.rs:40/:131` | a split = ONE object bundling all index files + a **hotcache** footer (the file-offset map + hot bytes) — one GET bootstraps a searchable index; request-count economics drove the format |
| `quickwit-storage/src/timeout_and_retry_storage.rs:37/:89` | **hedged/retried GETs**: if a ranged read exceeds the timeout policy, retry aggressively (cites AWS's own S3 latency guidance) — our hedge.rs stub |
| `quickwit-config/src/node_config/mod.rs:608` | `StorageTimeoutPolicy` — the hedge deadline as config |
| `quickwit-storage/src/split_cache/mod.rs:43/:123` | whole-split local cache with explicit admit/evict policy |
| `quickwit-storage/src/cache/byte_range_cache.rs` | byte-range cache — quickwit caches *ranges*, slatedb caches *parts*, we cache *blocks*: same trick, three granularities |

**Q4.** The hotcache: quickwit appends the "what's where + hottest
structures" bytes at the END of the bundle so one GET (or two: tail then
body) opens an index. Which topic 23 structures make it into the hotcache
(term dictionary FSTs' first layers, field offsets), and what's the
FalkorDB analogue for a graph snapshot object — what belongs in the footer
so a reader can route its *second* GET precisely (matrix block index /
offsets, label→matrix directory, node-count header)?

## 3. The convergence table (M28's menu)

| pathology | slatedb answer | quickwit answer | our stub |
|---|---|---|---|
| 15 ms GETs | RAM+disk block/part caches | split cache + byte-range cache | cache.rs |
| fat tail | retries in object_store client | TimeoutAndRetryStorage hedging | hedge.rs |
| per-request $ | big SSTs, block-granular ranged GETs | one-object bundles + hotcache | (block granularity) |
| no rename/atomicity | manifest CAS + epochs | immutable splits + metastore | — |
| cheap copies | checkpoint/clone over shared SSTs | splits shared by reference | branch.rs |
