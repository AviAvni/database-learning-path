# The minimal transactional KV interface: surrealdb's kvs layer

surrealdb doesn't implement MVCC — it *abstracts over* engines that do
(tikv, foundationdb, rocksdb, in-memory...), which forces it to define the
minimal transactional interface a multi-model DB needs. Read this one for
ARCHITECTURE, not algorithms: that interface is a good checklist for M8's
storage-backend abstraction (M1).

## 1. The layering

```
 Datastore (ds.rs) ── transaction() :3353 ──► Transaction (tx.rs:94)
                                                 │ caching + typed keys
                                                 ▼
                                              Transactor (tr.rs:37)
                                                 │ uniform async KV-txn API
                                                 ▼
                                   engine flavor (mem/rocksdb/tikv/fdb…)
```

- `TransactionType` (tr.rs:15): just `Read | Write` — declared UP FRONT at
  begin. Compare postgres (any txn can write) — declaring intent enables
  single-writer engines and read-only fast paths.
- `LockType` on `Datastore::transaction()` (ds.rs:3353): `Optimistic |
  Pessimistic` — the CHOICE of school is a per-transaction parameter passed
  down to engines that support both. The two RocksDB flavors you just read
  are literally behind this flag.
- `TransactionFactory` (ds.rs:314) / builder plumbing (ds.rs:450–571):
  the multi-backend dispatch. M1's `StorageBackend` trait, grown up.

## 2. The Transactor API (tr.rs) — read the signatures

- `get`/`getm`/`getr`/`getp` (:119–155) all take `version: Option<u64>` —
  **versioned point-in-time reads are part of the public KV contract**, not
  an engine internal. (Only some engines honor it; capability, not
  guarantee.)
- `set` :166 vs `put` :190 vs `putc` :202 — put fails if the key exists;
  putc is compare-and-set on the current value: optimistic concurrency
  primitives exposed as API, so upper layers can do OCC over any engine.
- `commit` :103 / `cancel` :95 — commit is where engine-level conflict
  errors surface; the query layer retries.

## 3. Transaction (tx.rs:94, impl :693)

Wraps Transactor with typed keys and read-through caches. The thing to
notice: caching inside a transaction is trivially correct — the snapshot
is immutable, so a within-txn cache never invalidates. (Topic 6's hardest
problem — invalidation — deleted by MVCC.)

## Questions for notes.md

1. `version: Option<u64>` on every read: what does time-travel-as-API cost
   the engines that support it (GC can't drop what an API can name)?
2. Read/Write declared at begin: what optimizations does that unlock for
   a single-writer engine? What does FalkorDB's GRAPH.RO_QUERY vs
   GRAPH.QUERY split already encode?
3. putc (CAS) as the portable OCC primitive: sketch how you'd build
   first-committer-wins snapshot isolation on top of ONLY get/putc.
4. M1 retrospective: does your storage-backend trait from topic 1 admit a
   transactional backend, or did you bake in auto-commit? What would you
   change now?

## Done when

You can list the 6–8 operations a transactional KV interface needs to
support a multi-model DB, and say which are capabilities vs guarantees.

## References

**Code**
- [surrealdb](https://github.com/surrealdb/surrealdb) —
  `surrealdb/core/src/kvs/`: `ds.rs`, `tr.rs`, `tx.rs`; ~1 h — read the
  Transactor signatures in tr.rs, they ARE the interface checklist
