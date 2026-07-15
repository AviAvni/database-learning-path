# The minimal transactional KV interface: surrealdb's kvs layer

surrealdb doesn't implement MVCC — it *abstracts over* engines that do
(tikv, foundationdb, rocksdb, in-memory...), which forces it to define the
minimal transactional interface a multi-model DB needs. This chapter
builds that interface concept by concept — why abstracting forces
minimalism, how the layers stack, what gets declared up front, and which
primitives make OCC portable — then points you at the exact signatures.
Read this one for ARCHITECTURE, not algorithms: the interface is a good
checklist for M8's storage-backend abstraction (M1).

## The problem in one sentence

One query engine must run transactions over five-plus storage engines with
wildly different concurrency machinery (single-node rocksdb, distributed
tikv/foundationdb, a plain in-memory map) — so what is the *smallest* set
of operations the query layer can demand from all of them? surrealdb's
answer fits in one trait: roughly eight method signatures in `tr.rs`.

## The concepts, step by step

### Step 1 — abstraction forces minimalism

When a database implements its own storage engine, the transaction
interface can be as fat and idiosyncratic as it likes — it has one caller
and one implementor. When it must run over N third-party engines, every
method in the interface must be implementable by ALL of them, so each
method must be either universal (get, set, commit) or explicitly optional
(a **capability** — "engines that can, do; callers must not assume").

That pressure is the reason to read this code: the interface that survives
it is close to the theoretical minimum for "transactional ordered KV",
which is exactly the contract M8's storage-backend trait needs to name.

### Step 2 — the layering: three structs between query and engine

The stack separates policy (caching, typed keys) from the portable
contract from the engine dispatch:

```
 Datastore (ds.rs) ── transaction() :3353 ──► Transaction (tx.rs:94)
                                                 │ caching + typed keys
                                                 ▼
                                              Transactor (tr.rs:37)
                                                 │ uniform async KV-txn API
                                                 ▼
                                   engine flavor (mem/rocksdb/tikv/fdb…)
```

- `Datastore` owns configuration and mints transactions; its
  `TransactionFactory` (ds.rs:314) and builder plumbing (ds.rs:450–571)
  are the multi-backend dispatch — M1's `StorageBackend` trait, grown up.
- `Transactor` is the uniform async KV-transaction API — the minimal
  interface this chapter is about.
- `Transaction` wraps a Transactor with conveniences (Step 5).

Why it matters: everything above `Transactor` is engine-agnostic by
construction; porting to a new engine means implementing one trait.

### Step 3 — declare intent at begin: read/write and the school

Two decisions are parameters of `begin`, not discoveries made mid-flight:

- `TransactionType` (tr.rs:15) is just `Read | Write`, declared UP FRONT.
  Compare postgres, where any transaction may write at any moment.
  Declaring intent enables engines to specialize: a single-writer engine
  can admit unlimited Read transactions concurrently and serialize only
  the Writes; read-only transactions can skip conflict tracking entirely
  (the same insight as SSI's safe snapshots).
- `LockType` on `Datastore::transaction()` (ds.rs:3353) is
  `Optimistic | Pessimistic` — the CHOICE of concurrency school (the two
  RocksDB flavors you just read) is a *per-transaction parameter*, passed
  down to engines that support both. The school is workload-dependent
  (contention flips the winner), so the interface refuses to hard-code it.

Cost of declaring: the application must know its intent — a "Read"
transaction that tries to write is an error, not an upgrade.

### Step 4 — the Transactor API: versioned reads and CAS as primitives

The portable contract itself (tr.rs) — read the signatures, they ARE the
checklist. Two design decisions stand out:

- **Versioned reads are public API.** `get`/`getm`/`getr`/`getp`
  (:119–155) all take `version: Option<u64>` — point-in-time reads are
  part of the KV contract, not an engine internal. Only some engines honor
  it: a capability, not a guarantee. The price of exposing it: GC can't
  drop what an API can still name (question 1).
- **Optimistic primitives are exposed, not hidden.** `set` :166 writes
  unconditionally; `put` :190 fails if the key exists; `putc` :202 is
  compare-and-set on the current value — write only if the value still
  equals what I read. With get + putc alone, an upper layer can build
  first-committer-wins snapshot isolation over ANY engine (question 3
  makes you sketch it) — OCC becomes portable because the *primitive* is
  in the interface even when the engine's own machinery isn't.

Plus the lifecycle pair: `commit` :103 / `cancel` :95 — commit is where
engine-level conflict errors surface, and the query layer retries. The
retry loop lives above the interface, matching SSI's lesson: serializable
semantics are a contract between engine AND application.

### Step 5 — caching under a snapshot: invalidation deleted by MVCC

`Transaction` (tx.rs:94, impl :693) wraps the Transactor with typed keys
and read-through caches. The thing to notice: caching *inside* a
transaction is trivially correct — the transaction reads a frozen
snapshot, so a value read once is valid for the transaction's whole life;
a within-txn cache never invalidates. Topic 6's hardest problem —
invalidation — deleted outright by MVCC's semantics. That is the kind of
simplification you get to collect when the layer below guarantees
snapshot reads.

## Where each step lives in the code

All under `surrealdb/core/src/kvs/`; ~1 h. Read the Transactor signatures
in tr.rs first — they ARE the interface checklist.

- **Step 2 — layering**: `Datastore::transaction()` — ds.rs:3353;
  `TransactionFactory` — ds.rs:314; builder plumbing — ds.rs:450–571;
  `Transaction` — tx.rs:94; `Transactor` — tr.rs:37.
- **Step 3 — intent**: `TransactionType` — tr.rs:15 (`Read | Write`);
  `LockType` (`Optimistic | Pessimistic`) on ds.rs:3353.
- **Step 4 — the contract**: versioned reads `get`/`getm`/`getr`/`getp` —
  tr.rs:119–155; `set` :166, `put` :190, `putc` :202; `commit` :103,
  `cancel` :95.
- **Step 5 — caching**: `Transaction` impl — tx.rs:693; note which
  methods read through the cache and that nothing ever invalidates it.

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
