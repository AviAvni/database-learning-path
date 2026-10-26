# redis dict: rehashing 100M keys without stopping the world

A hash table serving 100K ops/s cannot stop the world to rehash 100M entries
— the resulting p99.9 spike would be a service outage. This chapter walks
redis's answer, the topic's first industrial latency fix: keep **two tables**
and migrate one bucket at a time, piggybacked on normal operations. Before
opening `dict.c`, it builds the machine step by step — what a chained table
is, why it must resize, why the naive resize is an outage, and how the
two-table dance fixes it — then hands you the line anchors to watch each
piece in the source. It is also the design you'll replicate in this topic's
experiment.

Every anchor below is redis `unstable` at commit `e87dda2a4` (2026-10-26),
the revision this repo pins, quoted with the line numbers the code occupies
there. Re-check any of them yourself with
`tools/pinned-source.py show redis src/dict.c -r 406:437`.

## The problem in one sentence

Doubling a hash table is O(n) work done inside *one* insert — and this repo
has measured what that feels like: hashbrown's `rehash_spike` lane inserts
10 M keys one at a time and reports p50 **42 ns** with a max of **58.4 ms**
([FINDINGS.md](../../FINDINGS.md) row 2), a 1.4-millionfold spread inside a
single operation type, on a table 12× smaller than the 100M-key case redis
has to survive.

## The concepts, step by step

### Step 1 — a chained hash table: buckets of linked lists

> **In:** nothing yet — this step fixes the vocabulary and the cost model
> every later step reasons with.
> **Out:** the structure redis actually implements, and the one number
> (chain length) that decides what a lookup costs. Step 2 turns that number
> into the reason the table must grow.

A hash table stores key→value pairs so that lookup costs ~constant time: run
the key through a **hash function** (a function mapping any key to a
well-scrambled fixed-size integer), keep the low bits as an index into an
array of **buckets** (the fixed-size slot array; each slot holds the head of
whatever landed there), and put the entry there. Two keys landing in the same
bucket is a **collision**; **chaining** resolves it by making each bucket a
linked list of entries:

```
 buckets            entries (each malloc'd separately)
 ┌───┐
 │ ●─┼───► [k,v,next ●]───► [k,v,next ∅]     ← a 2-long chain
 ├───┤
 │ ∅ │                                        ← empty bucket
 ├───┤
 │ ●─┼───► [k,v,next ∅]
 └───┘
 lookup = hash key → pick bucket → walk the chain comparing keys
```

The cache cost (topic 0): every hop down a chain is a **dependent load** —
the address of the next node is only known once the current node has arrived,
so nothing can prefetch it and nothing can overlap two hops. Topic 0's
`cache_ladder` measured that ladder at ~1 ns (L1) / ~5 ns (L2) / ~100 ns
(DRAM) on this machine ([FINDINGS.md](../../FINDINGS.md) row 0), so a chain
hop that misses to DRAM costs ~100 ns and a chain of three costs ~300 ns that
no amount of instruction-level parallelism can hide. Chains must stay short.

### Step 2 — load factor: why the table must grow

> **In:** the chained table from Step 1.
> **Out:** the growth trigger (`ht_used[0] >= size`), and the arithmetic that
> says what a lookup costs at each load factor. Step 3 prices the growth
> itself.

The **load factor** — written α — is entries ÷ buckets: α = n/m for n entries
in m buckets. With a hash function that spreads keys evenly, the expected
number of entries examined by a *successful* lookup in a chained table is

```
E[entries examined] = 1 + α/2

  symbols:  α = n/m, the load factor
            n = entries stored          m = buckets allocated
  reading:  you always examine the one you find (the 1), plus on average
            half of the others sharing its bucket (α/2)
```

Worked on redis's two thresholds, with the ~100 ns dependent-miss cost from
Step 1 and one extra miss for the bucket array itself:

```
α = 1.0   (the normal grow trigger, dict.c:1679)
    entries examined = 1 + 1.0/2 = 1.5
    misses           = 1 (bucket array) + 1.5 (chain) = 2.5
    cold cost        = 2.5 × 100 ns = 250 ns

α = 4.0   (dict_force_resize_ratio, dict.c:46 — the ceiling redis tolerates
           while a fork child is alive, Step 7)
    entries examined = 1 + 4.0/2 = 3.0
    misses           = 1 + 3.0 = 4.0
    cold cost        = 4.0 × 100 ns = 400 ns

α = 10.0  (an un-resizable table left to degrade)
    entries examined = 1 + 5.0 = 6.0
    misses           = 7.0        cold cost = 700 ns
```

So the penalty is linear in α, not catastrophic — chaining degrades
gracefully, which is exactly why redis can afford to let α reach 4 during a
fork. But 700 ns against 250 ns is still a 2.8× lookup regression, and the
only fix is more buckets: allocate a bigger array (redis doubles — sizes are
powers of two, stored as *exponents* in `ht_size_exp`, so "size" is
`1 << exp`) and move every entry to its new bucket. Moving is mandatory
because the bucket index is `hash & (size − 1)`: change the size and half the
entries belong somewhere else. That move is the **rehash**.

### Step 3 — the stop-the-world rehash is a latency outage

> **In:** the obligation to move all n entries, from Step 2.
> **Out:** the cost of doing it inside one operation, in seconds, on two
> different per-entry assumptions — the number Step 4's design exists to
> avoid.

The textbook rehash happens inside whichever insert crosses the threshold:
that one operation allocates the new array and moves all n entries before
returning. **Tail latency** — the slowest percentiles, p99.9 and max, the
numbers a server actually promises its clients — is what this destroys, while
throughput barely notices, because the O(n) is amortized over the n inserts
that preceded it.

Put a number on it. This repo's `rehash_spike` lane gives a *measured*
per-entry cost for the friendliest possible case — hashbrown's flat,
malloc-free array, swept linearly. Assume the 58.4 ms maximum
([FINDINGS.md](../../FINDINGS.md) row 2, eighth decile) is the doubling that
happens when the table crosses 2²³ = 8,388,608 buckets, which at hashbrown's
7/8 load rule holds 7,340,032 live entries when it fires:

```
measured spike           58.4 ms = 58,400,000 ns
entries moved            2²³ × 7/8 = 7,340,032
per entry                58,400,000 / 7,340,032 = 7.96 ns

redis's dict at 100M entries, same 7.96 ns/entry (optimistic — this assumes
chained nodes sweep as cheaply as a flat array, which they do not):
                         100,000,000 × 7.96 ns  = 0.796 s

redis's dict at 100M entries, one dependent DRAM miss per chained node
(Step 1's ~100 ns, the honest number for malloc'd chain nodes):
                         100,000,000 × 100 ns   = 10.0 s
```

Both are outages. Almost every insert costs ~100 ns; this one costs between
0.8 and 10 seconds. A redis instance frozen for even the optimistic 0.8 s has
blown through every sane health-check timeout; at 10 s it has dropped every
client. The fix cannot be "rehash faster" — a 12× speedup still leaves an
0.8 s stall. It must be "never do all the work in one operation."

### Step 4 — the fix: two tables and a migration cursor

> **In:** the outage from Step 3, and the load-factor trigger from Step 2.
> **Out:** the five fields of `struct dict` that make a half-migrated table a
> legal state — the state Steps 5 and 6 operate on.

Redis keeps **both** the old and new bucket arrays alive during the resize
and migrates gradually. The whole design is visible in one struct:

```c
// src/dict.h — struct dict, 162-178 (the whole design; every field matters)
   162  struct dict {
   163      dictType *type;
   164
   165      dictEntry **ht_table[2];
   166      unsigned long ht_used[2];
   167
   168      long rehashidx; /* rehashing not in progress if rehashidx == -1 */
   169
   170      /* Note: pauserehash is a full unsigned so iterator increments
   171       * don't perform RMW on the same storage unit as other bitfields. */
   172      unsigned pauserehash; /* If >0 rehashing is paused */
   173
   174      /* Keep small vars at end for optimal (minimal) struct padding */
   175      signed char ht_size_exp[2]; /* exponent of size. (size = 1<<exp) */
   176      int16_t pauseAutoResize;  /* If >0 automatic resizing is disallowed (<0 indicates coding error) */
   177      void *metadata[];
   178  };
```

The line to look at is 168. `rehashidx` is a cursor sweeping ht[0] from
bucket 0 upward: every bucket *below* it has already moved to ht[1], every
bucket at or above it has not, and `-1` means no migration is in progress at
all. Line 165 is the pair of bucket arrays (ht[0] = old, ht[1] = new during a
rehash), 166 their live counts, 175 their sizes as exponents. Line 172's
`pauserehash` is the "hold still, someone is iterating me" brake (Step 8).

Every normal operation nudges the cursor forward:

```mermaid
flowchart LR
    OP["any dictAddRaw / dictFind<br/>dict.c:531 / dict.c:804"] --> HOOK["_dictRehashStepIfNeeded(d, idx)<br/>dict.c:1733"]
    HOOK -- "the bucket you are<br/>already touching" --> BR["_dictBucketRehash(d, idx)<br/>dict.c:476 — cache-friendly"]
    HOOK -- "otherwise" --> RH["dictRehash(d, 1)<br/>dict.c:406 — one bucket at the cursor"]
    BR --> DONE{"ht_used[0] == 0?"}
    RH --> DONE
    DONE -- yes --> SWAP["dictCheckRehashingCompleted<br/>dict.c:381 — free ht[0],<br/>ht[1] becomes ht[0], rehashidx = -1"]
    DONE -- no --> OP
```

Note the fork at the hook — the detail a "one bucket at the cursor" summary
loses: if the bucket your operation is *already* going to touch still lives
in ht[0], redis migrates *that* bucket (dict.c:1737-1740) rather than the one
under the cursor, because that memory is about to be in cache anyway. Only
when the visited bucket is already migrated or empty does it fall back to
`dictRehash(d,1)` at the cursor (dict.c:1744).

The O(n) rehash still happens — but as n tiny installments, each attached to
an operation that was paying a hash-table visit anyway.

### Step 5 — one migration step, and why its work is bounded

> **In:** the half-migrated two-table state from Step 4.
> **Out:** a hard bound on the work one operation can be charged, in buckets
> — the tail-latency guarantee that replaces Step 3's 0.8-to-10-second stall.

A step moves one *bucket*: walk its chain, re-hash every entry into ht[1].
The subtle hazard is a **sparse** old table — if most buckets are empty,
"move one bucket" could scan thousands of empty slots looking for a non-empty
one, silently breaking the bounded-work guarantee. Redis caps that scan:

```c
// src/dict.c — dictRehash, 406-437 (the whole bounded-work loop)
   406  int dictRehash(dict *d, int n) {
   407      int empty_visits = n*10; /* Max number of empty buckets to visit. */
   // ... 408-422: the DICT_RESIZE_FORBID / DICT_RESIZE_AVOID gates of Step 7 ...
   423      while(n-- && d->ht_used[0] != 0) {
   // ... 424-426: assert rehashidx is still inside ht[0] ...
   427          while(d->ht_table[0][d->rehashidx] == NULL) {
   428              d->rehashidx++;
   429              if (--empty_visits == 0) return 1;
   430          }
   431          /* Move all the keys in this bucket from the old to the new hash HT */
   432          rehashEntriesInBucketAtIndex(d, d->rehashidx);
   433          d->rehashidx++;
   434      }
   435
   436      return !dictCheckRehashingCompleted(d);
   437  }
```

Line 429 is the one that carries the guarantee: after ten fruitless bucket
loads the function gives up and returns 1 ("still rehashing"), having done
bounded work. Line 407 sets the budget at `n*10` for a request of n buckets,
so the single-bucket call every operation makes (`dictRehash(d,1)`,
dict.c:472 and dict.c:1744) can touch at most ten empty buckets plus one
chain.

The actual moving is one level down, and it is worth reading because of what
it does *not* do:

```c
// src/dict.c — rehashEntriesInBucketAtIndex, 337-353 and 372-377
   337  static void rehashEntriesInBucketAtIndex(dict *d, uint64_t idx) {
   338      dictEntry *de = d->ht_table[0][idx];
   // ... 339-340: locals ...
   341      while (de) {
   342          nextde = dictGetNext(de);
   343          void *storedKey = dictGetKey(de);
   344          /* Get the index in the new hash table */
   345          if (d->ht_size_exp[1] > d->ht_size_exp[0]) {
   346              const void *key = dictStoredKey2Key(d, storedKey);
   347              h = dictGetHash(d, key) & DICTHT_SIZE_MASK(d->ht_size_exp[1]);
   348          } else {
   349              /* We're shrinking the table. The tables sizes are powers of
   350               * two, so we simply mask the bucket index in the larger table
   351               * to get the bucket index in the smaller table. */
   352              h = idx & DICTHT_SIZE_MASK(d->ht_size_exp[1]);
   353          }
   // ... 354-371: the no_value key-inlining cases; all end at ht_table[1][h] ...
   372          d->ht_table[1][h] = de;
   373          d->ht_used[0]--;
   374          d->ht_used[1]++;
   375          de = nextde;
   376      }
   377      d->ht_table[0][idx] = NULL;
```

Line 372 is the move: the entry is *relinked*, not copied — chaining's one
structural gift, since the payload never changes address. Note the asymmetry
at 345-353: growing re-hashes the key (347, and that recomputation is why the
per-entry cost is nearer Step 3's 100 ns than its 8 ns), while **shrinking
just masks the old index** (352), because a smaller power-of-two mask is a
prefix of a larger one. Line 377 empties the source bucket, which is what
makes the cursor's "everything below me has moved" invariant true.

The machine, distilled to the shape you will re-implement:

```rust
// ILLUSTRATION — not quoted from redis. The real loop is dict.c:406-437 and
// the per-bucket move is dict.c:337-378; this is the same algorithm with the
// entry-encoding cases (dict.c:354-371) removed.
fn rehash_step(d: &mut Dict, mut buckets: usize) {
    let mut empty_visits = buckets * 10;         // dict.c:407
    while buckets > 0 && d.used[0] > 0 {
        while d.ht[0].bucket(d.rehashidx).is_empty() {
            d.rehashidx += 1;
            empty_visits -= 1;
            if empty_visits == 0 { return; }     // dict.c:429 — the bound
        }
        for entry in d.ht[0].take_bucket(d.rehashidx) {
            let idx = entry.hash & d.mask[1];    // dict.c:347 — NEW table only
            d.ht[1].push_bucket(idx, entry);
        }
        d.rehashidx += 1;
        buckets -= 1;
    }
    if d.used[0] == 0 { d.swap_tables(); d.rehashidx = -1; }   // dict.c:381-395
}
```

Now price the guarantee against Step 3's stall, at α = 1.0 (so a non-empty
chain holds ~1.5 entries, Step 2):

```
worst case for one operation = 10 empty bucket loads + 1 chain of ~1.5 entries
                             ≈ 11.5 dependent misses × 100 ns ≈ 1.15 µs

against the one-shot rehash's measured 58.4 ms:
                             58,400,000 ns / 1,150 ns ≈ 50,800× smaller
```

The bill does not disappear, it is *spread*. Migrating a 2²³-bucket table one
bucket per operation needs 8,388,608 operations to finish; at the 100K ops/s
of the opening sentence that is 8,388,608 / 100,000 = **83.9 seconds** during
which the dict is in the two-table state and every lookup pays Step 6's tax.
That is the trade: a 58 ms cliff becomes 84 seconds of a slightly slower
table.

### Step 6 — correctness during the migration: who pays the tax

> **In:** the partially migrated table Step 5 leaves behind, with `rehashidx`
> somewhere in the middle of ht[0].
> **Out:** the two rules — where reads look, where writes land — that make
> that state safe, and what each costs.

**Rule 1: a read may have to check both tables.** The key you want could
legitimately be in either. The lookup path is `dictFindLinkInternal`, and the
loop is more careful than "check ht[0], then ht[1]":

```c
// src/dict.c — inside dictFindLinkInternal, 782-800
   782      /* Rehash the hash table if needed */
   783      _dictRehashStepIfNeeded(d,idx);
   784
   785      int tables = (dictIsRehashing(d)) ? 2 : 1;
   786      for (table = 0; table < tables; table++) {
   787          if (table == 0 && (long)idx < d->rehashidx) continue;
   788          idx = hash & DICTHT_SIZE_MASK(d->ht_size_exp[table]);
   789
   790          link = &(d->ht_table[table][idx]);
   791          if (bucket) *bucket = link;
   792          while(link && *link) {
   // ... 793-798: compare the stored key, walk to the next link ...
   799          }
   800      }
```

Line 787 is the line to focus on, and it corrects the simple story: ht[0] is
skipped entirely when the target bucket sits *below* the cursor, because
everything below the cursor has already been migrated (Step 5's invariant, at
dict.c:377). So the read tax is not "always two lookups" — it is two lookups
only for keys whose ht[0] bucket the cursor has not yet reached, and it
shrinks to zero as the migration advances. Line 785 is the other half: when
`rehashidx == -1` the loop runs once and there is no tax at all.

**Rule 2: a new key goes only into ht[1].** This is not an optimization, it
is a correctness requirement:

```c
// src/dict.c — inside dictInsertKeyAtLink, 550-554
   550      /* If rehashing is ongoing, we insert in table 1, otherwise in table 0.
   551       * Assert that the provided bucket is the right table. */
   552      int htidx = dictIsRehashing(d) ? 1 : 0;
   553      assert(bucket >= &d->ht_table[htidx][0] &&
   554             bucket <= &d->ht_table[htidx][DICTHT_SIZE_MASK(d->ht_size_exp[htidx])]);
```

Line 552 decides it, and `dictFindLinkForInsert` hands over a bucket in the
same table (dict.c:1794). Why a bug and not merely waste: if a new entry
landed in an ht[0] bucket the cursor had already passed, nothing would ever
migrate it — `rehashidx` only moves forward — and `dictCheckRehashingCompleted`
frees ht[0] wholesale at dict.c:387 the moment `ht_used[0]` hits zero. The
key would be silently lost. (`ht_used[0]` would also never be decremented for
it, so in practice the dict would instead never *finish* rehashing; either
way the invariant "below the cursor, ht[0] is empty forever" is what the rule
protects.)

When ht[0] empties, `dictCheckRehashingCompleted` (dict.c:381-395) frees it,
copies ht[1] into slot 0, and sets `rehashidx = -1`. Cost model for the whole
scheme: O(n) total rehash work, amortized O(1) per operation, and no single
operation ever stalls for more than one chain plus ten empty visits.

### Step 7 — the resize policy, and a durability interaction

> **In:** the load-factor trigger from Step 2 and the migration machine from
> Steps 4-6.
> **Out:** the three-state global policy that decides *when* a migration is
> allowed to start at all — and the reason a persistence mechanism gets a
> vote on a data-structure parameter.

The growth decision lives in one function:

```c
// src/dict.c — inside dictExpandIfNeeded, 1672-1687
  1672      /* If we reached the 1:1 ratio, and we are allowed to resize the hash
  1673       * table (global setting) or we should avoid it but the ratio between
  1674       * elements/buckets is over the "safe" threshold, we resize doubling
  1675       * the number of buckets. */
  1676      dictResizeEnable can_resize;
  1677      atomicGet(dict_can_resize, can_resize);
  1678      if ((can_resize == DICT_RESIZE_ENABLE &&
  1679           d->ht_used[0] >= DICTHT_SIZE(d->ht_size_exp[0])) ||
  1680          (can_resize != DICT_RESIZE_FORBID &&
  1681           d->ht_used[0] >= dict_force_resize_ratio * DICTHT_SIZE(d->ht_size_exp[0])))
  1682      {
  1683          if (dictTypeResizeAllowed(d, d->ht_used[0] + 1))
  1684              dictExpand(d, d->ht_used[0] + 1);
  1685          return DICT_OK;
  1686      }
  1687      return DICT_ERR;
```

Line 1679 is the normal trigger: α ≥ 1.0. Line 1681 is the escape hatch:
even when resizing is discouraged, α ≥ `dict_force_resize_ratio` (= 4,
dict.c:46) forces it anyway — Step 2's arithmetic says that is a 400 ns
lookup against 250 ns, a degradation redis will tolerate but not exceed.
Lines 1676-1677 take one snapshot of the global policy into a local before
both tests read it; the global itself is declared `redisAtomic` (dict.c:45)
and written only through `atomicSet` in `dictSetResizeEnabled`.

What sets `dict_can_resize` is the interesting part, and it is not in dict.c
at all:

```c
// src/server.c — updateDictResizePolicy, 835-842
   835  void updateDictResizePolicy(void) {
   836      if (server.in_fork_child != CHILD_TYPE_NONE)
   837          dictSetResizeEnabled(DICT_RESIZE_FORBID);
   838      else if (hasActiveChildProcess())
   839          dictSetResizeEnabled(DICT_RESIZE_AVOID);
   840      else
   841          dictSetResizeEnabled(DICT_RESIZE_ENABLE);
   842  }
```

Three states, not two — the usual summary is that redis "disables resizing
during BGSAVE", and line 839 says otherwise. **Copy-on-write** is
the mechanism behind it: `fork()` gives the child a logical copy of the
parent's memory by sharing the physical pages read-only, and the kernel
copies a page only when one side writes to it. A rehash writes to nearly
every page holding entries, so a resize during a background save can force a
copy of most of the dataset — the parent's RSS balloons toward 2× while the
child writes an RDB file.

So: line 837, inside the forked child itself, resizing is **forbidden**
outright (the child is a snapshot; there is nothing to gain). Line 839, in
the *parent* while any child runs, it is **avoided** — meaning the α ≥ 1.0
trigger at dict.c:1679 is switched off but the α ≥ 4 force at 1681 stays
live, and `dictRehash` refuses to advance a migration that would not have
qualified under the same rule (dict.c:416-421). A durability mechanism tuning a
data-structure knob, with a bounded degradation (Step 2: 250 ns → 400 ns)
chosen as the price. Worth pausing on.

### Step 8 — iterating a table that rehashes under you: dictScan

> **In:** everything above — a table that may be half-migrated *and* may
> change size between two calls of the iterator.
> **Out:** the reverse-binary cursor and the exact guarantee it buys
> (every key present throughout is returned at least once; duplicates are
> possible).

SCAN must iterate the keyspace across many separate calls, holding no state
between them but a single integer cursor, while buckets migrate and the table
may double or halve in between. Redis's answer, designed by Pieter Noordhuis
and explained in the comment at dict.c:1458-1541, is to increment the cursor
in **reversed bit order** — reverse the bits, add one, reverse back:

```c
// src/dict.c — inside dictScanDefrag, the non-rehashing branch, 1598-1611
  1598      if (!dictIsRehashing(d)) {
  1599          htidx0 = 0;
  1600          m0 = DICTHT_SIZE_MASK(d->ht_size_exp[htidx0]);
  1601          dictScanDefragBucket(d, fn, defragfns, privdata, &d->ht_table[htidx0][v & m0]);
  1602
  1603          /* Set unmasked bits so incrementing the reversed cursor
  1604           * operates on the masked bits */
  1605          v |= ~m0;
  1606
  1607          /* Increment the reverse cursor */
  1608          v = rev(v);
  1609          v++;
  1610          v = rev(v);
  1611
```

Lines 1608-1610 are the whole trick (`rev` itself is dict.c:1448-1456). The
property that makes it work: because a bucket index is the hash's *low* bits,
the entries of bucket `b` at size 2ⁿ split across exactly buckets `b` and
`b + 2ⁿ` at size 2ⁿ⁺¹ — and a reverse-binary increment visits `b` and
`b + 2ⁿ` adjacently, so a bucket already visited at one size maps onto
already-visited buckets at the next.

Work the four-bit case the comment describes (mask 1111, size 16), counting
in reverse-binary order:

```
normal counting:   0000 0001 0010 0011 0100 …   (low bit varies fastest)
reverse counting:  0000 1000 0100 1100 0010 …   (HIGH bit varies fastest)

grow 16 → 64 after visiting 1100:
  the keys of bucket 1100 are now in 001100, 011100, 101100, 111100
  reverse counting from 1100 never re-emits a cursor ending in 1100,
  because those two low bits are the LAST ones it will vary
  ⇒ already-scanned work is never repeated, and nothing is skipped
```

The rehashing branch (dict.c:1612-1640) reduces the two-table case to the
one-table case: scan the smaller table's bucket, then every bucket of the
larger table that is an expansion of it (1629-1639). And line 1596 explains
`pauserehash`'s other job — the scan pauses rehashing across the callback, in
case the callback itself calls `dictFind` and moves buckets underneath the
iterator.

Guarantee, stated at dict.c:1467-1469: every element present in the dict for
the whole scan is returned **at least once**; some may be returned more than
once. Read the full comment (1458-1541) — one of the great comments in open
source.

## Where each step lives in the code

`src/dict.c` is 2410 lines at `e87dda2a4`; you need about 400 of them.

| Lines | What | Step |
|-------|------|------|
| `dict.h:162-178` | `struct dict` — two tables, `rehashidx`, `pauserehash`, sizes as exponents | 4 |
| `dict.h:212-213` | `DICT_HT_INITIAL_EXP` = 2, so a fresh table has 4 buckets | 2 |
| `dict.h:233-235` | `dictPauseRehashing` / `dictResumeRehashing` / `dictIsRehashingPaused` | 8 |
| `dict.c:45-46` | `dict_can_resize` (a `redisAtomic`), `dict_force_resize_ratio` = 4 | 7 |
| `dict.c:337-378` | `rehashEntriesInBucketAtIndex` — the actual chain relink; re-hash on grow (347), mask on shrink (352) | 5 |
| `dict.c:381-395` | `dictCheckRehashingCompleted` — free ht[0], promote ht[1], `rehashidx = -1` | 5, 6 |
| `dict.c:406-437` | `dictRehash` — the bounded loop; `empty_visits = n*10` (407), the bail-out (429) | 5 |
| `dict.c:449-461` | `dictRehashMicroseconds` — the *other* client, a time-budgeted rehash from the server cron | 5 |
| `dict.c:471-473` | `_dictRehashStep` — `dictRehash(d,1)`, skipped while paused | 4 |
| `dict.c:476-495` | `_dictBucketRehash` — migrate the bucket you are already touching | 4 |
| `dict.c:531-541` | `dictAddRaw` — the insert path | 6 |
| `dict.c:547-554` | `dictInsertKeyAtLink` — `htidx = rehashing ? 1 : 0`, the write rule | 6 |
| `dict.c:618-622` | `dictAddOrFind` | 6 |
| `dict.c:766-802` | `dictFindLinkInternal` — the two-table read, and the skip at 787 | 6 |
| `dict.c:804-808` | `dictFind` | 6 |
| `dict.c:1173-1181`, `1197-1221` | safe iterators: pause on first `dictNext` (1203), resume on reset (1177) | 8 |
| `dict.c:1448-1456` | `rev()` — the bit-reversal itself | 8 |
| `dict.c:1458-1541` | the `dictScan` comment — read it in full | 8 |
| `dict.c:1542-1548`, `1584-1645` | `dictScan` → `dictScanDefrag`; reverse increments at 1608-1610 and 1634-1636 | 8 |
| `dict.c:1662-1688` | `dictExpandIfNeeded` — α ≥ 1.0 (1679), forced α ≥ 4 (1681) | 7 |
| `dict.c:1733-1746` | `_dictRehashStepIfNeeded` — the piggyback hook and its bucket/cursor fork | 4 |
| `dict.c:1761-1796` | `dictFindLinkForInsert` — same two-table walk, returns an ht[1] bucket (1794) | 6 |
| `server.c:835-842` | `updateDictResizePolicy` — FORBID / AVOID / ENABLE | 7 |

Suggested route: the struct (`dict.h:162`) → `dictRehash` (406) →
`rehashEntriesInBucketAtIndex` (337) → the hook (1733) → the two payers,
`dictFindLinkInternal` (766) and `dictInsertKeyAtLink` (547) → the policy
(1662, then `server.c:835`) → `dictScan`'s comment (1458) last, on its own.

**Contrast case**: valkey's *client-side* dict at
`deps/libvalkey/src/dict.c:103-150` (valkey `668d875ca`) — `dictExpand` there
allocates the new table and moves every entry in one `for` loop (123-143),
asserts the old table is empty (144), frees it (145) and swaps (148). No
`rehashidx`, no second table, no cursor: the entire Step 4 machine is absent.
That is the right call for a client library's small maps and unacceptable for
a server's keyspace — same structure, different RUM position, because latency
requirements are part of the workload.

## Questions to answer in notes.md

1. During rehash, `dictInsertKeyAtLink` (dict.c:552) inserts only into ht[1].
   Why is inserting into ht[0] a correctness bug, not just a wasted move?
   Trace what `dictCheckRehashingCompleted` (dict.c:381-395) would do to that
   entry.
2. What does `pauserehash` exist for? Find its two users (dict.c:1203 and
   dict.c:1596) and say what breaks in each if the brake is removed.
3. Redis caps `empty_visits` at `n*10` (dict.c:407). What tail-latency
   guarantee does that give one operation, in buckets touched — and redo
   Step 5's ~1.15 µs bound for a table that is being *shrunk* rather than
   grown, where chains are longer and empty buckets rarer.
4. Line 787 skips ht[0] when `idx < rehashidx`. Sketch the read tax over the
   life of a migration: what fraction of lookups touch two tables when the
   cursor is 10% / 50% / 90% through ht[0]?
5. `dictRehashMicroseconds` (dict.c:449) rehashes on a *time* budget instead
   of a bucket budget, from the server's cron. Which of the two budgets would
   you give your own implementation, and what does the other one get wrong?

## Takeaway

The two-table dict is a latency structure, not a throughput structure: it
does strictly *more* total work than a stop-the-world rehash (two-table
lookups, a cursor, ten-empty-bucket scans) in exchange for never letting one
operation pay more than a bounded slice of it. That is the shape of almost
every fix in this curriculum — trade mean for max — and you are about to
build it in `experiments/src/incremental_map.rs`.

## Done when

Answer each before unfolding it.

- [ ] You can say what `rehashidx` means, including what `-1` means and what is true of every bucket below it.

  <details><summary>Answer</summary>

  `rehashidx` (dict.h:168) is the index of the next bucket of ht[0] to
  migrate. `-1` means no migration is in progress, which is what
  `dictIsRehashing` tests and what makes the read path a single-table walk
  (dict.c:785).

  The invariant is that every bucket of ht[0] strictly below `rehashidx` is
  empty and will stay empty: `rehashEntriesInBucketAtIndex` sets
  `d->ht_table[0][idx] = NULL` at dict.c:377 after relinking the chain, the
  cursor only ever moves forward (dict.c:428, 433), and new keys are never
  written into ht[0] while rehashing (dict.c:552). That invariant is exactly
  what licenses the read-path shortcut at dict.c:787 — if `idx < rehashidx`,
  looking in ht[0] cannot find anything.

  </details>

- [ ] You can explain why inserting a new key into ht[0] during a migration is a correctness bug rather than a wasted move.

  <details><summary>Answer</summary>

  Because the cursor never goes back. If the new entry lands in an ht[0]
  bucket below `rehashidx`, no future `dictRehash` step will visit that
  bucket — the `while` loop at dict.c:423-434 starts from `d->rehashidx` and
  only increments — so the entry is never moved into ht[1].

  What happens next depends on the bookkeeping. `dictCheckRehashingCompleted`
  (dict.c:381-395) fires when `ht_used[0]` reaches 0 and calls
  `zfree(d->ht_table[0])` at dict.c:387, taking the orphaned entry's bucket
  with it; if the insert also bumped `ht_used[0]`, the counter never reaches 0
  and the dict simply never finishes rehashing, holding both tables forever.
  Either outcome is a bug, which is why dict.c:552 makes the table choice a
  single unconditional expression and dict.c:553-554 asserts the caller handed
  over a bucket from that table.

  </details>

- [ ] You can state the bounded-work guarantee one operation gets, in buckets, and price it against the one-shot rehash this repo measured.

  <details><summary>Answer</summary>

  One operation triggers at most `dictRehash(d,1)`, which is `empty_visits =
  1*10` (dict.c:407) plus one non-empty bucket: **at most ten empty bucket
  loads and one chain**. Line 429 returns as soon as the tenth empty visit is
  spent, so the work is bounded even on a table that is 99.9% empty.

  Priced with topic 0's ~100 ns dependent DRAM miss and a ~1.5-entry chain at
  α = 1.0, that is about 11.5 × 100 ns ≈ 1.15 µs worst case. The one-shot
  alternative was measured in this repo at **58.4 ms**
  ([FINDINGS.md](../../FINDINGS.md) row 2) for a 7.34 M-entry table — about
  50,800× larger, and that was hashbrown's flat array, which sweeps at 7.96 ns
  per entry rather than chasing malloc'd chain nodes.

  </details>

- [ ] You can say what a read costs while a migration is in flight, and why "it always checks both tables" is not quite right.

  <details><summary>Answer</summary>

  `dictFindLinkInternal` sets `tables = 2` only while rehashing
  (dict.c:785), and then line 787 skips ht[0] whenever the key's ht[0] bucket
  index is below `rehashidx`, because Step 5's invariant guarantees that
  bucket is empty. So the tax applies only to keys whose old bucket the cursor
  has not reached yet, and it falls linearly as the cursor advances: roughly
  90% of lookups pay it when the cursor is 10% through, and roughly 10% when
  it is 90% through.

  The cost when it does apply is one extra bucket-array load plus that
  bucket's chain — Step 2's arithmetic makes it about 250 ns rather than
  125 ns for the second probe at α = 1.0. Cheap per operation, and paid for
  the whole 83.9 seconds it takes 100K ops/s to walk a 2²³-bucket table one
  bucket at a time.

  </details>

- [ ] You can explain why a fork for BGSAVE changes the resize policy, and what the parent is still allowed to do.

  <details><summary>Answer</summary>

  `fork()` shares the parent's pages with the child copy-on-write, so a page
  is duplicated only when someone writes it. A rehash relinks nearly every
  entry (dict.c:372) and therefore writes nearly every page holding entries,
  which would force the kernel to copy most of the dataset and roughly double
  resident memory while the child is writing its RDB.

  `updateDictResizePolicy` (server.c:835-842) therefore has three states, not
  two. Inside the forked child, `DICT_RESIZE_FORBID` (837) — no resizing at
  all. In the parent while any child runs, `DICT_RESIZE_AVOID` (839), which
  switches off the α ≥ 1.0 trigger at dict.c:1679 but leaves the forced grow
  at dict.c:1681 live, so a table whose load factor reaches
  `dict_force_resize_ratio` = 4 still expands. `dictRehash` applies the same
  test before advancing an in-flight migration (dict.c:416-421). The bounded
  price of that tolerance is Step 2's arithmetic: 3.0 chain entries examined
  instead of 1.5, roughly 400 ns instead of 250 ns per cold lookup.

  </details>

- [ ] You can implement the two-table scheme from memory — which is exactly what `experiments/src/incremental_map.rs` asks for.

  <details><summary>Answer</summary>

  There is no answer to unfold: the implementation is the exercise. The bar,
  in the order the code needs it — two bucket arrays and a cursor
  (dict.h:162-178); a migration step that moves one bucket and gives up after
  ten empty ones (dict.c:406-437); a read that consults ht[1] and consults
  ht[0] only when the cursor has not passed the key's old bucket
  (dict.c:785-787); a write that lands in ht[1] whenever a migration is in
  flight (dict.c:552); and a completion check that frees the old table,
  promotes the new one and resets the cursor to −1 (dict.c:381-395).

  The measurement that says you got it right is in
  [notes.md](notes.md): hashbrown's row is p50 42 ns / max 58.4 ms. Yours
  should keep the p50 within a few nanoseconds of that and move the max into
  microseconds. If your max is still in milliseconds, the usual cause is a
  step that migrates a *chain* rather than a *bucket*, or an `empty_visits`
  cap that was never wired up.

  </details>

## References

**Code**
- [redis](https://github.com/redis/redis) `src/dict.c` (2410 lines),
  `src/dict.h` (326 lines), `src/server.c` — pinned at `unstable` /
  `e87dda2a4` (2026-10-26).

| File | Lines | What |
|------|-------|------|
| `src/dict.h` | 162-178 | `struct dict` — the two-table state |
| `src/dict.h` | 233-235 | the `pauserehash` macros |
| `src/dict.c` | 46 | `dict_force_resize_ratio = 4` |
| `src/dict.c` | 337-378 | `rehashEntriesInBucketAtIndex` — one bucket moved |
| `src/dict.c` | 381-395 | completion: free ht[0], promote ht[1] |
| `src/dict.c` | 406-437 | `dictRehash` — the bounded step |
| `src/dict.c` | 429 | the `empty_visits` bail-out — the tail-latency guarantee |
| `src/dict.c` | 552 | `htidx = rehashing ? 1 : 0` — the write rule |
| `src/dict.c` | 787 | the read that skips an already-migrated ht[0] bucket |
| `src/dict.c` | 1458-1541 | the `dictScan` design comment |
| `src/dict.c` | 1608-1610 | the reverse-binary cursor increment |
| `src/dict.c` | 1679, 1681 | α ≥ 1.0, and the forced grow at α ≥ 4 |
| `src/dict.c` | 1733-1746 | the piggyback hook, with its bucket/cursor fork |
| `src/server.c` | 835-842 | FORBID / AVOID / ENABLE, decided by fork state |

- [valkey](https://github.com/valkey-io/valkey) (`668d875ca`)
  `deps/libvalkey/src/dict.c:103-150` — the single-table, full-rehash
  contrast case: one loop, every entry, inside one call.

**Measured in this repo**
- [FINDINGS.md](../../FINDINGS.md) row 2 — hashbrown insert p50 42 ns, max
  58.4 ms, the stop-the-world rehash this whole design avoids.
- [FINDINGS.md](../../FINDINGS.md) row 0 — the ~1 / 5 / 100 ns cache ladder
  every cost estimate above is priced with.
- [notes.md](notes.md) — the per-decile maxima, showing the spikes land
  exactly where the table crossed a power of two.
