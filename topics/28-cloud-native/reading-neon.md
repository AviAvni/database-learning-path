# Neon: page versions from WAL, branches for free

Aurora's idea, reimplemented for stock Postgres, in Rust, in the open —
the best codebase to *read* for this topic. Compute streams WAL to a
safekeeper quorum for durability; a pageserver indexes page versions by
(key, LSN) and reconstructs any page at any LSN on demand, which is
also why a branch costs O(1). This chapter builds those pieces step by
step — the Postgres contract Neon hooks into, the durability/serving
split, the version index, reconstruction, and branches — then hands you
the anchors to walk both crates.

## The problem in one sentence

Give stock Postgres bottomless, branchable storage without touching the
engine: commits must be durable in ~1 ms (so S3's ~15 ms is off the
commit path), any page must be readable *as of any point in history*, and
creating a full copy-on-write branch of a multi-TB database must cost
O(1), not O(size).

## The concepts, step by step

### Step 1 — the Postgres contract: WAL in, pages out

Postgres stores tables as 8 KB **pages** and, before touching any page,
appends a **WAL record** (write-ahead log — a small description of the
change) at a monotonically increasing byte offset called the **LSN** (log
sequence number). Every page version is therefore addressable as "page X
as of LSN N", and any version is derivable from an older copy plus the
WAL records in between (REDO, topic 5). Postgres also has a narrow
storage-manager interface (**smgr**) — "read page / write page" — that
Neon replaces with a network hook. That's the entire integration surface:
Neon receives the WAL stream and must answer one question,
`GetPage@(key, LSN)`, where key identifies the page. Compute stays
stateless: its local disk is pure cache.

### Step 2 — split durability from serving: safekeepers and the pageserver

The write path and the read path get different services, sized for
opposite requirements (the Socrates lesson):

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

A commit is durable the moment a **quorum** (2 of 3) of **safekeepers** —
small nodes whose only job is landing WAL on fast disks, coordinated by a
term-based, Raft-flavored protocol ("Paxos-ish" per their docs) — has the
record. Commit latency never touches S3. The **pageserver** consumes the
WAL stream *asynchronously*: it may lag, crash, or be rebuilt, because
everything it holds is derivable from WAL + S3. Rosetta to the previous
chapters: safekeepers = Socrates' XLOG landing zone, pageserver = page
servers, S3 = XStore — the same decomposition, arrived at independently.

### Step 3 — the pageserver is an LSM over (key, LSN)

The pageserver's job is to index *page versions*, so its key space is
two-dimensional: (page key × LSN). It stores that history in two kinds of
immutable **layer files**, each covering a rectangle of that space:
**delta layers** hold raw WAL records sorted by (key, LSN); **image
layers** hold fully **materialized** pages (the actual 8 KB bytes) as of
one LSN. Fresh WAL is ingested into delta layers; compaction periodically
produces image layers so history doesn't have to be replayed from the
beginning; old layers are uploaded to S3 and dropped from local disk; GC
deletes history below the retention horizon. The mental model: **an LSM
(topic 4) over the key space (key, LSN)** — delta layers = level files,
image layers = the compacted form — topic 4 for the third time, after GIN
pending lists and differential arrangements (topic 27 notes).
`LayerMap::search(key, end_lsn)` is the 2-D lookup that answers "which
layers can contain versions of `key` at or below `end_lsn`".

### Step 4 — reconstruction: REDO on the read path

`GetPage@(key, LSN)` is answered by *rebuilding* the page: find the
newest image layer at or below the requested LSN, collect every delta
(WAL record) between that image and the LSN, and replay them — using an
actual sandboxed Postgres subprocess (**walredo**) as the replay engine,
so Neon never reimplements record semantics. Topic 5's REDO, promoted
from crash recovery to the everyday read path. The loop, reduced:

```rust
fn get_page(tl: &Timeline, key: Key, lsn: Lsn) -> Page {
    let (mut tl, mut lsn) = (tl, lsn);
    let mut deltas = vec![];
    loop {
        match tl.layers.search(key, lsn) {            // 2-D (key × LSN) search
            Found::Image(img) => {                    // ONE image suffices...
                return walredo(img, deltas);          // ...replay deltas on it
            }                                         //    (REDO on the READ path)
            Found::Delta(rec, below) => {             // collect, keep descending
                deltas.push(rec); lsn = below;
            }
            Found::Nothing => {                       // not born on this timeline:
                lsn = tl.ancestor_lsn.min(lsn);       // ask the parent, capped at
                tl = tl.ancestor();                   //   the branch point
            }
        }
    }
}
```

The cost gradient: a read needs at most **one** image layer but possibly
**many** delta layers — so compaction (creating fresh image layers) is
what caps read latency (Q1). One more subtlety: because the pageserver
may lag the safekeepers, compute sends the LSN it needs with each
request, and the pageserver *waits* until it has ingested up to that LSN
rather than serving a stale page (Q3).

### Step 5 — branches: a branch is two numbers

A **branch** (timeline) is metadata: `(parent timeline, branch LSN)` —
created in O(1), copying nothing, because history is already immutable
and indexed by LSN (Step 3). Reads on a child that miss its own layers
fall through to the parent, *capped at the branch LSN* — the
`Found::Nothing` arm in the Step 4 loop, and exactly our branch.rs stub.
The same trick as Snowflake's file-list clones and SlateDB's
manifest-reference clones, at page granularity. Two costs to watch: a
deep branch chain makes reads walk many ancestors (Neon bounds this by
materializing image layers *into* child timelines over time — Q4), and
GC gets harder — a layer may be garbage for the child but live for the
parent, so every child's branch LSN becomes a retain point (Q2).

## Where each step lives in the code

Pageserver anchors (Steps 3–5, the read path):

| anchor | what it is |
|---|---|
| `pageserver/src/pgdatadir_mapping.rs:258` | `get_rel_page_at_lsn` — the public question: (relation, block, LSN) → page (Step 1's contract) |
| `pageserver/src/tenant/timeline.rs:1227/:1339` | `Timeline::get` / `get_vectored` — batched key×LSN reads (Step 4) |
| `timeline.rs:4491` | `get_vectored_reconstruct_data` — gather image + deltas needed to rebuild each page (Step 4) |
| `timeline.rs:4548` | the **ancestor walk**: keys not found on this timeline are re-asked of `ancestor_timeline` capped at the branch LSN — our branch.rs stub verbatim (Step 5) |
| `tenant/layer_map.rs:71/:448/:596` | `LayerMap::search(key, end_lsn)` — which layers can contain versions of `key` below `end_lsn`; a 2-D (key × LSN) search structure (Step 3) |
| `tenant/storage_layer/delta_layer.rs:213` | `DeltaLayer` — sorted (key, LSN) → WAL record files (Step 3) |
| `tenant/storage_layer/image_layer.rs:148` | `ImageLayer` — materialized pages at one LSN (Step 3) |
| `pageserver/src/walredo.rs:55/:173/:473` | `PostgresRedoManager::request_redo` → `apply_wal_records` in a *sandboxed Postgres subprocess* — topic 5's REDO on the read path (Step 4) |
| `pageserver/src/tenant.rs:4985` | `branch_timeline_impl` — a branch is metadata: (ancestor, ancestor_lsn). O(1). (Step 5) |

For Q2, also look at how `gc_info.insert_child` (tenant.rs:588-592)
registers `ancestor_lsn` as a retain point.

Safekeeper anchors (Step 2, the write path):

| anchor | what it is |
|---|---|
| `safekeeper/src/safekeeper.rs:292` | `AppendRequest` — WAL push protocol messages, term-based (Raft-flavored, "Paxos-ish" per their docs) |
| `safekeeper/src/wal_storage.rs` | segment files on safekeeper disk — the durable landing zone |
| `safekeeper/src/wal_backup.rs` | offload of safekeeper WAL to S3 once pageserver has consumed it |
| `safekeeper/src/timeline_eviction.rs` | evict cold timelines from safekeeper disk — even the landing zone tiers to S3 |

Socrates rosetta: safekeepers = XLOG landing zone; pageserver = page
servers; S3 = XStore. Same decomposition, independent arrival.

## Questions to answer in notes.md

**Q1.** `LayerMap::search` answers "newest layer that could hold (key,
≤ lsn)". Why does reconstruction need at most ONE image layer but possibly
MANY delta layers, and what does compaction (creating new image layers)
buy — in *our* tier_bench vocabulary, which lane's latency does it cap?

**Q2.** Branches make GC hard: a layer can be garbage for the child but
live for the parent (or vice versa). Look at how `gc_info.insert_child`
(tenant.rs:588-592) registers `ancestor_lsn` as a retain point. State the
GC rule in one sentence. (Keep everything ≥ min over children's branch
LSNs and the PITR horizon.)

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

## References

**Code**
- [neon](https://github.com/neondatabase/neon) — pageserver +
  safekeeper crates (Rust); read path anchors in
  `pageserver/src/pgdatadir_mapping.rs`, `tenant/timeline.rs`,
  `tenant/layer_map.rs`, `tenant/storage_layer/`,
  `pageserver/src/walredo.rs`; write path in `safekeeper/src/`
- Neon architecture posts: "Architecture decisions in Neon", "Get page
  at LSN" docs in `docs/` in-repo (skim `docs/pageserver-storage.md` &
  `docs/walservice.md` equivalents if present)
