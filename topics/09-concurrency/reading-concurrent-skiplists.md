# Reading guide — two concurrent skiplists: RocksDB (CAS) vs memgraph (lazy locking) (~2 h)

Same structure, two schools. Read RocksDB first (you know this file from
topic 2 — now the concurrency), then memgraph as the contrast.

## 1. RocksDB InlineSkipList — CAS school
`~/repos/rocksdb/memtable/inlineskiplist.h`

- The contract (:23): `InsertConcurrently` is safe with concurrent reads
  AND writes — but the LSM makes it easier: memtable entries are
  **never deleted** (topic 4: deletion = tombstone insert; the whole
  memtable dies at flush). No delete ⇒ no reclamation problem ⇒ no
  epochs needed. Always ask "what did the workload let them NOT solve?"
- `CASNext` (:393): the linking primitive — one
  `compare_exchange_strong` per level. Insert per level: read pred/succ,
  set `new->next = succ` (relaxed — unpublished), CAS pred->next from
  succ to new; on failure re-find just that level and retry.
- `Splice` (:64): a cached array of (pred, succ) per level — the search
  is the expensive part, so sequential writers reuse the previous
  insert's splice (`Insert(key, splice, ...)` :1028, hint variant :113)
  and `RecomputeSpliceLevels` (:331/:1016) repairs only the invalid
  levels. Amortize the O(log n) search across nearby inserts.
- `Insert` :908 vs `InsertConcurrently` :913 — same template, `UseCAS`
  flag: single-writer mode skips atomics. The single-writer fast path
  is a compile-time choice. (M9 note: FalkorDB's single writer can take
  exactly this door.)

## 2. memgraph SkipList — lazy-locking school (Herlihy et al.)
`~/repos/memgraph/src/utils/skip_list.hpp`

- Node (:156): per-node `SpinLock` (:163), `marked` (:164),
  `fully_linked` (:165), flexible-array tower `nexts[0]` (:169) — the
  same intrusive-tower trick as RocksDB, plus TWO state bits.
- Insert (:1335): `find_node` (:1285) collects preds/succs, LOCK the
  preds bottom-up, re-validate, link all levels, then PUBLISH with
  `fully_linked.store(true, release)` (:1398). Readers ignore
  half-linked nodes — publication idiom with a bit instead of a CAS'd
  pointer.
- Remove (:1655): lock, `marked.store(true, release)` (:1672) —
  logical delete first (readers skip marked nodes), THEN unlink.
  Deletion exists here, so reclamation must too:
- **Accessor-id GC** (:244–246, `SkipListGc` :257, `Collect` :367): every
  `Accessor` (:877) gets a monotonically increasing id; a retired node
  records the newest alive accessor id; free when all older accessors
  are gone. Epoch reclamation with transaction-scoped pins — compare
  crossbeam's 3-epoch scheme; same idea, coarser pin.
- `kSkipListGcHeightTrigger` (:69) and `create_chunks` (:817–955 —
  chunked parallel iteration for analytics) show this is the SPINE of
  memgraph: vertices, edges, and indexes all live in these lists.

## 3. The comparison table (fill it in notes.md)

| | RocksDB | memgraph |
|---|---|---|
| writers coordinate by | CAS per level | per-node spinlocks |
| readers see partial insert? | yes — per-level linking is independent (fine for a set) | no — fully_linked gate |
| delete | never (tombstones) | marked bit + unlink |
| reclamation | none needed (arena dies at flush) | accessor-id GC |
| failure/retry | re-find level, re-CAS | unlock all, restart |

## Questions for notes.md

1. RocksDB dodged reclamation via arena-per-memtable. What's the graph
   equivalent — arena per matrix version? Does M8's CoW give M9 the same
   dodge (old version dies wholesale when last reader leaves)?
2. Why does the lazy list lock preds BOTTOM-up and validate after
   locking? Construct the lost-insert without validation.
3. A splice cache assumes locality of consecutive inserts. Does a graph
   bulk-load (sorted node ids) hit that path? What about random edges?
4. Which school for YOUR concurrent_set.rs — and what does crossbeam-epoch
   give you that lets you pick CAS *with* deletion (the combination
   neither production list needed)?

## Done when

You can fill the table from memory and explain what each system's
workload allowed it to NOT build.
