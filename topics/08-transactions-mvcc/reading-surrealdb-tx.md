# The minimal transactional KV interface: surrealdb's kvs layer

surrealdb doesn't implement MVCC — it *abstracts over* engines that do, which
forces it to write down the minimal transactional interface a multi-model
database needs. This chapter builds that interface concept by concept: why
abstracting forces minimalism, how the layers stack, what gets declared up
front, which primitives make optimistic concurrency portable, and — the part
that matters most — what the interface deliberately does **not** promise. Read
this one for ARCHITECTURE, not algorithms: the interface is a good checklist for
M8's storage-backend abstraction (M1).

Line numbers below are `surrealdb/surrealdb@9d9a5b0`, checked with
`python3 tools/pinned-source.py`. Everything lives under
`surrealdb/core/src/kvs/`; that prefix is dropped in the prose below and
restored in the tables.

**Two corrections to the previous version of this guide, made up front because
the rest depends on them.** (1) `Transactor` (`tr.rs:37`) is a *struct*, not a
trait — it holds `inner: Box<dyn Transactable>`, and `Transactable`
(`api.rs:498`) is the actual trait each engine implements. (2) The engine list
at this pin is **five**, and FoundationDB is not among them: `kv-mem`,
`kv-indxdb`, `kv-rocksdb`, `kv-tikv`, `kv-surrealkv` (`core/Cargo.toml:19-35`,
enumerated as `DatastoreFlavor` at `ds.rs:556-567`).

## The problem in one sentence

One query engine must run transactions over five storage engines with wildly
different concurrency machinery — a plain in-memory map, single-node RocksDB and
SurrealKV, browser IndexedDB, distributed TiKV — so what is the *smallest* set
of operations the query layer can demand from all of them, and what happens to
the semantics that the smallest set cannot pin down?

## The concepts, step by step

### Step 1 — the vocabulary, and why abstraction forces minimalism

> **In:** the words this layer uses in its own doc comments. **Out:** a
> definition for each, plus the design pressure that produced the interface.

- **Transaction** — a group of reads and writes the database treats as one
  unit. Here it is an object with a lifetime: you get one from the datastore,
  call methods on it, and end it with exactly one of `commit` or `cancel`.
- **Snapshot** — the frozen view of the data a transaction reads from.
  surrealdb does not implement snapshots; it *requires* them from the engine and
  tests for them (`tests/snapshot.rs`, Step 6).
- **MVCC** — the engine-side mechanism that makes snapshots possible: writing a
  key leaves the old value in place and adds a new version. See
  [reading-postgres-heapam.md](reading-postgres-heapam.md).
- **Version** — one historical value of a key, addressed here by a `u64`
  timestamp. `version: Option<u64>` appears in every read signature.
- **`commit` / `cancel`** — the two terminal operations. `cancel` "reverses all
  changes made within the transaction" (`api.rs:519-522`).
- **CAS (compare-and-set)** — write a new value *only if* the current value
  still equals what I expected. Here it is `putc` (`tr.rs:202`).
- **First-committer-wins** — the conflict rule where, among concurrent writers
  of the same key, the first to commit succeeds and the rest are aborted.
- **Last-writer-wins** — the opposite: everybody commits, and the final value is
  whoever committed last. Step 6 shows both, from the same interface calls.
- **Optimistic / pessimistic** — the two concurrency schools from
  [reading-rocksdb-transactions.md](reading-rocksdb-transactions.md): validate
  at commit and abort, versus take locks up front and block.
- **Capability** — an interface method that some engines implement and others
  reject at runtime. The opposite of a guarantee.

Now the design pressure. When a database owns its storage engine, the
transaction interface can be as fat and idiosyncratic as it likes: one caller,
one implementor. When it must run over N third-party engines, every *required*
method must be implementable by all of them. Anything one engine cannot do has
to become either a capability (implementable as an error) or a derived method
(implementable in terms of the required ones).

Why it matters: that pressure is the reason to read this code. The interface
that survives it is close to the theoretical minimum for "transactional ordered
KV" — which is exactly the contract M8's storage-backend trait needs to name.

### Step 2 — the layering: what sits between a query and an engine

> **In:** a query that needs to read a row. **Out:** the four types it passes
> through, and which of them is engine-agnostic.

```
 Datastore (ds.rs:210)
   │  .transaction(TransactionType, LockType)          ds.rs:3353
   ▼
 TransactionFactory (ds.rs:302, impl :314)
   │  .transaction(write, lock, sequences)             ds.rs:348
   │  flattens both enums to bool                      ds.rs:355-363
   │  builder.new_transaction(write, lock)             ds.rs:365
   ▼
 Transaction (tx.rs:94, impl :693)      typed keys + a catalog cache
   │  .tr: Transactor                                  tx.rs:119
   ▼
 Transactor (tr.rs:37)                  thin typed wrapper, engine-agnostic
   │  .inner: Box<dyn Transactable>                    tr.rs:39
   ▼
 Transactable (api.rs:498)              THE interface — implemented per engine
   │
   ▼
 DatastoreFlavor::{Mem, RocksDB, IndxDB, TiKV, SurrealKV}   ds.rs:556-567
```

The dispatch is smaller than it looks. `TransactionFactory::transaction` reduces
both declared enums to a pair of booleans and hands them to the builder:

```rust
// surrealdb/core/src/kvs/ds.rs — TransactionFactory::transaction, 348-375 (elided)
   348      pub async fn transaction(
   349          &self,
   350          write: TransactionType,
   351          lock: LockType,
   352          sequences: Sequences,
   353      ) -> Result<Transaction> {
   354          // Specify if the transaction is writeable
   355          let write = match write {
   356              Read => false,
   357              Write => true,
   358          };
   359          // Specify if the transaction is lockable
   360          let lock = match lock {
   361              Pessimistic => true,
   362              Optimistic => false,
   363          };
   364          // Create a new transaction on the datastore
   365          let (inner, local) = self.builder.new_transaction(write, lock).await?;
   366          Ok(Transaction::new(
   ...
   371              Transactor {
   372                  inner,
   373              },
   ...
   375      }
```

Two booleans (:355-363) are the *entire* per-transaction configuration surface
that reaches an engine. Everything else the query layer wants is expressed as
method calls afterwards.

Note also `Transactor`'s `Drop` (`tr.rs:48-58`): dropping a writeable,
unfinished transaction logs `warn!` under `cfg(test)` (`:52-53`) and `error!`
otherwise (`:55-56`) — it does *not* silently roll back at this layer. (The
comment at `:54` says "Panic when running in normal mode"; the code does not
panic. Trust the code.) Ending a transaction is the caller's obligation, which
is a stricter contract than `experiments/src/mvcc.rs`'s ("Dropping a `Txn`
without commit = abort, no effects", `mvcc.rs:14`).

Why it matters: everything above `Transactable` is engine-agnostic by
construction. Porting to a new engine means implementing one trait — and Step 4
counts exactly how much work that is.

### Step 3 — declare intent at begin: read/write and the school

> **In:** `ds.transaction(Write, Optimistic)`. **Out:** why both parameters are
> declared up front rather than discovered mid-flight, and what each buys.

```rust
// surrealdb/core/src/kvs/tr.rs — the two begin-time enums, 13-34
    13  /// Specifies whether the transaction is read-only or writeable.
    14  #[derive(Copy, Clone, Eq, PartialEq)]
    15  pub enum TransactionType {
    16      Read,
    17      Write,
    18  }
    19
    20  /// Specifies whether the transaction is optimistic or pessimistic.
    21  #[derive(Copy, Clone)]
    22  pub enum LockType {
    23      Pessimistic,
    24      Optimistic,
    25  }
    26
    27  impl From<bool> for LockType {
    28      fn from(value: bool) -> Self {
    29          match value {
    30              true => LockType::Pessimistic,
    31              false => LockType::Optimistic,
    32          }
    33      }
    34  }
```

- **`TransactionType`** (`:15`) is declared UP FRONT. Compare postgres, where
  any transaction may write at any moment and the system discovers it (recall
  `MyXactDidWrite = true` being set on the first write in
  [reading-ssi-postgres.md](reading-ssi-postgres.md)). Declaring intent lets an
  engine specialise: a single-writer engine can admit unlimited `Read`
  transactions concurrently and serialize only the `Write`s, and a read-only
  transaction can skip conflict tracking entirely — the same insight as SSI's
  safe snapshots, but paid for by the caller instead of inferred.
- **`LockType`** (`:22`) makes the *choice of concurrency school* a
  per-transaction parameter — the two RocksDB flavours you just read, selected
  per call rather than per build. The interface refuses to hard-code it because
  the right answer is workload-dependent: contention flips the winner.

The cost of declaring is enforced, not advisory: `Transactor::writeable`
(`tr.rs:87`) is checked at the top of every mutating method, and a write on a
`Read` transaction is `Error::TransactionReadonly` (`api.rs:516`, raised at
e.g. `mem/mod.rs:354-356`). A `Read` transaction that tries to write is an
error, not an upgrade.

Why it matters: this is the cheapest optimisation in the whole layer. One enum
at begin buys the engine the right to make an entire class of transactions
free — and it costs the application only the discipline of knowing its own
intent.

### Step 4 — the `Transactable` trait: 19 required, 19 derived

> **In:** the trait an engine must implement. **Out:** the exact split between
> what every engine must supply and what the trait builds for it, with the
> counts.

The old version of this guide said the interface "fits in one trait: roughly
eight method signatures." The real shape is more interesting, and countable. At
`9d9a5b0`, `Transactable` (`api.rs:498-1082`) declares **38 methods: 19 with no
body (required) and 19 with a default body (derived)**.

The 19 an engine must write:

| group | methods | lines |
|---|---|---|
| introspection | `kind`, `closed`, `writeable` | `api.rs:500`, `:508`, `:517` |
| lifecycle | `cancel`, `commit` | `:522`, `:527` |
| point reads | `exists`, `get` | `:530`, `:533` |
| point writes | `set`, `put`, `putc`, `del`, `delc` | `:536`, `:539`, `:542`, `:545`, `:549` |
| range | `keys`, `keysr`, `scan`, `scanr` | `:558`, `:573`, `:588`, `:603` |
| savepoints | `new_save_point`, `release_last_save_point`, `rollback_to_save_point` | `:1010`, `:1013`, `:1016` |

Everything else is *derived* — written once, in the trait, in terms of those 19:

```rust
// surrealdb/core/src/kvs/api.rs — three derived methods, 668-676 + 726-735 (elided)
   666      /// Insert or replace a key in the datastore.
   668      fn replace(&self, key: Key, val: Val) -> BoxFut<'_, Result<()>> {
   669          Box::pin(async move { self.set(key, val).await })
   670      }
   671
   672      /// Delete all versions of a key from the datastore.
   674      fn clr(&self, key: Key) -> BoxFut<'_, Result<()>> {
   675          Box::pin(async move { self.del(key).await })
   676      }
   ...
   726      fn getp(&self, key: Key, version: Option<u64>) -> BoxFut<'_, Result<ScanResult>> {
   727          Box::pin(async move {
   ...
   733              let range = util::to_prefix_range(&key)?;
   734              self.getr(range, version).await
   735          })
   736      }
```

`getm` (`:693`) is a loop over `get`. `getr` (`:745`) is a loop over
`batch_keys_vals`. `open_keys_cursor` (`:624`) wraps `keys`/`keysr` and advances
`range.start` between batches, with the doc comment noting that "backends that
can keep a native iterator alive across batches override this method"
(`:636-638`). `delp`, `delr`, `clrp`, `clrr`, `count`, `batch_keys`,
`batch_keys_vals`, `timestamp`, `safe_timestamp`, `timestamp_impl`, `compact`
round out the 19.

Two of the derived methods are the cleanest statement of what a *capability*
means in this codebase:

- `compact` (`:1079-1081`): the default body is
  `bail!(Error::CompactionNotSupported)`. An engine that has a compaction
  primitive overrides it; the doc comment (`:1076-1078`) says "the call is
  advisory — callers must not rely on it for correctness."
- `safe_timestamp` (`:1054-1056`): the default returns `timestamp()`, and the
  doc comment (`:1047-1053`) says exactly which engines that is correct for
  ("mem, rocksdb, surrealkv") and which class "MUST override this … or the
  router can miss notifications."

Why it matters: 19 is the number to carry into M1. It says a transactional
backend trait needs two lifecycle methods, seven point operations, four range
operations, three savepoint operations and three introspection methods — and
that everything else you were tempted to put in the trait can be written once,
above it.

### Step 5 — the two interesting primitives: versioned reads and CAS

> **In:** the required point-operation signatures. **Out:** the two design
> decisions in them that are not obvious, and what each costs.

**Versioned reads are public API.** Every read in the trait — `exists`, `get`,
`keys`, `keysr`, `scan`, `scanr`, and the derived `getm`/`getp`/`getr` — takes
`version: Option<u64>`. Point-in-time reads are part of the KV contract, not an
engine internal. But only some engines honour it, and the *shape* of the refusal
differs by engine:

```rust
// surrealdb/core/src/kvs/mem/mod.rs — the capability gate, 51-58
    51  impl Transaction {
    52      fn ensure_versioned(&self, version: Option<u64>) -> Result<()> {
    53          if !self.versioned && version.is_some() {
    54              return Err(Error::UnsupportedVersionedQueries);
    55          }
    56          Ok(())
    57      }
    58  }
```

`mem` (`:52`), `rocksdb` (`rocksdb/mod.rs:165`) and `surrealkv`
(`surrealkv/mod.rs:58`) all carry that identical gate, keyed on a per-datastore
`versioned: bool` — they *can* time-travel if the datastore was opened for it.
TiKV cannot, at all, and says so inline in every read:

```rust
// surrealdb/core/src/kvs/tikv/mod.rs — Transactable::get, 711-724 (elided)
   711      fn get(&self, key: Key, version: Option<u64>) -> BoxFut<'_, Result<Option<Val>>> {
   712          Box::pin(async move {
   713              // TiKV does not support versioned queries.
   714              if version.is_some() {
   715                  return Err(Error::UnsupportedVersionedQueries);
   716              }
   ...
   724              let res = inner.tx.get(key).await?;
```

So `version: Option<u64>` is a *capability* with three tiers at this pin:
always-available (none), available-if-configured (mem, rocksdb, surrealkv),
never (tikv). `Error::UnsupportedVersionedQueries` is declared once, at
`err.rs:69`, and is the whole vocabulary for the refusal. The price of exposing
it at all: garbage collection cannot drop what an API can still name — question 1.

**Optimistic primitives are exposed, not hidden.** Three writes, ordered by how
much they assume:

- `set` (`tr.rs:166`) — "insert or update", unconditional.
- `put` (`tr.rs:190`) — "insert a key if it doesn't exist".
- `putc` (`tr.rs:202`) — "update a key … if the current value matches a
  condition". Compare-and-set, with the expected value passed as
  `chk: Option<V>`.

`putc`'s semantics are three arms, and the `None`/`None` case is the one people
miss:

```rust
// surrealdb/core/src/kvs/mem/mod.rs — Transactable::putc, 347-367 (elided)
   347      fn putc(&self, key: Key, val: Val, chk: Option<Val>) -> BoxFut<'_, Result<()>> {
   348          Box::pin(async move {
   ...
   359              // Set the key if valid
   360              match (inner.get(&key)?, chk) {
   361                  (Some(v), Some(w)) if v == w => inner.set(key, val)?,
   362                  (None, None) => inner.set(key, val)?,
   363                  _ => return Err(Error::TransactionConditionNotMet),
   364              };
   ...
   367      }
```

Line 361 is the ordinary CAS. Line 362 is "I expected this key to be absent, and
it is" — `putc(k, v, None)` is `put` with the absence made explicit. Everything
else is `Error::TransactionConditionNotMet` (:363).

With `get` + `putc` alone, a layer above can build first-committer-wins over an
engine that does not implement it:

```rust
// ILLUSTRATION — not quoted from surrealdb. The real primitives are
// Transactor::get (tr.rs:119) and Transactor::putc (tr.rs:202); the
// three-arm CAS semantics are at mem/mod.rs:360-364.
async fn compare_and_swap(tx: &Transactor, key: &[u8], f: impl Fn(Option<Val>) -> Val)
    -> Result<()>
{
    let before = tx.get(key, None).await?;      // the value my decision is based on
    let after  = f(before.clone());             // my new value
    tx.putc(key, after, before).await           // Err(TransactionConditionNotMet)
}                                               // if anyone changed it meanwhile
```

That is the whole trick: the *primitive* is in the interface even when the
engine's own conflict machinery isn't, so the retry loop can live above the
abstraction. Question 3 makes you extend it to a multi-key write set.

Why it matters: `putc` is the reason this interface can offer uniform
concurrency semantics without demanding uniform engines. It is also, per Step 6,
not enough on its own.

### Step 6 — the same calls, opposite outcomes: what the interface does not promise

> **In:** three concurrent writers of one key, issuing identical calls.
> **Out:** two different, both-correct final states, and the cargo feature that
> decides which one you get.

This is the sharpest thing in the `kvs` directory, and it is in the test suite
rather than the source. Two test files contain the *same* transaction script and
opposite assertions. Here is the script, with the assertions from both:

```
  ds.transaction(Write, Optimistic); set("test", "some text");    commit -> ok
  tx1 = transaction(Write, Optimistic);  tx1.set("test", "other text 1")
  tx2 = transaction(Write, Optimistic);  tx2.set("test", "other text 2")
  tx3 = transaction(Write, Optimistic);  tx3.set("test", "other text 3")
  tx1.commit()   tx2.commit()   tx3.commit()

  multiwriter_same_keys_conflict.rs         multiwriter_same_keys_allow.rs
  #![cfg(any(kv-mem, kv-rocksdb,            #![cfg(kv-tikv)]                  :1
             kv-surrealkv))]         :1
    tx1.commit().unwrap()           :27       tx1.commit().unwrap()           :27
    tx2.commit().unwrap_err()       :28       tx2.commit().unwrap()           :28
    tx3.commit().unwrap_err()       :29       tx3.commit().unwrap()           :29
    read back -> b"other text 1"    :33       read back -> b"other text 3"    :33

  = FIRST-COMMITTER-WINS                    = LAST-WRITER-WINS
```

Identical API calls; three commits succeed on one engine and one succeeds on the
others; the surviving value differs. Nothing in `Transactable` forbids either.
`commit` is declared as `fn commit(&self) -> BoxFut<'_, Result<()>>`
(`api.rs:527`) with the doc comment "This attempts to commit all changes made
within the transaction" — *attempts* is the entire specification of its failure
behaviour.

What the interface *does* pin down is snapshot reads, and there is a test for
that too — `tests/snapshot.rs`, which runs unconditionally rather than under a
`cfg`:

```
  set("test", "some text"); commit                                    :12-14
  tx1 = transaction(Read, ...);  tx1.get("test") == b"some text"      :16-19
  txw = transaction(Write, ...); txw.set("test", "other text")        :21-23
  tx2 = transaction(Read, ...);  tx2.get("test") == b"some text"      :25-27
  tx3 = transaction(Read, ...);  tx3.get("test") == b"some text"      :29-31
  txw.set("test", "extra text")                                       :33
  tx1.get("test") == b"some text"       <- STILL, after two writes    :35-36
  txw.commit()                                                        :42
```

Line 35-36 is the guarantee: a reader's answer does not move under it, no matter
how many times a concurrent writer writes. `tx2` and `tx3`, begun *after* `txw`
wrote but *before* it committed, also see the old value (:25-31) — uncommitted
writes are invisible. That is snapshot isolation, stated as an executable
assertion at the portable layer.

So the honest summary of the contract is a three-way split:

| property | status in `Transactable` | evidence |
|---|---|---|
| snapshot reads; uncommitted writes invisible | **guaranteed** — tested for every engine | `tests/snapshot.rs` (no `cfg` gate) |
| point-in-time reads by version | **capability** — refused by name | `err.rs:69`, `tikv/mod.rs:713-716` |
| write-write conflict behaviour at commit | **unspecified** — engine-defined | the two test files above |

Why it matters: this is the real cost of abstracting over engines, and it is not
the one the old version of this guide named. The *operations* port cleanly. The
*isolation semantics* do not, and the layer's response is not to paper over the
difference but to write two tests and gate them by cargo feature. If M8 takes
this trait shape, it inherits this decision — and had better make it
deliberately.

### Step 7 — caching under a snapshot: what MVCC deletes, and what it doesn't

> **In:** a read-through cache living inside a transaction. **Out:** which
> class of invalidation snapshot reads eliminate, and the two classes they
> don't.

`Transaction` (`tx.rs:94`) wraps the `Transactor` with typed keys and a cache
(`tx.rs:121`). The cache is a *catalog* cache, not a row cache: its keys are
schema lookups — `Nss` (namespaces), `Dbs`, `Tbs`, `Ixs`, `Fds`, `Nds`
(cluster nodes) and about thirty more (`cache/tx/lookup.rs:11-70`). The pattern
is the ordinary read-through:

```rust
// surrealdb/core/src/kvs/tx.rs — NodeProvider::all_nodes, 2173-2192 (elided)
  2173      fn all_nodes(&self) -> BoxProviderFut<'_, Result<Arc<[Node]>>> {
  2174          Box::pin(
  2175              async move {
  2176                  let qey = cache::tx::Lookup::Nds;
  2177                  match self.cache.get(&qey) {
  2178                      Some(val) => val.try_into_nds(),
  2179                      None => {
  2180                          let beg = crate::key::root::nd::prefix();
  2181                          let end = crate::key::root::nd::suffix();
  2182                          let val = self.getr(beg..end, None).await?;
  ...
  2184                          let entry = cache::tx::Entry::Nds(Arc::clone(&val));
  2185                          self.cache.insert(qey, entry);
  2186                          Ok(val)
  2187                      }
  2188                  }
```

The thing worth noticing is what is *absent* from :2177-2187: any check that the
cached answer is still current. There is no version, no generation counter, no
subscription to an invalidation channel. That is what a snapshot buys —
concurrent writers cannot change the answer, so the cache can never go stale
*because of someone else*. Topic 6's hardest problem, cross-actor invalidation,
is deleted outright by the layer below.

**But "a within-txn cache never invalidates" — the previous version of this
guide's claim — is not true at this pin, in two ways.**

1. **Self-invalidation is real, and explicit.** A transaction that writes the
   catalog must invalidate its own cached view of it, because read-your-own-writes
   means the cached answer is stale *to itself*:

```rust
// surrealdb/core/src/kvs/tx.rs — after removing a namespace definition, 1399-1409 (elided)
  1399          self.set(
  1400              &rc,
  ...
  1405          .await?;
  1406          // Invalidate cached namespace lookups so the removal is observed.
  1407          self.cache.remove(&cache::tx::Lookup::Nss);
  1408          self.cache.remove(&cache::tx::Lookup::NsByName(&ns_def.name));
  1409          Ok(Some(ns_def.namespace_id))
```

   The same pattern appears for databases (`tx.rs:1450-1451`) and indexes
   (`:1503-1505`), and `clear_cache` (`:2143-2147`) drops the lot.

2. **It is a bounded cache, so entries can be evicted.** It is a `quick_cache`
   with an estimated capacity and a weight budget (`cache/tx/mod.rs:41-46`).
   Eviction is not invalidation, but it means a cache hit is never guaranteed.

The one thing the snapshot *does* buy the implementation is worth quoting,
because it is a design decision most caches cannot make: `shards(1)`
(`cache/tx/mod.rs:44`), justified at `:35-39` — "The cache is per-transaction
and not concurrently accessed across threads, so `shards = 1` is used. The
default `available_parallelism() * 4` would allocate hundreds of sharded
`CacheShard` structs per transaction on large boxes, all of which then get
dropped at commit or cancel."

Why it matters: the correct statement of the simplification is narrower and more
useful than the old one. Snapshot reads delete **cross-transaction**
invalidation. They do nothing about **self**-invalidation, which you still have
to get right by hand — and the comment at `tx.rs:1406` is what that looks like
when you do.

## Where each step lives in the code

All paths are relative to `surrealdb/core/src/`; ~1 h. Read the 19 required
`Transactable` signatures in `api.rs` first — they ARE the interface checklist.

| Step | What | File | Lines |
|---|---|---|---|
| 2 | `Datastore` and its `transaction()` entry point | `kvs/ds.rs` | 210, 3353 |
| 2 | `TransactionFactory` struct / impl / flattening | `kvs/ds.rs` | 302, 314, 348-375 |
| 2 | the five engine flavours | `kvs/ds.rs` | 556-567 |
| 2 | `Transaction` struct / impl | `kvs/tx.rs` | 94, 693 |
| 2 | `Transactor` and its `Drop` warning | `kvs/tr.rs` | 37-40, 48-58 |
| 3 | `TransactionType` and `LockType` | `kvs/tr.rs` | 13-34 |
| 3 | `writeable()`, and the readonly error | `kvs/tr.rs`, `kvs/api.rs` | 87, 510-517 |
| 4 | the `Transactable` trait, all 38 methods | `kvs/api.rs` | 498-1082 |
| 4 | the 19 required ones | `kvs/api.rs` | 500-603, 1010-1016 |
| 4 | `compact` — a capability with a default refusal | `kvs/api.rs` | 1071-1081 |
| 5 | versioned reads, in every read signature | `kvs/api.rs` | 530, 533, 558-609 |
| 5 | the capability gate, three engines | `kvs/mem/mod.rs`, `kvs/rocksdb/mod.rs`, `kvs/surrealkv/mod.rs` | 52, 165, 58 |
| 5 | TiKV's flat refusal | `kvs/tikv/mod.rs` | 713-716 |
| 5 | `set` / `put` / `putc` / `del` / `delc` | `kvs/tr.rs` | 166, 190, 202, 215, 226 |
| 5 | `putc`'s three arms | `kvs/mem/mod.rs` | 360-364 |
| 6 | first-committer-wins | `kvs/tests/multiwriter_same_keys_conflict.rs` | 1, 27-33 |
| 6 | last-writer-wins | `kvs/tests/multiwriter_same_keys_allow.rs` | 1, 27-33 |
| 6 | snapshot reads, ungated | `kvs/tests/snapshot.rs` | 16-42 |
| 7 | the transaction cache | `kvs/tx.rs`, `kvs/cache/tx/mod.rs` | 121, 27-56 |
| 7 | read-through, no staleness check | `kvs/tx.rs` | 2173-2192 |
| 7 | self-invalidation | `kvs/tx.rs` | 1407-1408, 1450-1451, 1503-1505, 2146 |

## How to read the code

1. **`api.rs:498-1082`** — the trait, top to bottom. Mark each method as
   required or derived as you go; you should reach 19 and 19.
2. **`tr.rs:1-230`** — the typed wrapper. It is almost entirely
   `key.into_vec()` then a delegation, which is the point: `Transactor` adds
   types, not behaviour.
3. **`mem/mod.rs`** — the smallest engine implementation. Read `commit`
   (`:183`), `putc` (`:347`) and `ensure_versioned` (`:52`); that is enough to
   see the whole shape of an implementation.
4. **`kvs/tests/`** — read `snapshot.rs`, then the two `multiwriter_same_keys_*`
   files back to back. This is Step 6 and it is the fastest 5 minutes in the
   directory.
5. **`ds.rs:302-376`** — the dispatch, if you want to see how the two begin-time
   enums reach an engine.
6. **`tx.rs`** — skim. It is 4 973 lines of typed catalog accessors; read
   `all_nodes` (`:2173`) for the cache pattern and one `remove` site
   (`:1406-1408`) for the invalidation, and move on.

## Questions for notes.md

1. `version: Option<u64>` on every read makes time travel part of the public
   API. What does that cost the engines that support it? (Think about what
   garbage collection is allowed to drop once an API can still name an old
   version — and compare postgres's answer in
   [reading-postgres-heapam.md](reading-postgres-heapam.md), where the bound is
   the oldest *snapshot*, not the oldest nameable timestamp.)
2. Read/Write declared at begin: what optimisations does that unlock for a
   single-writer engine, and what does FalkorDB's `GRAPH.RO_QUERY` vs
   `GRAPH.QUERY` split already encode?
3. `putc` as the portable OCC primitive: sketch first-committer-wins over ONLY
   `get`/`putc`, for a transaction with a write set of *n* keys. Where does your
   sketch break, and what does that tell you about why real engines do this
   below the interface rather than above it?
4. Step 6 showed the same script producing first-committer-wins on three engines
   and last-writer-wins on a fourth. If you were writing the query layer above
   this trait, which of the two would you have to code against — and what would
   you have to add to the trait to stop having to guess?
5. M1 retrospective: does your storage-backend trait from topic 1 admit a
   transactional backend, or did you bake in auto-commit? Compare its method
   count against this trait's 19 required. What would you change now?

## Takeaway

The minimum viable transactional KV interface is 19 methods: two lifecycle,
seven point, four range, three savepoint, three introspection. Everything else
— prefix scans, multi-gets, cursors, range deletes, compaction hints — is
derivable and belongs in the trait's default bodies, not in each engine. But the
interface's real lesson is negative: a portable set of *operations* does not
give you portable *semantics*. surrealdb pins down snapshot reads and tests them
for every engine; it exposes point-in-time reads as a named capability that
engines may refuse; and it leaves write-write conflict behaviour completely
unspecified, to the point of shipping two contradictory test files gated by
cargo feature. If you build an abstraction like this, the operations are the
easy part. Deciding, and writing down, which guarantees survive the abstraction
is the work.

## Connections to this topic's experiment

`experiments/src/mvcc.rs` is the layer *below* this one — the thing an engine
would have to implement to sit under `Transactable`. Mapping the two makes both
clearer:

| `Transactable` (`api.rs`) | `mvcc.rs` | note |
|---|---|---|
| `commit` (`:527`) | `Txn::commit` (`mvcc.rs:105`) | yours returns a typed `CommitError`; the trait returns an opaque `Result` |
| `cancel` (`:522`) | dropping a `Txn` (`mvcc.rs:14`) | surrealdb warns on an undropped write txn instead (`tr.rs:48-58`) |
| `get(key, version)` (`:533`) | `Txn::get` (`mvcc.rs:89`) | yours has no `version` parameter — it is the always-`None` tier of Step 5 |
| `set` / `del` (`:536`, `:545`) | `Txn::put` / `Txn::delete` (`mvcc.rs:94`, `:99`) | same shape |
| `putc` (`:542`) | — | you have no CAS; Step 5's question 3 is about adding one |
| — | `Mvcc::gc` (`mvcc.rs:70`) | garbage collection is below the interface, which is exactly why versioned reads complicate it |

Your `Mode::Snapshot` commit rule — first-committer-wins
(`mvcc.rs:7-9`) — is the `multiwriter_same_keys_conflict.rs` column of Step 6's
table, and `first_committer_wins_on_write_write_conflict` is the same assertion
as that file's `tx2.commit().unwrap_err()` at `:28`. Your `Mode::Serializable`
has no counterpart anywhere in `kvs`: nothing in `Transactable` validates a read
set, and nothing in it could, since the trait never sees which keys you read.

**What this repo has measured, and what it has not.** The provided lane
(`FINDINGS.md` row 8, and the baseline table in [notes.md](notes.md)) is a
single global `Mutex<HashMap>`, 4 threads × 50 000 transactions × 4 operations,
on an Apple M3 Pro:

| mix | global-lock txn/s | mvcc txn/s | aborts |
|---|---|---|---|
| read-heavy 95/5, 10K keys | 623 454 | stub | stub |
| write-heavy 50/50, 10K keys | 594 264 | stub | stub |
| write-heavy 50/50, 64 keys (HOT) | 676 691 | stub | stub |

**The headline is the flatness, and it is a negative result.** ~600k txn/s on
all three mixes: the mutex does not care whether the workload is 95% reads or
50% writes, or whether it collides on 10 000 keys or 64, because it had already
serialized everything. The 64-key row is even the *fastest* — a cache-resident
working set, with no contention penalty to pay because there was only ever one
lock to contend on.

So: **this repo has not measured MVCC beating a mutex, and has measured nothing
at all about surrealdb.** The `mvcc txn/s` and `aborts` columns are `stub`
because that code is yours to write. Nothing in this guide is a repo
measurement — this chapter contains no timings, only counted and quoted source.
When you fill in those columns, the prediction worth writing down first is in
[notes.md](notes.md): MVCC should crush the baseline on row 1, where readers
never block, and may well *lose* on row 3, where first-committer-wins converts
key contention into aborted work the mutex never had to redo.

## Done when

Answer each before unfolding it.

- [ ] List the operations a transactional KV interface needs, grouped, and give
      the count of required versus derived methods in `Transactable`.
  <details><summary>Answer</summary>

  **19 required** (`api.rs`): introspection `kind`/`closed`/`writeable`
  (`:500`, `:508`, `:517`); lifecycle `cancel`/`commit` (`:522`, `:527`); point
  reads `exists`/`get` (`:530`, `:533`); point writes
  `set`/`put`/`putc`/`del`/`delc` (`:536`–`:549`); range
  `keys`/`keysr`/`scan`/`scanr` (`:558`–`:603`); savepoints
  `new_save_point`/`release_last_save_point`/`rollback_to_save_point`
  (`:1010`–`:1016`). **19 derived**, with default bodies written in terms of
  those: `replace`, `clr`, `clrc`, `getm`, `getp`, `getr`, `delp`, `delr`,
  `clrp`, `clrr`, `count`, `batch_keys`, `batch_keys_vals`, `open_keys_cursor`,
  `open_vals_cursor`, `timestamp`, `safe_timestamp`, `timestamp_impl`,
  `compact`. Note that `Transactor` (`tr.rs:37`) is a struct wrapping
  `Box<dyn Transactable>`, not the trait itself.
  </details>

- [ ] Name one guarantee, one capability and one unspecified behaviour in this
      interface, and the evidence for each.
  <details><summary>Answer</summary>

  **Guarantee — snapshot reads.** `tests/snapshot.rs` has no `cfg` gate, so
  every engine must pass it: a reader's answer does not change under concurrent
  writes (`:35-36`), and uncommitted writes are invisible to readers begun after
  them (`:25-31`). **Capability — point-in-time reads.** `version: Option<u64>`
  is in every read signature, but mem/rocksdb/surrealkv gate it on a
  per-datastore flag (`mem/mod.rs:52-55` and its two twins) and TiKV refuses it
  outright (`tikv/mod.rs:713-716`), both via `Error::UnsupportedVersionedQueries`
  (`err.rs:69`). **Unspecified — write-write conflict behaviour.** `commit`'s
  doc comment says only that it "attempts to commit" (`api.rs:524-527`), and the
  test suite ships both outcomes: `multiwriter_same_keys_conflict.rs` (mem,
  rocksdb, surrealkv) asserts `unwrap_err()` for the second and third committer,
  `multiwriter_same_keys_allow.rs` (tikv) asserts `unwrap()` for all three.
  </details>

- [ ] Three transactions each `set` the same key, then commit in order. Give
      both possible final values, and say which engines produce which.
  <details><summary>Answer</summary>

  `b"other text 1"` under first-committer-wins — `kv-mem`, `kv-rocksdb`,
  `kv-surrealkv`, per `multiwriter_same_keys_conflict.rs:1` and its assertions
  at `:27-29` (`unwrap`, `unwrap_err`, `unwrap_err`) and `:33`. Or
  `b"other text 3"` under last-writer-wins — `kv-tikv`, per
  `multiwriter_same_keys_allow.rs:1`, `:27-29` (three `unwrap`s) and `:33`. The
  API calls are identical in both files; only the cargo feature and the
  assertions differ. That is the interface declining to specify the semantics.
  </details>

- [ ] What does `putc(key, val, None)` mean, and how does it differ from
      `putc(key, val, Some(old))` and from `put`?
  <details><summary>Answer</summary>

  `mem/mod.rs:360-364` gives all three arms. `putc(k, v, Some(w))` succeeds only
  if the current value is exactly `w` (`:361`) — ordinary compare-and-set.
  `putc(k, v, None)` succeeds only if the key is currently **absent** (`:362`) —
  the same effect as `put` (`api.rs:539`, "insert a key if it doesn't exist"),
  but with the expectation written down rather than implied. Anything else is
  `Error::TransactionConditionNotMet` (`:363`). The reason `putc` matters is
  that `get` + `putc` is enough to build first-committer-wins *above* the
  interface, on an engine that does not provide it below.
  </details>

- [ ] Snapshot reads delete one class of cache invalidation. Which — and which
      classes remain?
  <details><summary>Answer</summary>

  They delete **cross-transaction** invalidation: a concurrent writer cannot
  change the answer to a read you already made, so `all_nodes`
  (`tx.rs:2173-2192`) can cache with no version, no generation counter and no
  subscription. What remains: (1) **self**-invalidation, because
  read-your-own-writes makes your own catalog writes stale your own cache —
  hence the explicit `self.cache.remove(...)` calls at `tx.rs:1407-1408`,
  `:1450-1451`, `:1503-1505` with the comment "Invalidate cached namespace
  lookups so the removal is observed"; and (2) **eviction**, since the cache is
  a bounded `quick_cache` with a weight budget (`cache/tx/mod.rs:41-46`). "A
  within-transaction cache never invalidates" is therefore too strong.
  </details>

- [ ] What has this repo measured about surrealdb, and what does the topic's
      measured lane actually report?
  <details><summary>Answer</summary>

  **Nothing.** This chapter contains no timing at all — only counted and quoted
  source at `surrealdb/surrealdb@9d9a5b0`. The topic's measured lane
  ([`FINDINGS.md`](../../FINDINGS.md) row 8, [notes.md](notes.md)) is a global
  `Mutex<HashMap>` at **623 454 / 594 264 / 676 691 txn/s** across read-heavy,
  write-heavy and hot-key mixes — flat, because a single mutex had already
  serialized everything, and *fastest* on the 64-key row because that working
  set is cache-resident. The `mvcc txn/s` and `aborts` columns are `stub`: this
  repo has **not** measured MVCC beating a mutex.
  </details>

## References

**Code** (`surrealdb/surrealdb@9d9a5b0`, under `surrealdb/core/src/`)

| File | Lines | What |
|---|---|---|
| `kvs/api.rs` | 498-1082 | `Transactable` — the interface: 19 required, 19 derived |
| `kvs/api.rs` | 1071-1081 | `compact` — a capability whose default body refuses |
| `kvs/tr.rs` | 13-34 | `TransactionType` and `LockType` |
| `kvs/tr.rs` | 37-58 | `Transactor` (a struct, not a trait) and its `Drop` warning |
| `kvs/tr.rs` | 95-230 | the typed wrapper: `cancel`, `commit`, reads, `set`/`put`/`putc`/`del`/`delc` |
| `kvs/ds.rs` | 302-376 | `TransactionFactory` — two enums flattened to two bools |
| `kvs/ds.rs` | 556-567 | `DatastoreFlavor` — the five engines |
| `kvs/tx.rs` | 94-141, 693 | `Transaction` — typed keys and the catalog cache |
| `kvs/tx.rs` | 2173-2192 | read-through caching with no staleness check |
| `kvs/tx.rs` | 1406-1408 | self-invalidation, with the comment that explains it |
| `kvs/cache/tx/mod.rs` | 27-56 | the per-transaction cache, `shards(1)` and why |
| `kvs/mem/mod.rs` | 52-57, 347-367 | the versioned-read gate and `putc`'s three arms |
| `kvs/tikv/mod.rs` | 711-716 | an engine refusing a capability inline |
| `kvs/err.rs` | 69, 80 | `UnsupportedVersionedQueries`, `CompactionNotSupported` |
| `kvs/tests/snapshot.rs` | 16-42 | snapshot isolation as an ungated assertion |
| `kvs/tests/multiwriter_same_keys_conflict.rs` | 1, 27-33 | first-committer-wins (mem, rocksdb, surrealkv) |
| `kvs/tests/multiwriter_same_keys_allow.rs` | 1, 27-33 | last-writer-wins (tikv) |
| `core/Cargo.toml` | 19-35 | the five `kv-*` features |

**In this repo**

| File | Lines | What |
|---|---|---|
| `experiments/src/mvcc.rs` | 6-15 | the commit contract your engine has to satisfy |
| `experiments/src/mvcc.rs` | 60-107 | the operations, mapped against `Transactable` above |
| [`notes.md`](notes.md) | baseline table | the measured global-mutex lane; `mvcc` columns still `stub` |
| [`FINDINGS.md`](../../FINDINGS.md) | row 8 | the flat ~600k txn/s headline |

**Related chapters**
- [reading-rocksdb-transactions.md](reading-rocksdb-transactions.md) — the two
  schools `LockType` selects between, one level down.
- [reading-ssi-postgres.md](reading-ssi-postgres.md) — what it takes to add read-set
  tracking, which this interface has no hook for.
- [reading-postgres-heapam.md](reading-postgres-heapam.md) — the version chains
  `version: Option<u64>` reads from, and why GC is the price.
