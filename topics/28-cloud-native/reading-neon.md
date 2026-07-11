# Reading guide — Neon: pageserver & safekeepers (code walk)

**Sources:**
- [`~/repos/neon/`](https://github.com/neondatabase/neon) — pageserver + safekeeper crates (Rust)
- Neon architecture posts: "Architecture decisions in Neon", "Get page at
  LSN" docs in `docs/` in-repo (skim `docs/pageserver-storage.md` &
  `docs/walservice.md` equivalents if present)

Aurora's idea, reimplemented for stock Postgres, in Rust, in the open. The
best codebase to *read* for this topic.

## 1. The data flow

```
 Postgres (unmodified + smgr hook)
   │  WAL (streamed, synchronous quorum)          GetPage@(key, LSN)
   ▼                                                     ▲
 safekeepers ×3 ── consensus on WAL ──► pageserver ──────┘
   (durability NOW)                       │  ingests WAL -> delta layers
                                          │  compacts    -> image layers
                                          ▼
                                         S3 (all layers, all history)
```

## 2. Pageserver anchors (the read path)

| anchor | what it is |
|---|---|
| `pageserver/src/pgdatadir_mapping.rs:258` | `get_rel_page_at_lsn` — the public question: (relation, block, LSN) → page |
| `pageserver/src/tenant/timeline.rs:1227/:1339` | `Timeline::get` / `get_vectored` — batched key×LSN reads |
| `timeline.rs:4491` | `get_vectored_reconstruct_data` — gather image + deltas needed to rebuild each page |
| `timeline.rs:4548` | the **ancestor walk**: keys not found on this timeline are re-asked of `ancestor_timeline` capped at the branch LSN — our branch.rs stub verbatim |
| `tenant/layer_map.rs:71/:448/:596` | `LayerMap::search(key, end_lsn)` — which layers can contain versions of `key` below `end_lsn`; a 2-D (key × LSN) search structure |
| `tenant/storage_layer/delta_layer.rs:213` | `DeltaLayer` — sorted (key, LSN) → WAL record files |
| `tenant/storage_layer/image_layer.rs:148` | `ImageLayer` — materialized pages at one LSN |
| `pageserver/src/walredo.rs:55/:173/:473` | `PostgresRedoManager::request_redo` → `apply_wal_records` in a *sandboxed Postgres subprocess* — topic 5's REDO on the read path |
| `pageserver/src/tenant.rs:4985` | `branch_timeline_impl` — a branch is metadata: (ancestor, ancestor_lsn). O(1). |

The mental model: **an LSM over the key space (key, LSN)**. Delta layers =
level files of WAL records; image layers = the "compacted" form; GC =
dropping history below the PITR horizon (respecting branch points!). Topic 4
for the third time, after GIN pending lists and differential arrangements
(topic 27 notes).

**Q1.** `LayerMap::search` answers "newest layer that could hold (key,
≤ lsn)". Why does reconstruction need at most ONE image layer but possibly
MANY delta layers, and what does compaction (creating new image layers)
buy — in *our* tier_bench vocabulary, which lane's latency does it cap?

**Q2.** Branches make GC hard: a layer can be garbage for the child but
live for the parent (or vice versa). Look at how `gc_info.insert_child`
(tenant.rs:588-592) registers `ancestor_lsn` as a retain point. State the
GC rule in one sentence. (Keep everything ≥ min over children's branch
LSNs and the PITR horizon.)

## 3. Safekeeper anchors (the write path)

| anchor | what it is |
|---|---|
| `safekeeper/src/safekeeper.rs:292` | `AppendRequest` — WAL push protocol messages, term-based (Raft-flavored, "Paxos-ish" per their docs) |
| `safekeeper/src/wal_storage.rs` | segment files on safekeeper disk — the durable landing zone |
| `safekeeper/src/wal_backup.rs` | offload of safekeeper WAL to S3 once pageserver has consumed it |
| `safekeeper/src/timeline_eviction.rs` | evict cold timelines from safekeeper disk — even the landing zone tiers to S3 |

Socrates rosetta: safekeepers = XLOG landing zone; pageserver = page
servers; S3 = XStore. Same decomposition, independent arrival.

**Q3.** Commit waits for a safekeeper quorum only — the pageserver may lag.
What read anomaly does GetPage@LSN prevent that a lagging page service
would otherwise cause, and what does compute have to send with each read
request to get it? (The LSN it needs — reads *wait* for the pageserver to
catch up to that LSN rather than returning stale pages.)

**Q4 (M28).** Our branch.rs stub resolves get(branch, page, lsn) by
walking parents. Neon avoids unbounded walks: image layers get *copied
down* (materialized) into child timelines by compaction over time. When
would M28's graph branches need the same trick — what query pattern makes
a 64-deep ancestor walk show up, and what's the graph equivalent of an
image layer (a materialized matrix snapshot at the branch point)?
