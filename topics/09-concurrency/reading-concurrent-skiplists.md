# Two concurrent skiplists: CAS vs lazy locking

Same structure, two schools of coordination: RocksDB's memtable skiplist
links nodes with per-level CAS and never deletes; memgraph's skiplist — the
spine of its whole graph store — uses per-node spinlocks, state bits, and
real deletion with GC. Before you open either file, this chapter builds
both designs one concept at a time — the skiplist shape *as these two
implementations actually parameterise it*, the CAS toolkit, each school's
insert protocol, and the deletion problem only one of them has to solve —
then hands you the line anchors to watch each piece in production code.
Read RocksDB first (you know this file from topic 2 — now the concurrency),
then memgraph as the contrast.

Everything below is read at the pinned commits **`facebook/rocksdb@7c80a5a`**
(`memtable/inlineskiplist.h`, 1422 lines) and
**`memgraph/memgraph@8f87f6a`** (`src/utils/skip_list.hpp`, 1854 lines).
Confirm with `python3 tools/pinned-source.py ref rocksdb`.

## The problem in one sentence

Keep one sorted in-memory structure correct while 32 writer threads insert
into it and readers traverse it at full speed — a single mutex around it
caps a 32-core machine at *less* than the throughput of one core (this
topic's `scaling` lane measures a global `Mutex<BTreeSet>` going from 8.65
Mops/s at one thread to **2.96 Mops/s at sixteen**, 2.9× *backwards*), so
both designs coordinate at the granularity of individual pointers instead.

## The concepts, step by step

### Step 1 — the skiplist, with the constants these two actually chose

> **In:** the need for a sorted structure whose inserts never move existing
> data.
> **Out:** the shape, and the two very different probability constants
> RocksDB and memgraph picked — plus why both are right.

A **skiplist** is a sorted linked list where each node also gets a random
number of stacked "express lane" links — a **tower**. A node's tower height
is drawn from a geometric distribution at creation, so the higher lanes hold
exponentially fewer nodes:

```
 level 3:  head ─────────────────────► 50 ──────────────────────► nil
 level 2:  head ─────────► 20 ───────► 50 ─────────► 80 ────────► nil
 level 1:  head ──► 10 ──► 20 ──► 30 ─► 50 ──► 60 ──► 80 ──► 90 ─► nil
           level 1 = every node. Search: start top-left, go right until
           you'd overshoot, drop a level, repeat.
```

Generic write-ups say "height ≥ h with probability 2⁻ʰ". **Neither of these
implementations uses that.** Read the real constants:

```cpp
// memtable/inlineskiplist.h:70-78 — RocksDB's defaults
    70    static const uint16_t kMaxPossibleHeight = 32;
    76    explicit InlineSkipList(Comparator cmp, Allocator* allocator,
    77                            int32_t max_height = 12,
    78                            int32_t branching_factor = 4);
```

```cpp
// memtable/inlineskiplist.h:559-573 — the coin, once per level
   559  int InlineSkipList<Comparator>::RandomHeight() {
   560    auto rnd = Random::GetTLSInstance();
   562    // Increase height with probability 1 in kBranching
   563    int height = 1;
   564    while (height < kMaxHeight_ && height < kMaxPossibleHeight &&
   565           rnd->Next() < kScaledInverseBranching_) {
   566      height++;
   567    }
```

`branching_factor = 4` means **p = 1/4**, not 1/2, and `max_height = 12`
caps the tower at 12 (not the 32 `kMaxPossibleHeight` allows). memgraph
went the other way:

```cpp
// src/utils/skip_list.hpp:106-116 — one RNG draw, ffs gives a geometric height
   106    static uint8_t gen_height() {
   110      uint32_t value = thread_local_mt19937()();
   111      if (value < 1UL << (32 - kSkipListMaxHeight)) return kSkipListMaxHeight;
   112      // The value should have exactly `kSkipListMaxHeight` bits.
   113      value >>= (32 - kSkipListMaxHeight);
   114      // ffs = find first set
   116      return static_cast<uint8_t>(__builtin_ffs(value));
   117    }
```

`__builtin_ffs` (find-first-set) of a uniform 32-bit value returns 1 with
probability ½, 2 with probability ¼, and so on — **p = 1/2**, with
`kSkipListMaxHeight = 32` (`:61`). One RNG call per node instead of a loop,
which is the trick the linked blog comment at `:101` is about.

**Work the search cost on both and the answer is surprising.** The expected
number of hops for a skiplist with parameter p over n keys is
`(1/p) · log_{1/p}(n)`. At the 50 000-key preload of this topic's
`scaling` lane:

```
  RocksDB, p = 1/4:   log₄(50 000)  = 7.80 levels ×  4 hops/level = 31.2 hops
  memgraph, p = 1/2:  log₂(50 000)  = 15.61 levels × 2 hops/level = 31.2 hops
```

**Identical.** The expected hop count is `(1/p)·ln n / ln(1/p)`, and for
p = 1/2 and p = 1/4 the factor `(1/p)/ln(1/p)` is 2/0.693 = 2.885 and
4/1.386 = 2.885 — the function has its minimum around p = 1/e and is flat
between these two. So why did RocksDB pick 1/4? **Memory.** Expected tower
height is `1/(1−p)`:

```
  p = 1/2:  E[height] = 2.00 pointers per node
  p = 1/4:  E[height] = 1.33 pointers per node   → 33% fewer pointers
```

A memtable is size-capped and flushed when full, so a third fewer pointers
is a third more *keys* per memtable, which is directly fewer SST files.
memgraph's lists hold vertices and edges that live for the lifetime of the
database and are traversed constantly, so it buys the shallower, wider
search instead. Same structure, opposite pressure, and the constant is where
the difference is written down.

The reason both engines picked a skiplist over a B-tree (topic 1) for
*concurrency* is separate: a B-tree keeps sorted order by shifting rows
inside pages and splitting full pages — bulk moves of existing data. A
skiplist keeps order purely with pointers, so an insert never moves anything
that already exists: it is one pointer swing per level, and single pointer
swings are exactly what atomic hardware instructions can do. The cost is
those ~31 *dependent* pointer hops per search — up to 31 serialised cache
misses (topic 0 §2) — versus a B-tree's few cache-friendly binary searches.
Step 5 shows RocksDB clawing that back.

### Step 2 — CAS, memory ordering, and the publication idiom

> **In:** a node built in private memory that must become visible to
> readers running right now.
> **Out:** the three orderings both files use, the one-word limit that
> splits the two schools, and where RocksDB's orderings actually live.

**CAS** (compare-and-swap) is the atomic CPU instruction "replace this one
64-bit word with a new value only if it still equals the value I read" — if
another thread changed it in between, the CAS fails and you retry.

**Memory ordering** decides what *else* a thread sees when it sees your
write. **Relaxed** guarantees only that the write is atomic — nothing about
the order of your other writes. **Release** on a store guarantees that
everything you wrote before it is visible to any thread that observes the
store. **Acquire** on a load is the other half: observe a Release-store and
you observe everything that preceded it. `SeqCst` adds a single total order
all threads agree on, and costs more.

Together Release/Acquire form the **publication idiom**: build your object
privately with plain or Relaxed writes, then *publish* it with one Release
store; readers Acquire-load and are guaranteed a fully-built object.
RocksDB says exactly this in the comments on its accessors:

```cpp
// memtable/inlineskiplist.h:379-396 — where the barriers live, and why
   379    Node* Next(int n) {
   381      // Use an 'acquire load' so that we observe a fully initialized
   382      // version of the returned Node.
   383      return ((&next_[0] - n)->Load());
   384    }
   386    void SetNext(int n, Node* x) {
   388      // Use a 'release store' so that anybody who reads through this
   389      // pointer observes a fully initialized version of the inserted node.
   390      (&next_[0] - n)->Store(x);
   391    }
   393    bool CASNext(int n, Node* expected, Node* x) {
   395      return (&next_[0] - n)->CasStrong(expected, x);
   396    }
```

Two things worth stopping on. First, `Load`/`Store`/`CasStrong` are
RocksDB's own wrappers in `util/atomic.h` — `Load` is acquire (`:111-113`),
`Store` is release (`:108-110`), and `CasStrong` is **acq_rel**
(`:118-121`), not the release/relaxed pair you might guess. Second, look at
the addressing: `&next_[0] - n`. **The tower grows downward in memory.**
The comment at `:352-356` explains: the key is stored in the bytes
immediately after the struct and the higher `next_` pointers immediately
*before* it, so a node is one allocation with no separate tower array and
no stored height. `NoBarrier_SetNext` (`:399`, wrapping `StoreRelaxed` at
`util/atomic.h:60`) is the Relaxed variant used for the not-yet-published
half of the idiom.

The catch that splits the two schools: **CAS swings ONE word, but a height-4
tower is four links.** A multi-pointer insert cannot be atomic, so each
school must decide what readers are allowed to see in between.

### Step 3 — the CAS school: link one level at a time (RocksDB)

> **In:** a new node and a `Splice` of (pred, succ) per level.
> **Out:** RocksDB's actual insert loop, what a lost race costs, and why
> partial towers are harmless here.

RocksDB's answer: don't make the tower atomic. Link it bottom-up, one CAS
per level, and let readers see partial towers. This is the real loop, not a
paraphrase:

```cpp
// memtable/inlineskiplist.h:1134-1172 — Insert<UseCAS=true>, asserts elided
  1134    if (UseCAS) {
  1135      for (int i = 0; i < height; ++i) {
  1136        while (true) {
  1137          // Checking for duplicate keys on the level 0 is sufficient
  1138          if (UNLIKELY(i == 0 && splice->next_[i] != nullptr &&
  1139                       compare_(splice->next_[i]->Key(), key_decoded) <= 0)) {
  1141            return false;
  1142          }
  1143          if (UNLIKELY(i == 0 && splice->prev_[i] != head_ &&
  1144                       compare_(splice->prev_[i]->Key(), key_decoded) >= 0)) {
  1146            return false;
  1147          }
  1152          x->NoBarrier_SetNext(i, splice->next_[i]);
  1153          if (splice->prev_[i]->CASNext(i, splice->next_[i], x)) {
  1155            break;
  1156          }
  1157          // CAS failed, we need to recompute prev and next. ...
  1162          FindSpliceForLevel<false>(key_decoded, splice->prev_[i], nullptr, i,
  1163                                    &splice->prev_[i], &splice->next_[i]);
  1168          if (i > 0) {
  1169            splice_is_valid = false;
  1170          }
  1171        }
  1172      }
```

Read four things off it:

- **`NoBarrier_SetNext` then `CASNext`** (`:1152-1153`) is the publication
  idiom, verbatim: relaxed write into the unpublished node, then one
  acq_rel CAS that makes it reachable.
- **A lost race re-searches ONE level, from where it already was**
  (`:1162-1163`): `FindSpliceForLevel` starts at `splice->prev_[i]`, not at
  the head. The comment at `:1157-1161` gives the reasoning — it is unlikely
  that many nodes landed between prev and next, so scanning forward from the
  old prev beats restarting. No thread ever waits for another; there is no
  full restart.
- **Duplicate detection happens only at level 0** (`:1138-1147`, and the
  comment says so). That is the linearization point: whoever wins the level-0
  CAS owns the key.
- **A failed CAS above level 0 invalidates the splice** (`:1168-1170`),
  because narrowing the bracket at level i may break the `Splice` invariant
  stated at `:341-346` — `prev_[i+1].key <= prev_[i].key < next_[i].key <=
  next_[i+1].key`.

Why bottom-first makes partial towers harmless *for a set*: level 0 (every
node) is the ground truth, and the node is findable the instant its bottom
link lands — upper levels are only shortcuts, so a reader that doesn't see
node 35 at level 2 yet still finds it at level 0:

```
 inserting 35, tower height 3:
 level 2:   20 ─────────────► 50        (not linked yet — readers skip 35 here)
 level 1:   20 ────► 35 ────► 50        (linked)
 level 0:   30 ────► 35 ────► 50        (linked FIRST — 35 is now findable)
```

**Now price the retry, because the folklore here is wrong.** "Lock-free
means you burn CPU on retries" is the standard worry. Evaluate it. Expected
CAS attempts under a per-attempt failure probability p is a geometric mean,
`E[attempts] = 1/(1−p)`. Estimate p for the `scaling` lane: 16 threads,
19.28 Mops/s, 10% writes ⇒ 1.93 M inserts/s spread over a 50 000-key
keyspace ⇒ about 39 inserts per second land at any one level-0 link point.
The CAS window — load succ, store, CAS — is maybe 5 ns. So
p ≈ 39 × 5e-9 ≈ **2 × 10⁻⁷**, and E[attempts] = 1.0000002: **one retry per
five million inserts**. Even a pathological workload where *every* insert
targets the same link point gives p ≈ 1.93e6 × 5e-9 = 9.6 × 10⁻³ and
E[attempts] = 1.0097 — a 1% overhead.

**Retries are never the cost. The contended cache line is.** The same lane's
`false_sharing` companion measures one cross-core line transfer at
**38.3 ns** against a 2.28 ns uncontended atomic — so a *single* extra
bounced line costs as much as 17 wasted CAS attempts. Optimise for lines
touched, not for attempts avoided. (Step 6 of the Bw-tree guide has the
counter-example where p really does approach 1, and what it costs.)

The contract comment states the guarantee the whole design buys:

```cpp
// memtable/inlineskiplist.h:20-27 — the thread-safety contract
    20  // Thread safety -------------
    22  // Writes via Insert require external synchronization, most likely a mutex.
    23  // InsertConcurrently can be safely called concurrently with reads and
    24  // with other concurrent inserts.  Reads require a guarantee that the
    25  // InlineSkipList will not be destroyed while the read is in progress.
    26  // Apart from that, reads progress without any internal locking or
    27  // synchronization.
```

"Reads progress without any internal locking or synchronization" is the
prize: a reader writes **nothing**, so it never takes a cache line away from
anyone. Contrast the LWLock guide's Step 3, where a *shared* acquisition is
still a read-modify-write on a shared word.

### Step 4 — what the workload let RocksDB not build: deletion

> **In:** the contract comment above.
> **Out:** the assumption three lines below it that deletes an entire
> subsystem.

The invariants immediately following that contract carry the enabling
assumption:

```cpp
// memtable/inlineskiplist.h:29-38 — the invariants, and the one that matters
    29  // Invariants:
    31  // (1) Allocated nodes are never deleted until the InlineSkipList is
    32  // destroyed.  This is trivially guaranteed by the code since we never
    33  // delete any skip list nodes.
    35  // (2) The contents of a Node except for the next/prev pointers are
    36  // immutable after the Node has been linked into the InlineSkipList.
    37  // Only Insert() modifies the list, and it is careful to initialize a
    38  // node and use release-stores to publish the nodes in one or more lists.
```

Memtable entries are **never deleted**. In the LSM (topic 4), a delete is a
*tombstone insert*, and the whole memtable dies wholesale at flush — its
arena is freed in one shot. No delete ⇒ no "when may I `free()` a node some
reader still holds?" problem ⇒ no epochs, no hazard pointers, no reference
counts, nothing. That is why the crate you read next (crossbeam-epoch) has
no counterpart anywhere in this file.

Invariant (2) is the other half of the same bargain: nodes are immutable
after linking, so a reader that reaches a node never has to re-validate
anything it read. Both invariants are gifts from the LSM's write path, not
properties of skiplists.

The discipline to carry into every code read: always ask **"what did the
workload let them NOT solve?"** — and then check whether *your* workload
grants the same permission. For `concurrent_set.rs` it does not: your tests
require `remove`, so you inherit Step 7's problem and must solve it with
epochs.

### Step 5 — the splice: amortizing the search across nearby inserts

> **In:** the ~31 dependent hops from Step 1, dwarfing the ~2 CASes an
> insert needs.
> **Out:** the cached search path, when it survives, and the compile-time
> door that removes the atomics entirely.

Do the arithmetic that motivates this. An insert at p = 1/4 costs ~31
dependent pointer hops (Step 1) but only `E[height] = 1.33` CASes. If a hop
is an L2/LLC miss at ~15 ns, the search costs ~465 ns and the CASes ~3 ns.
**The search is 99% of the insert.** So RocksDB caches it:

```cpp
// memtable/inlineskiplist.h:340-350 — Splice: a cached search path, with its invariant
   340  struct InlineSkipList<Comparator>::Splice {
   341    // The invariant of a Splice is that prev_[i+1].key <= prev_[i].key <
   342    // next_[i].key <= next_[i+1].key for all i.  That means that if a
   343    // key is bracketed by prev_[i] and next_[i] then it is bracketed by
   344    // all higher levels.  It is _not_ required that prev_[i]->Next(i) ==
   345    // next_[i] (it probably did at some point in the past, but intervening
   346    // or concurrent operations might have inserted nodes in between).
   347    int height_ = 0;
   348    Node** prev_;
   349    Node** next_;
   350  };
```

A `Splice` is the (pred, succ) pair per level left over from the previous
insert. `InsertWithHint` (`:111`) and `InsertWithHintConcurrently` (`:117`)
take one; `RecomputeSpliceLevels` (`:331`, defined `:1016`) repairs **only
the levels the new key invalidated**. Sequential or near-sequential writers
— the common memtable pattern, since keys arrive roughly in order within a
column family — keep most levels and re-search only the bottom one or two.
The last sentence of the invariant is the concurrency subtlety: a splice may
be *stale* (someone inserted between prev and next) without being *wrong*,
which is exactly why the CAS at `:1153` can fail and re-search just its own
level.

One more workload door: `Insert` (`:908`) and `InsertConcurrently` (`:913`)
are the same template (`:1028`) selected by a `UseCAS` bool — look back at
Step 3's `if (UseCAS)` at `:1134` and the `else` at `:1173`. Single-writer
mode skips the atomics entirely, and it is a **compile-time** choice, so
there is not even a branch. (M9 note: FalkorDB's single writer can take
exactly this door.)

### Step 6 — the locking school: lazy locking (memgraph, Herlihy et al.)

> **In:** the same insert problem, and a workload that needs whole towers to
> appear at once.
> **Out:** memgraph's optimistic-find / lock / validate / link / publish
> protocol — and five places where its authors found the published paper
> wrong.

memgraph takes the other road: make the whole tower appear atomically by
briefly locking the neighbours. A **spinlock** is a lock whose waiter
busy-waits instead of sleeping — right for critical sections measured in
nanoseconds, and wrong for anything that might block (see the LWLock
guide's Step 4 for the test-and-test-and-set discipline a good one needs).

Each node (`skip_list.hpp:156`) carries a per-node `SpinLock` (`:163`), two
state bits — `marked` (`:164`) and `fully_linked` (`:165`) — and the
flexible-array tower `nexts[0]` (`:169`), the same intrusive-tower trick as
RocksDB (memgraph's grows upward; RocksDB's downward).

Insert is **optimistic**: find with no locks held, then lock, then check
that what you found is still true.

```cpp
// src/utils/skip_list.hpp:1334-1399 — insert: find, lock, validate, link, publish
  1334      while (true) {
  1335        int layer_found = find_node(object, preds, succs);
  1360          for (int layer = 0; valid && (layer < top_layer); ++layer) {
  1361            TNode *pred = preds[layer];
  1362            TNode *succ = succs[layer];
  1363            if (pred != previous_locked) {
  1364              pred->lock.lock();
  1367              previous_locked = pred;
  1368            }
  1369            // Existence test is missing in the paper.
  1370            valid = !pred->marked.load(std::memory_order_acquire) &&
  1371                    pred->nexts[layer].load(std::memory_order_acquire) == succ &&
  1372                    (succ == nullptr || !succ->marked.load(std::memory_order_acquire));
  1373          }
  1375          if (!valid) continue;
  1390          for (int layer = 0; layer < top_layer; ++layer) {
  1391            new_node->nexts[layer].store(succs[layer], std::memory_order_release);
  1392          }
  1393          for (int layer = 0; layer < top_layer; ++layer) {
  1394            preds[layer]->nexts[layer].store(new_node, std::memory_order_release);
  1395          }
  1396        }
  1398        new_node->fully_linked.store(true, std::memory_order_release);
  1399        size_.fetch_add(1, std::memory_order_acq_rel);
```

The protocol, in order: `find_node` (`:1285`) collects preds/succs with *no*
locks held (so it may be stale); the loop at `:1360-1373` locks each
distinct pred **bottom-up** (deduplicated by `previous_locked` at `:1363`,
because one node is often the pred at several levels) and re-validates that
the pred still points at the succ and neither is marked; a failed validation
drops every lock via the `OnScopeExit` guard at `:1352-1356` and
`continue`s — **a full restart**; on success every level is written
(`:1390-1395`, both stores Release) while the locks are held; and the node
is finally PUBLISHed by `fully_linked.store(true, release)` at `:1398`.

Readers ignore not-yet-`fully_linked` nodes — the publication idiom from
Step 2, with a bit instead of a CAS'd pointer, so the entire tower appears
at once. The cost profile is the exact inverse of Step 3: a lost race costs
a whole-insert restart rather than a one-level re-find, and lock-order
discipline (always bottom-up) is what prevents deadlock.

**And then read the comments.** `:1358-1359` says "The paper has a wrong
condition here. In the paper it states that this loop should have `(layer <=
top_layer)`, but that isn't correct." `:1369`: "Existence test is missing in
the paper." `:1388-1389`: "The paper is also wrong here." Two more in
`remove` (`:1648-1649`, `:1679-1680`, `:1694-1695`). That is **five separate
corrections to a peer-reviewed algorithm** (Herlihy, Lev, Luchangco &
Shavit, SIROCCO 2007), found by people who had to run it. Take it as the
topic's recurring lesson in its most literal form: published concurrent
algorithms are specifications with bugs, and the errata live in the source
files of whoever shipped them.

### Step 7 — real deletion, reclamation, and the scorecard

> **In:** a node that must be removed from a list readers are traversing
> right now.
> **Out:** the two-phase delete, memgraph's accessor-id GC, and the
> side-by-side comparison.

memgraph must delete for real, and it does it in two phases:

```cpp
// src/utils/skip_list.hpp:1662-1701 — remove: mark, then unlink, then collect
  1662      while (true) {
  1663        int layer_found = find_node<GCPolicy::DoNotRun>(key, preds, succs);
  1664        if (is_marked || (layer_found != -1 && ok_to_delete(succs[layer_found], layer_found))) {
  1665          if (!is_marked) {
  1666            node_to_delete = succs[layer_found];
  1667            top_layer = node_to_delete->height;
  1668            node_guard = std::unique_lock{node_to_delete->lock};
  1669            if (node_to_delete->marked.load(std::memory_order_acquire)) {
  1670              return false;
  1671            }
  1672            node_to_delete->marked.store(true, std::memory_order_release);
  1673            is_marked = true;
  1674          }
  1681          for (int layer = 0; valid && (layer < top_layer); ++layer) {
  1688            valid = !pred->marked.load(std::memory_order_acquire) &&
  1689                    pred->nexts[layer].load(std::memory_order_acquire) == succ;
  1690          }
  1692          if (!valid) continue;
  1696          for (int layer = top_layer - 1; layer >= 0; --layer) {
  1697            preds[layer]->nexts[layer].store(node_to_delete->nexts[layer].load(std::memory_order_acquire),
  1698                                             std::memory_order_release);
  1699          }
  1700          gc_.Collect(node_to_delete);
```

`marked.store(true, release)` at `:1672` is the **logical delete** and the
linearization point: readers skip marked nodes, so the key leaves the set
before any pointer moves, and the check-then-set under the node's own lock
(`:1668-1673`) is what makes "exactly one caller returns true" true. Note
also that the unlink at `:1696-1698` runs **top-down**, the mirror of
insert's bottom-up — remove the shortcuts first, the ground truth last, so
the node is never reachable at a high level but absent at level 0.

Deletion exists, so reclamation must too — the problem RocksDB dodged in
Step 4. memgraph's answer is **accessor-id GC**, and its doc comment is the
clearest short description of the family:

```cpp
// src/utils/skip_list.hpp:241-253 — the reclamation scheme, in its own words
   241  /// The skip list doesn't have built-in reclamation of removed nodes (objects).
   242  /// This class handles all operations necessary to remove the nodes safely.
   244  /// Each accessor is given a monotonically increasing ID. When a node is
   245  /// collected (after the skip list has already unlinked it so no new accessor
   246  /// can reach it) the ID of the newest currently-alive accessor is recorded.
   247  /// The node can be freed once that accessor has been destroyed; older ones
   248  /// must have been destroyed too (ReleaseId records a strict prefix of dead ids).
   251  /// alive/dead bits for ~500k accessors. ReleaseId is lock-free (atomic
   252  /// fetch_or). GC walks the blocks to find `live_horizon` (one past the last
   253  /// released id) and frees every pending node whose tag is < live_horizon.
```

Compare crossbeam-epoch line by line and it is the same scheme with a
coarser clock: an `Accessor` (`:877`, taking an id at `:881` and releasing
it in `~Accessor` at `:890`) is a *pin*; the accessor id is the *epoch*; the
`live_horizon` is `is_expired`'s `>= 2` threshold. The differences are that
memgraph's ids are per-accessor rather than a global counter, and that GC
runs opportunistically from `insert` when a tall node appears —
`if (top_layer >= kSkipListGcHeightTrigger) gc_.Run();` (`:1333`, with
`kSkipListGcHeightTrigger = 16` at `:69`). A height-16 tower at p = 1/2
occurs once per 2¹⁶ = 65 536 inserts, so that is a "run maintenance roughly
every 65k inserts" trigger with no counter at all — the same amortisation
crossbeam gets from `PINNINGS_BETWEEN_COLLECT = 128`, sampled from the
existing randomness instead of counted.

That this list is memgraph's *spine* — vertices, edges, and indexes all live
in these lists — is visible in what got bolted on: `create_chunks`
(accessor wrappers at `:944` and `:955`, implementations at `:1716` and
`:1731`, and `create_chunks_` at `:1780`) splits the list into ranges at
max-height elements for parallel analytics scans.

The comparison table (fill it in notes.md):

| | RocksDB `InlineSkipList` | memgraph `SkipList` |
|---|---|---|
| p, max height | 1/4, 12 (`:76-78`) | 1/2, 32 (`:61`, `:106-116`) |
| writers coordinate by | one CAS per level (`:1153`) | per-node spinlocks (`:1364`) |
| readers see partial insert? | yes — levels link independently (fine for a set) | no — `fully_linked` gate (`:1398`) |
| readers write shared memory? | never (`:26-27`) | never (they only read the bits) |
| delete | never (tombstones; invariant (1) `:31-33`) | `marked` bit, then unlink (`:1672`, `:1696`) |
| reclamation | none needed (arena dies at flush) | accessor-id GC (`:241-253`) |
| failure/retry | re-find *one level* from prev (`:1162`) | unlock all, restart insert (`:1375`) |
| single-writer escape | `UseCAS=false`, compile-time (`:1134`) | none |

## Where each step lives in the code

Read RocksDB first, then memgraph as the contrast. Budget ~2 h. In both
files, read the doc comment above the class before the class.

**RocksDB `InlineSkipList`** — `memtable/inlineskiplist.h` at `7c80a5a`

| Step | What | Line |
|---|---|---|
| 1 | `kMaxPossibleHeight = 32`; the ctor defaults `max_height=12, branching_factor=4` | `:70`, `:76-78` |
| 1 | `RandomHeight` — the p = 1/4 coin | `:559-573` |
| 2 | `Next`/`SetNext`/`CASNext` and their barrier comments | `:379-396` |
| 2 | the tower grows *downward*: `&next_[0] - n` | `:352-356`, `:383` |
| 2 | `NoBarrier_SetNext` / `NoBarrier_Next` | `:399-410` |
| 2 | the orderings themselves | `util/atomic.h:60`, `:108-113`, `:118-121` |
| 3 | the thread-safety contract | `:20-27` |
| 3 | `Insert<UseCAS>` — the whole CAS loop | `:1134-1172` |
| 3 | one-level re-find on CAS failure | `:1162-1163`; the reasoning `:1157-1161` |
| 4 | invariant (1): nodes are never deleted | `:29-38` |
| 5 | `Splice` and its invariant | `:340-350`; fwd decl `:64` |
| 5 | `InsertWithHint` / `InsertWithHintConcurrently` | `:111`, `:117` |
| 5 | `RecomputeSpliceLevels` | `:331` (decl), `:1016` (defn) |
| 5 | `Insert` / `InsertConcurrently` / the shared template | `:908`, `:913`, `:1028` |

**memgraph `SkipList`** — `src/utils/skip_list.hpp` at `8f87f6a`; one header
holds the list, the accessors, and the GC.

| Step | What | Line |
|---|---|---|
| 1 | `kSkipListMaxHeight = 32`; `gen_height` (p = 1/2, one draw) | `:61`, `:106-117` |
| 6 | `SkipListNode`, `SpinLock`, `marked`, `fully_linked`, `nexts[0]` | `:156`, `:163`, `:164`, `:165`, `:169` |
| 6 | `find_node` — optimistic, no locks | `:1285` |
| 6 | `insert` — lock bottom-up, validate, link, publish | `:1328-1402`; validate `:1370-1372`; publish `:1398` |
| 6 | the paper's bugs, as found by the implementers | `:1358-1359`, `:1369`, `:1388-1389` |
| 7 | `ok_to_delete` and its paper bug | `:1647-1652` |
| 7 | `remove` — mark, validate, unlink top-down, collect | `:1655-1707`; mark `:1672`; unlink `:1696-1698`; collect `:1700` |
| 7 | the GC scheme, described by its author | `:241-255`; `SkipListGc` `:257`; `Collect` `:367` |
| 7 | `Accessor` — the pin | `:877`; `AllocateId` `:881`; release in `~Accessor` `:890` |
| 7 | `kSkipListGcHeightTrigger = 16`, and where it fires | `:69`, `:1333` |
| 7 | `create_chunks` — parallel scan support | `:944`, `:955` (accessors); `:1716`, `:1731`, `:1780` |

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
Answer each before unfolding it.

- [ ] State each list's height distribution parameter and max height, and
  say why they differ.
  <details><summary>Answer</summary>

  RocksDB: **p = 1/4, max height 12** — the constructor defaults are
  `max_height = 12, branching_factor = 4` (`inlineskiplist.h:76-78`), and
  `RandomHeight` (`:559-573`) increases the height "with probability 1 in
  kBranching". memgraph: **p = 1/2, max height 32** —
  `kSkipListMaxHeight = 32` (`skip_list.hpp:61`) and `gen_height`
  (`:106-117`) takes `__builtin_ffs` of a uniform 32-bit draw, which is 1
  with probability ½, 2 with ¼, and so on.

  They differ on *memory*, not on search cost. Expected hops are
  `(1/p)·log_{1/p}(n)`, which at n = 50 000 is 4 × 7.80 = 31.2 for p = 1/4
  and 2 × 15.61 = 31.2 for p = 1/2 — identical, because `(1/p)/ln(1/p)` is
  flat between those values. But expected tower height is `1/(1−p)`: 1.33
  pointers per node at p = 1/4 against 2.00 at p = 1/2. A size-capped
  memtable buys 33% more keys per flush; a long-lived graph store spends
  the pointers.
  </details>

- [ ] RocksDB's insert loses a CAS at level 3. What exactly does it redo?
  <details><summary>Answer</summary>

  Only level 3, and not from the head. `Insert<UseCAS>` calls
  `FindSpliceForLevel<false>(key_decoded, splice->prev_[i], nullptr, i, …)`
  at `inlineskiplist.h:1162-1163` — the search restarts from the *old*
  `prev_[i]` and scans forward. The comment at `:1157-1161` gives the
  reason: it is unlikely that many nodes were inserted between prev and
  next, and `next_[i]` is known stale so it is useless as a hint.

  It also sets `splice_is_valid = false` when `i > 0` (`:1168-1170`),
  because narrowing the bracket at level i can break the `Splice` invariant
  at `:341-346` — so the *next* insert recomputes the whole path rather
  than trusting a cache that may now be inconsistent between levels.
  Levels 0, 1 and 2, already linked, are untouched.
  </details>

- [ ] Estimate the expected number of CAS attempts per insert on this
  topic's `scaling` workload, and say what that implies about where the
  cost is.
  <details><summary>Answer</summary>

  `E[attempts] = 1/(1−p)` where p is the per-attempt failure probability.
  The lane runs 16 threads at 19.28 Mops/s with 10% writes ⇒ 1.93 M
  inserts/s over a 50 000-key keyspace ⇒ ~39 inserts/s at any one level-0
  link point. With a CAS window of ~5 ns, p ≈ 39 × 5e-9 ≈ **2 × 10⁻⁷** and
  E[attempts] ≈ **1.0000002** — one retry per five million inserts. Even if
  every insert hit the *same* link point, p ≈ 9.6 × 10⁻³ and E[attempts] ≈
  1.0097.

  Implication: **retry cost is noise, and "lock-free wastes work on
  retries" is the wrong worry.** One cross-core cache-line transfer costs
  38.3 ns on this machine against a 2.28 ns uncontended atomic — a single
  bounced line is worth ~17 wasted CAS attempts. Count lines touched, not
  attempts avoided.
  </details>

- [ ] Name the two invariants at the top of `inlineskiplist.h` and the
  subsystem each one deletes.
  <details><summary>Answer</summary>

  (1) at `:31-33`: "Allocated nodes are never deleted until the
  InlineSkipList is destroyed." That deletes *all* of memory reclamation —
  no epochs, no hazard pointers, no reference counts. It is granted by the
  LSM (topic 4): a delete is a tombstone insert, and the whole memtable's
  arena is freed in one shot at flush.

  (2) at `:35-38`: node contents other than the next pointers are immutable
  once linked. That deletes re-validation — a reader that reaches a node
  never has to check whether what it read is still true, which is precisely
  the check memgraph's `insert` must perform at `skip_list.hpp:1370-1372`.
  Neither invariant is a property of skiplists; both are gifts from the
  write path.
  </details>

- [ ] memgraph's `insert` finds preds and succs with no locks held. What
  are the three conditions it re-checks after locking, and what breaks if
  you skip them?
  <details><summary>Answer</summary>

  `skip_list.hpp:1370-1372`, per level: the pred is not `marked`; the
  pred's `nexts[layer]` still equals the succ we found; and the succ (if
  non-null) is not `marked`.

  Skip the middle one and you lose inserts: a concurrent insert of a
  smaller key between pred and succ would be silently overwritten when
  `:1394` stores the new node into `pred->nexts[layer]`. Skip the marked
  checks and you link into a node that is being unlinked, so the new node
  is reachable through a corpse and vanishes when the corpse is spliced
  out. The third check is not in the published algorithm at all — the
  comment at `:1369` says "Existence test is missing in the paper" — one of
  five errata this file records against Herlihy et al. (`:1358-1359`,
  `:1369`, `:1388-1389`, `:1648-1649`, `:1679-1680`).
  </details>

- [ ] memgraph triggers GC from `insert`, not from a timer or a counter.
  What is the trigger, and how often does it fire?
  <details><summary>Answer</summary>

  `if (top_layer >= kSkipListGcHeightTrigger) gc_.Run();` —
  `skip_list.hpp:1333`, with `kSkipListGcHeightTrigger = 16` at `:69`. The
  trigger is the randomly-drawn height of the node being inserted.

  At p = 1/2, a tower of height ≥ 16 occurs with probability 2⁻¹⁶, so GC
  runs roughly **once per 65 536 inserts** — and it needs no counter, no
  clock, and no shared state to decide, because it reuses randomness the
  insert had to generate anyway. Compare crossbeam's
  `PINNINGS_BETWEEN_COLLECT = 128` (`internal.rs:335`), which buys the same
  amortisation with an explicit thread-local counter. Both are the same
  move: make maintenance a bounded, rare, opportunistic step on an existing
  hot path.
  </details>

## References

**Papers**

| Paper | What to take |
|---|---|
| Herlihy, Lev, Luchangco, Shavit — *A Simple Optimistic Skiplist Algorithm* (SIROCCO 2007) | the lazy-locking design memgraph implements — read it **next to** `skip_list.hpp`, whose comments record five corrections to it |
| Pugh — *Skip Lists: A Probabilistic Alternative to Balanced Trees* (CACM 1990) | where `(1/p)·log_{1/p}(n)` and the p = 1/4 recommendation come from |

**Code**

| File | Lines | What |
|---|---|---|
| `memtable/inlineskiplist.h` (`rocksdb@7c80a5a`) | 20–38 | the contract and the two invariants — start here |
| | 70–78, 559–573 | p = 1/4, max height 12 |
| | 340–350 | `Splice` and its invariant |
| | 352–410 | `Node` — downward tower, barrier-carrying accessors |
| | 1134–1172 | `Insert<UseCAS>` — the CAS loop, in full |
| | 908, 913, 1028 | `Insert` / `InsertConcurrently` / the shared template |
| `util/atomic.h` (`rocksdb@7c80a5a`) | 60, 108–121 | what `Load`, `Store`, `CasStrong` actually order |
| `src/utils/skip_list.hpp` (`memgraph@8f87f6a`) | 61, 69, 106–117 | max height, GC trigger, `gen_height` |
| | 156–169 | the node: lock, `marked`, `fully_linked`, tower |
| | 241–255, 257, 367 | the accessor-id GC, described by its author |
| | 877–890 | `Accessor` — the pin, and its release |
| | 1328–1402 | `insert` — and three of the paper's bugs |
| | 1647–1707 | `ok_to_delete` and `remove` — and two more |

**Measurements** — from this topic's lanes; see `notes.md` and `FINDINGS.md`
row 9.

| Lane | Figure used above |
|---|---|
| `scaling` | crossbeam `SkipSet` 4.21 → 19.28 Mops/s (1 → 16 threads); global mutex 8.65 → **2.96** |
| `scaling` | keyspace 100 000, 50 000-key preload, 10% writes — the inputs to the retry estimate |
| `false_sharing` | one cross-core line transfer = **38.3 ns**; uncontended padded atomic = 2.28 ns |

**Cross-topic** — topic 2 for this same RocksDB file read for its *layout*;
topic 4 for the LSM that grants invariant (1); the crossbeam-epoch guide for
the reclamation memgraph hand-rolled; the LWLock guide, Step 4, for what a
production spinlock has to do that a naive one does not.
