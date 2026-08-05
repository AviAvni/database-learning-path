# InlineSkipList: lock-free by refusing to delete

This is where LSM write throughput lives: every `Put` in half the industry
lands in this one header. Two ideas are the whole file — a node layout that
puts the hot pointer and the key in the same allocation by indexing the tower
*negatively*, and a concurrency contract kept small by one workload
restriction: memtables never delete, they freeze and drop wholesale. This
chapter builds up to both — what a memtable is, why it is a skiplist, the
layout trick, then the lock-free insert — before pointing you at the lines.
Budget: 1–2 h.

Everything below is read against **facebook/rocksdb at `7c80a5a`**, where
`memtable/inlineskiplist.h` is 1422 lines. Line numbers move between releases;
re-check yours with:

```
tools/pinned-source.py ref rocksdb
tools/pinned-source.py show rocksdb memtable/inlineskiplist.h -r 350:422
```

If a number below does not match what you see, trust your checkout and record
the drift in `notes.md` — that is the exercise, not an error report.

## The problem in one sentence

Eight writer threads must insert into one sorted in-memory structure at
millions of ops/s without a lock serializing them — a single mutex around a
sorted map caps the whole LSM engine at one core.

## The concepts, step by step

### Step 1 — the memtable: the sorted buffer every LSM write hits first

> **In:** an LSM engine that must accept writes at memory speed and hand disk
> a *sorted* file.
> **Out:** three requirements — ordered iteration, concurrent insert — and one
> non-requirement, delete, which is the lever the rest of the file pulls.

In an LSM engine (topic 1), every write goes to an in-memory buffer — the
**memtable** — which, when full, is **frozen** (made immutable), flushed to
disk as a sorted file, and then dropped wholesale. The full threshold is
`write_buffer_size`, default **64 MiB**:

```
// include/rocksdb/options.h — write_buffer_size, 175-191
   175    // Amount of data to build up in memory (backed by an unsorted log
   176    // on disk) before converting to a sorted on-disk file.
   177    //
   178    // Larger values increase performance, especially during bulk loads.
   ... 179-187: max_write_buffer_number, recovery time, per-column-family note ...
   188    // Default: 64MB
   ... 189-190: dynamically changeable through SetOptions() ...
   191    size_t write_buffer_size = 64 << 20;
```

Two requirements follow from "converting to a sorted on-disk file": the
structure must support **sorted iteration**, and it must absorb **concurrent
inserts** from many writer threads — RocksDB turns those on by default:

```
// include/rocksdb/options.h — allow_concurrent_memtable_write, 1421-1429
  1421    // If true, allow multi-writers to update mem tables in parallel.
  1422    // Only some memtable_factory-s support concurrent writes; currently it
  1423    // is implemented only for SkipListFactory.  Concurrent memtable writes
  1424    // are not compatible with inplace_update_support or filter_deletes.
  ...
  1429    bool allow_concurrent_memtable_write = true;
```

One **non**-requirement matters just as much: the structure never needs to
*delete* a node. Even a user's `Delete` is an insert (a tombstone entry);
physical removal happens only when the whole frozen memtable is dropped at
once. Hold onto that — Step 6 spends it.

### Step 2 — why a skiplist, not a hash table or B-tree

> **In:** the two requirements from Step 1 — ordered iteration and concurrent
> insert.
> **Out:** the elimination argument that leaves a skiplist, and the property
> that makes it CAS-able: an insert touches independent single words.

A hash table has no ordered iteration — flushing to a sorted file would
require sorting 64 MiB of entries on every flush. A B-tree keeps order but
inserts trigger node splits: multi-node rewrites that need real latching under
concurrency, because a split must appear atomic to a concurrent reader
descending through it.

A skiplist (previous chapter) keeps order, and an insert touches only a
handful of *independent* forward pointers — each one a single word that can be
swapped atomically with **CAS** (compare-and-swap: one atomic CPU instruction
that writes a new value only if the location still holds the expected old
value, and reports failure otherwise). Independent single-word updates are
exactly the shape lock-free programming can handle without a multi-word atomic
primitive that no hardware provides. That is the whole case: sortedness plus
CAS-able inserts.

The price is honest and this repo has measured it. Topic 0's `lookup_shootout`
lane at n = 10⁶ reports `hashmap 8.8 ns`, `btreemap 26.6 ns`,
`vec_binary_search 25.8 ns` per lookup. A skiplist is a *worse* pointer chase
than a B-tree — same dependent misses, less fanout per cache line — so the
ordered structures here are already ~3× the hash table on point lookups, and a
skiplist lands at or above the B-tree. RocksDB accepts that ratio to buy
concurrent writers and a sorted flush. Different RUM position, deliberately
chosen.

### Step 3 — the node layout: one allocation, tower indexed negatively

> **In:** a skiplist node needs a key, a height, and `height` forward
> pointers.
> **Out:** a three-region single allocation with the `Node*` pointing at the
> *middle*, so the hot fields (level-0 link and the key) are adjacent and the
> cold tower sits behind the pointer.

A textbook skiplist node holds a key *pointer* and an array of forward
pointers — so a lookup touches the node, then chases the pointer to the key:
two dependent misses per comparison. RocksDB's own header opens by naming the
saving it is after:

```
// memtable/inlineskiplist.h — file header, 10-18
    10  // InlineSkipList is derived from SkipList (skiplist.h), but it optimizes
    11  // the memory layout by requiring that the key storage be allocated through
    12  // the skip list instance.  For the common case of SkipList<const char*,
    13  // Cmp> this saves 1 pointer per skip list node and gives better cache
    14  // locality, at the expense of wasted padding from using AllocateAligned
    15  // instead of Allocate for the keys.  The unused padding will be from
    16  // 0 to sizeof(void*)-1 bytes, and the space savings are sizeof(void*)
    17  // bytes, so despite the padding the space used is always less than
    18  // SkipList<const char*, ..>.
```

The comment above `Node` says how, and it is the single most surprising line
in the file:

```
// memtable/inlineskiplist.h — Node layout comment, 352-356
   352  // The Node data type is more of a pointer into custom-managed memory than
   353  // a traditional C++ struct.  The key is stored in the bytes immediately
   354  // after the struct, and the next_ pointers for nodes with height > 1 are
   355  // stored immediately _before_ the struct.  This avoids the need to include
   356  // any pointer or sizing data, which reduces per-node memory overheads.
```

So one allocation, three regions, `Node*` aimed at the middle:

```
 raw allocation (AllocateNode, line 868):

 ┌───────────────────────────┬───────────────┬──────────────────┐
 │ tower: next_[-(h-1)]…[-1] │ Node: next_[0]│ key bytes inline │
 └───────────────────────────┴───────────────┴──────────────────┘
   prefix = 8*(h-1) bytes      ▲ Node* points HERE
                               │
   level n reached by NEGATIVE index (&next_[0] - n)     line 383
   key reached as (&next_[1])                            line 374
```

Both tricks are one line each, and the struct that makes them legal is a
one-element array:

```
// memtable/inlineskiplist.h — Node accessors, 374-396 and 417-420
   374    const char* Key() const { return reinterpret_cast<const char*>(&next_[1]); }
   375
   376    // Accessors/mutators for links.  Wrapped in methods so we can add
   377    // the appropriate barriers as necessary, and perform the necessary
   378    // addressing trickery for storing links below the Node in memory.
   379    Node* Next(int n) {
   380      assert(n >= 0);
   381      // Use an 'acquire load' so that we observe a fully initialized
   382      // version of the returned Node.
   383      return ((&next_[0] - n)->Load());
   384    }
   385
   386    void SetNext(int n, Node* x) {
   387      assert(n >= 0);
   388      // Use a 'release store' so that anybody who reads through this
   389      // pointer observes a fully initialized version of the inserted node.
   390      (&next_[0] - n)->Store(x);
   391    }
   392
   393    bool CASNext(int n, Node* expected, Node* x) {
   394      assert(n >= 0);
   395      return (&next_[0] - n)->CasStrong(expected, x);
   396    }
   ... 397-416: no-barrier variants and InsertAfter ...
   417   private:
   418    // next_[0] is the lowest level link (level 0).  Higher levels are
   419    // stored _earlier_, so level 1 is at next_[-1].
   420    Atomic<Node*> next_[1];
```

Line 374 is the payoff: `&next_[1]` is the byte just past the struct, which is
where the key was written. No key pointer exists, so no key pointer can be
missed on. Line 383 is the other half: `&next_[0] - n` walks *backwards* into
the prefix.

The allocation that sets this up does the arithmetic explicitly:

```
// memtable/inlineskiplist.h — AllocateNode, 858-880
   858  template <class Comparator>
   859  typename InlineSkipList<Comparator>::Node*
   860  InlineSkipList<Comparator>::AllocateNode(size_t key_size, int height) {
   861    auto prefix = sizeof(Atomic<Node*>) * (height - 1);
   862
   863    // prefix is space for the height - 1 pointers that we store before
   864    // the Node instance (next_[-(height - 1) .. -1]).  Node starts at
   865    // raw + prefix, and holds the bottom-mode (level 0) skip list pointer
   866    // next_[0].  key_size is the bytes for the key, which comes just after
   867    // the Node.
   868    char* raw = allocator_->AllocateAligned(prefix + sizeof(Node) + key_size);
   869    Node* x = reinterpret_cast<Node*>(raw + prefix);
   ... 870-877: comment explaining why height need not be stored ...
   878    x->StashHeight(height);
   879    return x;
   880  }
```

Line 868 is one allocation for tower + node + key. Line 878 is a second trick
worth naming: the height is not a field. It is written *into* `next_[0]`
(`StashHeight`, lines 361-364) and read back by `Insert` (`UnstashHeight`,
lines 368-372) before that slot is used as a pointer — so a node carries no
height field at all, because a search that arrived at level *h* already knows
*h* is valid for this node (the comment at 870-877 says exactly this).

**Work the saving.** On a 64-bit build `sizeof(void*)` = 8. Node height is
geometric with p = 1/4, so E[height] = 1/(1 − p) = 1/0.75 = **1.333 levels**,
i.e. 8 × 1.333 = **10.67 bytes** of forward pointers per node. The classic
`SkipList<const char*>` adds one more word for the key pointer: 10.67 + 8 =
**18.67 bytes** of metadata per node. InlineSkipList drops the key pointer and
pays alignment padding of 0–7 bytes instead (header lines 15-17), so with
uniformly distributed key lengths the expected padding is 3.5 bytes and the
expected net saving is 8 − 3.5 = **4.5 bytes per node**. In a 64 MiB memtable
holding 100-byte entries — 67,108,864 / 100 = **671,089 entries** — that is
671,089 × 4.5 = 3,019,899 bytes = **2.88 MiB, or 4.5% of the memtable**. Which
is 4.5% more user data per flush, hence ~4.5% fewer flushes and ~4.5% less L0
write amplification. The header's "always less than `SkipList<const char*>`"
(lines 17-18) is the guaranteed version of that: the saving is 8 and the
worst-case padding is 7, so the difference never goes the wrong way.

This is README §4's "dense filter / inline payload" pattern once more, by an
author who priced the cache lines: the common case — a level-0 step followed
by a key compare — touches `next_[0]` and the key bytes, which are adjacent.
The taller levels, needed by only 1/4 of nodes, sit *before* the node, out of
the hot path.

### Step 4 — the concurrency contract: publish with acquire/release

> **In:** readers walking the list with no lock while a writer is building and
> linking a node.
> **Out:** the exact memory ordering on each link operation, and why a
> *relaxed* store appears in the middle of a lock-free insert without breaking
> anything.

Readers and writers share the list through the forward pointers, so every link
is an atomic with declared ordering. The header states the contract:

```
// memtable/inlineskiplist.h — thread safety and invariants, 20-38
    20  // Thread safety -------------
    21  //
    22  // Writes via Insert require external synchronization, most likely a mutex.
    23  // InsertConcurrently can be safely called concurrently with reads and
    24  // with other concurrent inserts.  Reads require a guarantee that the
    25  // InlineSkipList will not be destroyed while the read is in progress.
    26  // Apart from that, reads progress without any internal locking or
    27  // synchronization.
    28  //
    29  // Invariants:
    30  //
    31  // (1) Allocated nodes are never deleted until the InlineSkipList is
    32  // destroyed.  This is trivially guaranteed by the code since we never
    33  // delete any skip list nodes.
    34  //
    35  // (2) The contents of a Node except for the next/prev pointers are
    36  // immutable after the Node has been linked into the InlineSkipList.
    37  // Only Insert() modifies the list, and it is careful to initialize a
    38  // node and use release-stores to publish the nodes in one or more lists.
```

The orderings behind `Load`, `Store` and `CasStrong` are one file away:

```
// util/atomic.h — Atomic<T>, 104-121
   104  template <typename T>
   105  class Atomic : public RelaxedAtomic<T> {
   106   public:
   107    explicit Atomic(T initial = {}) : RelaxedAtomic<T>(initial) {}
   108    void Store(T desired) {
   109      RelaxedAtomic<T>::v_.store(desired, std::memory_order_release);
   110    }
   111    T Load() const {
   112      return RelaxedAtomic<T>::v_.load(std::memory_order_acquire);
   113    }
   ... 114-117: CasWeak, acq_rel ...
   118    bool CasStrong(T& expected, T desired) {
   119      return RelaxedAtomic<T>::v_.compare_exchange_strong(
   120          expected, desired, std::memory_order_acq_rel);
   121    }
```

So `Next()` is an **acquire** load (line 112), `SetNext()` is a **release**
store (line 109), and `CASNext()` is an **acq_rel** compare-exchange (line
120). This is the classic publish pattern (topic 9): fully construct the node,
*then* make it reachable with one release operation. Release on the writer
side orders everything written before it — the node's key bytes — ahead of the
pointer store; acquire on the reader side means a reader that observes the new
pointer also observes those bytes. No reader can see a half-built node.

`SeqCst` would add a global total order across *all* atomics, which nothing
here needs and which costs a full barrier on x86 stores and a `dmb ish` on
ARM. Acquire/release is exactly the pairing the invariant requires.

And now the subtlety that the pseudocode version of this chapter used to get
wrong. Look at lines 404-407: `NoBarrier_SetNext` is a **relaxed** store. It
is used at line 1152 to set the *new node's* outgoing pointer just before the
CAS at 1153 publishes it. That is safe because the new node is not yet
reachable by any reader — there is nothing to order against — and the CAS at
1153 is itself `acq_rel`, so it orders the relaxed store ahead of the moment
the node becomes visible. The release is on the CAS, not on the node's own
pointer write. `InsertAfter` (lines 410-415) states the rule in a comment:
"NoBarrier_SetNext() suffices since we will add a barrier when we publish a
pointer to `this` in prev."

### Step 5 — the lock-free insert: level 0 is the only truth

> **In:** a fully built node with a stashed height, and a *splice* — the
> prev/next pair at every level, the same role as redis's `update[]`.
> **Out:** the bottom-up CAS loop, the reason a partially linked node is never
> incorrect, and the two duplicate checks that only run at level 0.

`Insert` and `InsertConcurrently` are the same template body with a `UseCAS`
flag; the concurrent one differs only in putting the splice on the stack
instead of reusing the cached one:

```
// memtable/inlineskiplist.h — Insert entry points, 907-920
   907  template <class Comparator>
   908  bool InlineSkipList<Comparator>::Insert(const char* key) {
   909    return Insert<false>(key, seq_splice_, false);
   910  }
   911
   912  template <class Comparator>
   913  bool InlineSkipList<Comparator>::InsertConcurrently(const char* key) {
   914    Node* prev[kMaxPossibleHeight];
   915    Node* next[kMaxPossibleHeight];
   916    Splice splice;
   917    splice.prev_ = prev;
   918    splice.next_ = next;
   919    return Insert<true>(key, &splice, false);
   920  }
```

The single-threaded `Insert` reuses `seq_splice_` — a splice cached on the
list itself (line 842) — because a sequential writer can trust the splice it
computed last time. A concurrent writer cannot, so it gets a fresh stack one.
Note `kMaxPossibleHeight` = 32 (line 70) makes those stack arrays a fixed
512 bytes; that constant exists so this allocation-free path is legal.

Here is the loop itself:

```
// memtable/inlineskiplist.h — Insert<UseCAS>, CAS path, 1134-1172
  1134    if (UseCAS) {
  1135      for (int i = 0; i < height; ++i) {
  1136        while (true) {
  1137          // Checking for duplicate keys on the level 0 is sufficient
  1138          if (UNLIKELY(i == 0 && splice->next_[i] != nullptr &&
  1139                       compare_(splice->next_[i]->Key(), key_decoded) <= 0)) {
  1140            // duplicate key
  1141            return false;
  1142          }
  1143          if (UNLIKELY(i == 0 && splice->prev_[i] != head_ &&
  1144                       compare_(splice->prev_[i]->Key(), key_decoded) >= 0)) {
  1145            // duplicate key
  1146            return false;
  1147          }
  ... 1148-1151: two asserts that the splice still brackets the key ...
  1152          x->NoBarrier_SetNext(i, splice->next_[i]);
  1153          if (splice->prev_[i]->CASNext(i, splice->next_[i], x)) {
  1154            // success
  1155            break;
  1156          }
  1157          // CAS failed, we need to recompute prev and next. It is unlikely
  1158          // to be helpful to try to use a different level as we redo the
  1159          // search, because it should be unlikely that lots of nodes have
  1160          // been inserted between prev[i] and next[i]. No point in using
  1161          // next[i] as the after hint, because we know it is stale.
  1162          FindSpliceForLevel<false>(key_decoded, splice->prev_[i], nullptr, i,
  1163                                    &splice->prev_[i], &splice->next_[i]);
  1164
  1165          // Since we've narrowed the bracket for level i, we might have
  1166          // violated the Splice constraint between i and i-1.  Make sure
  1167          // we recompute the whole thing next time.
  1168          if (i > 0) {
  1169            splice_is_valid = false;
  1170          }
  1171        }
  1172      }
```

Read line 1135 first: `i` counts **up**, so level 0 is linked before any
express lane. That direction is the correctness argument. Level 0 contains
*every* node, so a search that descends to level 0 finds everything; the upper
levels are only shortcuts. A node that is linked at level 0 but not yet at
level 3 is merely *slower to find* — never missing. That asymmetry is what
lets each level be CAS'd independently, with no multi-word atomicity anywhere.

Line 1153 is the publish, and the retry at 1162-1163 is deliberately narrow:
on a lost race it re-finds the bracket for **level i only**, starting from the
`prev_[i]` it already has, rather than restarting the whole search. The
comment at 1157-1161 justifies it — the window between `prev[i]` and `next[i]`
is small, so a local re-scan almost always finds the new neighbour in a hop or
two. Lines 1168-1169 pay for that shortcut: a narrowed bracket at level i may
no longer nest inside level i−1's, so the cached splice is marked invalid for
next time rather than silently reused.

Lines 1137-1147 are the last piece: duplicate detection runs **only at level
0**, guarded by `i == 0`, because level 0 is where every key lives. That is
the same fact used twice — once for correctness of partial linking, once to
avoid three redundant comparisons per insert.

### Step 6 — no deletes: the restriction that removes an entire literature

> **In:** invariant (1) from Step 4's header quote.
> **Out:** the class of machinery that invariant deletes, and the two redis
> features it costs.

Invariant (1) at lines 31-33 states the contract: **allocated nodes are never
deleted until the list is destroyed**. General lock-free deletion is a
research problem, not an implementation detail: an unlinked node may still be
held by a concurrent reader mid-traversal, so freeing it safely needs hazard
pointers, epoch-based reclamation, or RCU — hundreds of lines and a
reclamation thread. InlineSkipList sidesteps all of it with the Step 1
workload fact: memtables are insert-only until frozen, then dropped wholesale,
so nothing is ever freed while a reader runs. One workload restriction removes
an entire class of machinery. That is the design lesson of the file.

It is also why the redis skiplist's **spans** and **backward pointers** are
absent here. Both require updating several pointers *as one atomic step*: a
span is a count that every node above the insertion point must increment
together with the link, and a backward pointer means the successor's `back`
field and the predecessor's `next` field must change together. No single CAS
covers two words, so both features are incompatible with this insert loop.
Redis pays a global lock (single-threaded) and buys `ZRANK` in O(log n);
RocksDB pays no lock and gives up rank queries it never needed.

### Step 7 — the supporting cast: heights, arena, and pluggable memtables

> **In:** the insert path from Step 5, which still needs a height and memory.
> **Out:** where the height comes from, why the allocator is not the next
> bottleneck, and the interface that makes the whole skiplist swappable.

Heights come from a coin flip with no loop-carried allocation:

```
// memtable/inlineskiplist.h — RandomHeight, 558-573
   558  template <class Comparator>
   559  int InlineSkipList<Comparator>::RandomHeight() {
   560    auto rnd = Random::GetTLSInstance();
   561
   562    // Increase height with probability 1 in kBranching
   563    int height = 1;
   564    while (height < kMaxHeight_ && height < kMaxPossibleHeight &&
   565           rnd->Next() < kScaledInverseBranching_) {
   566      height++;
   567    }
   ... 568-571: sync point and asserts ...
   572    return height;
   573  }
```

`kScaledInverseBranching_` is `(Random::kMaxNext + 1) / kBranching_` (line
837), so the comparison at line 565 is a p = 1/`kBranching_` coin without a
division. The defaults are in the constructor signature: `max_height = 12`,
`branching_factor = 4` (lines 77-78), and `kMaxPossibleHeight = 32` (line 70)
is the compile-time cap that sizes the stack arrays in Step 5.

**Work the numbers.** With p = 1/4, the expected search cost is
log₄(n) levels × (1 − p)/p forward hops per level. At n = 10⁶:
log₄(10⁶) = ln(10⁶)/ln(4) = 13.8155/1.3863 = **9.97 levels**, and
(1 − 0.25)/0.25 = **3.0 hops per level**, so 9.97 × 3.0 = **~30 dependent
pointer hops** per lookup. Now check the cap: `kMaxHeight_` = 12 means the
tallest express lane spans 4¹² = **16,777,216 entries**, and a 64 MiB memtable
of 100-byte entries holds 67,108,864 / 100 = **671,089** entries, needing
log₄(671,089) = **9.68 levels**. The default 12 is sized for the default
memtable with headroom, not for a general-purpose index — raise
`write_buffer_size` far enough and 12 stops being enough, which is why it is a
constructor parameter.

Compare those ~30 hops with topic 0's measured `lookup_shootout` at n = 10⁶:
`hashmap 8.8 ns`. Thirty dependent accesses cannot happen in 8.8 ns — topic
0's ladder puts DRAM at ~100 ns and L1 at ~1 ns, so the skiplist only survives
because the top levels are tiny and stay cached, while the last few hops miss.
The hash table wins point lookups outright. What it cannot do is iterate in
order for the flush, or absorb eight concurrent writers without a latch.

Nodes come from a **concurrent arena**, and its class comment is more precise
than "lock-free":

```
// memory/concurrent_arena.h — ConcurrentArena, 35-41 and 57-68
    35  // ConcurrentArena wraps an Arena.  It makes it thread safe using a fast
    36  // inlined spinlock, and adds small per-core allocation caches to avoid
    37  // contention for small allocations.  To avoid any memory waste from the
    38  // per-core shards, they are kept small, they are lazily instantiated
    39  // only if ConcurrentArena actually notices concurrent use, and they
    40  // adjust their size so that there is no fragmentation waste when the
    41  // shard blocks are allocated from the underlying main arena.
    ...
    57    char* AllocateAligned(size_t bytes, size_t huge_page_size = 0,
    58                          Logger* logger = nullptr) override {
    59      size_t rounded_up = ((bytes - 1) | (sizeof(void*) - 1)) + 1;
    ... 60-62: assert that rounding is correct and pointer-aligned ...
    63      return AllocateImpl(rounded_up, huge_page_size != 0 /*force_arena*/,
    ... 64-67: lambda falling back to arena_.AllocateAligned ...
    68    }
```

It is a bump allocator behind a **spinlock**, with per-core shards that are
created lazily only once contention is observed (lines 38-39) — not a
lock-free allocator. That is enough: the common path takes a shard-local bump
and never reaches the spinlock, so `malloc` does not become the next
bottleneck behind the lock-free list. `MemTable` holds one directly
(`db/memtable.h:914`, `ConcurrentArena arena_;`), which is also why freeing
the memtable is a single arena teardown — the other half of Step 6's bargain.

Finally, the skiplist is only *one* implementation of the `MemTableRep`
interface (`memtable/skiplistrep.cc:17`, `class SkipListRep : public
MemTableRep`, in a 425-line file). Its siblings ship in the same directory:
`hash_skiplist_rep.cc` (hash → per-bucket skiplists, for point-heavy
workloads), `hash_linklist_rep.cc`, and `vectorrep.cc` (bulk load: append,
sort on flush). The memtable is *pluggable* because the RUM position differs
per workload — RocksDB ships four answers and lets you pick. Note the
`allow_concurrent_memtable_write` comment from Step 1: only `SkipListFactory`
supports concurrent writes, so choosing a sibling silently costs you the
property this whole chapter is about.

## Where each step lives in the code

All in `memtable/inlineskiplist.h` unless another file is named.

| Lines | What | Step |
|-------|------|------|
| `options.h:175-191` | `write_buffer_size = 64 << 20` | 1 |
| `options.h:1421-1429` | `allow_concurrent_memtable_write = true` | 1 |
| 10-18 | header: "saves 1 pointer per skip list node" | 3 |
| 20-27 | thread-safety contract | 4 |
| 31-33 | invariant (1): nodes are never deleted | 6 |
| 35-38 | invariant (2): release-stores publish nodes | 4 |
| 70 | `kMaxPossibleHeight = 32` | 5, 7 |
| 77-78 | ctor defaults `max_height = 12`, `branching_factor = 4` | 7 |
| 352-356 | Node layout comment — key after, tower before | 3 |
| 361-372 | `StashHeight` / `UnstashHeight` — height in `next_[0]` | 3 |
| 374 | `Key()` = `&next_[1]` | 3 |
| 379-396 | `Next` acquire / `SetNext` release / `CASNext` | 4 |
| 404-407, 410-415 | `NoBarrier_SetNext` and the comment justifying it | 4 |
| 417-420 | `Atomic<Node*> next_[1]` and the negative-index comment | 3 |
| `util/atomic.h:104-121` | the actual orderings behind Load/Store/CasStrong | 4 |
| 558-573 | `RandomHeight` | 7 |
| 837 | `kScaledInverseBranching_` — the division-free coin | 7 |
| 853-856 | `AllocateKey` — the caller's entry point | 3 |
| 858-880 | `AllocateNode` — one allocation, `Node*` at `raw + prefix` | 3 |
| 907-920 | `Insert` vs `InsertConcurrently` | 5 |
| 1030-1044 | recover `Node*` from key, unstash height, grow `max_height_` | 5 |
| 1047-1131 | splice validation and `RecomputeSpliceLevels` | 5 |
| 1134-1172 | the CAS path — the heart of the file | 5 |
| 1173-1199 | the non-CAS path, for contrast | 5 |
| `concurrent_arena.h:35-41, 57-68` | spinlock + per-core shards | 7 |
| `db/memtable.h:914` | `ConcurrentArena arena_;` | 7 |
| `skiplistrep.cc:17` | `SkipListRep : public MemTableRep` | 7 |

A route through it that builds rather than jumps:

1. Read the header, lines 10-38, in one pass. It is a design document: the
   layout rationale, the thread-safety contract, and both invariants.
2. `Node`, lines 352-420. Find line 374 and line 383 and satisfy yourself that
   they address opposite sides of the same pointer.
3. `AllocateNode`, 858-880. Confirm line 868 allocates all three regions at
   once and line 869 aims `Node*` at the middle.
4. `util/atomic.h:104-121`. Write down which ordering each of `Load`, `Store`,
   `CasStrong` carries; you will need them in the next step.
5. The CAS loop, 1134-1172. Follow one insert of a height-3 node: which level
   is linked first, what happens on a failed CAS at level 2, and why the
   duplicate checks are guarded by `i == 0`.
6. **Aha:** line 1152 is a *relaxed* store immediately before the CAS at 1153
   that publishes the node. Once you see why that is not a bug — the node is
   unreachable until 1153 succeeds, and the `acq_rel` CAS orders the relaxed
   store ahead of publication — you have understood the file's memory model.
   Every other ordering decision follows from the same rule.

**Contrast case.** Read the non-CAS path at lines 1173-1199 straight after the
CAS path. Same loop, same bottom-up direction, but the link is a plain
`SetNext` at line 1197 with no retry and no `splice_is_valid` bookkeeping,
because external synchronisation guarantees the splice is still accurate. The
diff between the two branches is precisely the cost of lock-freedom in this
design: one retry loop, one narrow re-find, and one invalidation flag. That is
small — and it is small *because* of invariant (1). Compare with redis's
`zslInsert`, which needs no atomics at all because it holds the only thread.

## Questions to answer in notes.md

1. Redis's skiplist has spans and backward pointers; this one has neither. For
   each, say exactly which line of the CAS loop (1134-1172) would have to
   become a multi-word atomic, and why no CAS can provide it.
2. Why acquire/release rather than `SeqCst` on the links? Name the specific
   reorder prevented at line 383, and say what `SeqCst` would add that nothing
   here consumes.
3. Line 1152 is `NoBarrier_SetNext` — a relaxed store — inside a lock-free
   insert. Explain why it is safe, then construct the variant that *would* be
   a bug (hint: move the relaxed store after line 1153).
4. Redo Step 7's arithmetic for your own workload: at branching factor 4 and
   your `write_buffer_size`, how many levels does log₄(entries) want, and how
   much headroom does `kMaxHeight_ = 12` leave? At what memtable size does 12
   stop being enough?
5. Estimate the dependent misses per lookup at 10⁶ entries and compare against
   this repo's measured `hashmap 8.8 ns` and `btreemap 26.6 ns` (topic 0
   `lookup_shootout`). Where does the skiplist still win, and what would you
   have to measure to show it?

## Takeaway

Two ideas carry the file. The layout idea: put the tower *before* the node and
the key *after* it, so the hot pair — level-0 link and key bytes — share an
allocation and the cold tower is out of the way; the height does not even need
a field, because arriving at level *h* proves *h* is valid. The concurrency
idea: level 0 contains every node, so links can be made bottom-up with
independent single-word CASes, and a partially linked node is slow, never
wrong. Both are cheap only because the workload never deletes — one
restriction that removes hazard pointers, epochs, and the redis features
(spans, backward links) that need multi-word atomicity. When a lock-free
structure looks suspiciously small, look for the restriction paying for it.

## Done when

Answer each before unfolding it.

- [ ] Given a `Node*` and a height of 3, name the byte offsets of `next_[0]`,
      `next_[-2]`, and the first key byte, relative to the raw allocation.

<details>
<summary>Answer</summary>

From `AllocateNode` (858-869): `prefix = sizeof(Atomic<Node*>) * (height - 1)`
= 8 × 2 = 16 bytes, and `Node* x = raw + prefix`, so the `Node` starts at
offset 16. `next_[0]` is at offset **16** (it is the first and only declared
member, line 420). `next_[-2]` — level 2 — is 2 words *earlier*: offset
16 − 16 = **0**, the very start of the allocation. The key is `&next_[1]`
(line 374), one word past `next_[0]`: offset **24**. Level 1 sits at offset 8.

</details>

- [ ] Why can duplicate keys be detected by checking level 0 alone (lines
      1137-1147), and what would break if the check ran at every level?

<details>
<summary>Answer</summary>

Level 0 is the only level that contains every node — upper levels are a random
subset — so if a duplicate exists anywhere, it is on level 0. Running the
check at every level would not be *incorrect*, just wasteful: three extra key
comparisons per height-4 insert, each a potential cache miss on
`splice->next_[i]->Key()`. Worse, an upper level could miss a duplicate that
level 0 would catch, so the check would still be needed at level 0 — the extra
work buys nothing. The comment at line 1137 says exactly this: "Checking for
duplicate keys on the level 0 is sufficient."

</details>

- [ ] A reader is walking level 0 while a writer is midway through the loop at
      line 1135, having linked its node at level 0 but not yet at level 2.
      What does the reader observe, and is it correct?

<details>
<summary>Answer</summary>

The reader finds the new node — level 0 is linked, and the `acq_rel` CAS at
1153 paired with the acquire `Load` at line 383 guarantees the node's key
bytes are visible to it. A *different* reader descending from level 2 will
step past the new node at that level and land on it after descending to level
0 or 1. So the node is findable by every search, just via a slightly longer
path until the upper links land. Partial linking costs latency, never
correctness — that is the property that makes per-level CAS legal.

</details>

- [ ] Invariant (1) says nodes are never deleted. Name the concrete machinery
      that invariant removes, and the concrete feature it costs.

<details>
<summary>Answer</summary>

Removed: safe memory reclamation — hazard pointers, epoch-based reclamation,
or RCU, plus the reclamation thread and the per-read overhead of announcing a
hazard. Without deletes there is never an unlinked-but-still-referenced node,
so freeing is a single arena teardown (`db/memtable.h:914`) when the frozen
memtable is dropped. Cost: no in-place delete (a user `Delete` becomes a
tombstone *insert*, which the compaction layer must later resolve), and no
spans or backward pointers, since both need several words updated as one step.

</details>

- [ ] The header claims the layout saves one pointer per node "despite the
      padding". Show the inequality, and turn it into bytes for a 64 MiB
      memtable of 100-byte entries.

<details>
<summary>Answer</summary>

Saving is exactly `sizeof(void*)` = 8 bytes (the eliminated key pointer);
padding is 0 to `sizeof(void*) - 1` = 0 to 7 bytes (header lines 15-17). Since
7 < 8, the net is strictly positive for every key length — that is the "always
less than `SkipList<const char*>`" claim at lines 17-18. With uniform key
lengths the expected padding is 3.5 bytes, so the expected net saving is
8 − 3.5 = 4.5 bytes/node. A 64 MiB memtable of 100-byte entries holds
67,108,864 / 100 = 671,089 nodes, so the saving is 671,089 × 4.5 = 3,019,899
bytes = 2.88 MiB, i.e. **4.5% more user data per flush**.

</details>

## References

**Code**

- [rocksdb](https://github.com/facebook/rocksdb) at `7c80a5a` — verify with
  `tools/pinned-source.py ref rocksdb`.

| File | Lines | What |
|------|-------|------|
| `memtable/inlineskiplist.h` | 10-38 | header: layout rationale, thread safety, both invariants |
| `memtable/inlineskiplist.h` | 352-420 | `Node` — negative tower index, inline key, stashed height |
| `memtable/inlineskiplist.h` | 558-573 | `RandomHeight` — p = 1/4 coin, capped at `kMaxHeight_` |
| `memtable/inlineskiplist.h` | 853-880 | `AllocateKey` / `AllocateNode` — one allocation, three regions |
| `memtable/inlineskiplist.h` | 907-920 | `Insert` vs `InsertConcurrently` |
| `memtable/inlineskiplist.h` | 1134-1172 | the CAS path — bottom-up, level-0 duplicate check, narrow retry |
| `memtable/inlineskiplist.h` | 1173-1199 | the non-CAS path — the contrast case |
| `util/atomic.h` | 104-121 | release `Store`, acquire `Load`, `acq_rel` `CasStrong` |
| `memory/concurrent_arena.h` | 35-41, 57-68 | spinlock-guarded bump arena with lazy per-core shards |
| `db/memtable.h` | 914 | `ConcurrentArena arena_;` — the memtable owns one |
| `memtable/skiplistrep.cc` | 17 | `SkipListRep : public MemTableRep` — the plug-in point |
| `include/rocksdb/options.h` | 175-191, 1421-1429 | 64 MiB memtable; concurrent writes on by default |

Siblings in `memtable/`: `hash_skiplist_rep.cc`, `hash_linklist_rep.cc`,
`vectorrep.cc` — three other RUM positions for the same interface.

**Measured in this repo**

- `topics/00-performance-toolbox/notes.md`, `lookup_shootout` at n = 10⁶:
  `hashmap 8.8 ns`, `btreemap 26.6 ns`, `vec_binary_search 25.8 ns` per
  lookup — the ordered/unordered gap this design pays for concurrency.
- `topics/00-performance-toolbox/notes.md`, cache ladder: ~1 ns L1 / ~5 ns L2
  / ~100 ns DRAM — the prices behind "~30 dependent hops".
- `topics/02-in-memory-structures/notes.md` (FINDINGS row 2): the
  `rehash_spike` lane, `p50 = 42 ns` against `max = 58.4 ms`. A skiplist has
  no rehash, so it has no equivalent tail — worth remembering when the median
  says the hash table wins.

**Companion chapters**

- [`reading-redis-skiplist.md`](reading-redis-skiplist.md) — the same
  structure single-threaded, with spans and backward pointers. Read the two
  side by side: every feature redis has and RocksDB lacks is a multi-word
  update.
- [`reading-hashbrown.md`](reading-hashbrown.md) — the unordered alternative
  ruled out in Step 2, and why it wins point lookups.
