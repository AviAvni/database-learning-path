# Bw-tree vs OLC: why lock-free lost to optimistic latches

Three papers, one arc: the most radical lock-free index ever shipped
(the Bw-tree, ICDE '13), the paper that measured it honestly (SIGMOD '18),
and the modest protocol that won (optimistic lock coupling). Before the
papers, this chapter builds each design from its first principle — what
latches cost, what CAS can and cannot do, and how the memory hierarchy
prices the alternatives. The arc is this topic's thesis in miniature — the
memory hierarchy, not elegance, decides which concurrency scheme survives.

This is a papers chapter, so the standard is different: **download the PDFs
and check the section numbers.** Every figure below is followed by the
section or table it came from, and where the two papers disagree the
disagreement is the point. The OLC protocol has real pinned source —
`leanstore/leanstore@90fcf18`, `backend/leanstore/sync-primitives/Latch.hpp`
— so Step 8 reads code, not pseudo-code.

## The problem in one sentence

Every thread traversing a latched B-tree *writes* the root's latch — even
pure readers — so the root's cache line ping-pongs between all cores at
**38.3 ns per transfer on this machine** and read throughput stops scaling
right when core counts explode; the Bw-tree bet everything on eliminating
latches, and five years later an honest benchmark found it
**under-performing lock-based indexes by 1.5–4.5×** (Wang & Pavlo et al.,
SIGMOD '18, §1) while requiring "an order of magnitude more code" than a
B-tree with optimistic lock coupling (Leis et al., IEEE Data Eng. Bull.
2019, §3.2).

## The concepts, step by step

### Step 1 — the enemy: latch traffic on hot cache lines

> **In:** a B-tree, N cores, and a read-only workload.
> **Out:** why *readers* are the problem, and the measured cost of the
> line they fight over.

A **latch** is a short-duration lock protecting a data structure's
*physical* integrity — nanoseconds, held across one node access — unlike
topic 8's transaction locks, which protect *logical* content for seconds.
The Bw-tree paper's follow-up states the convention outright: "in this
paper, we always use the term 'lock' when referring to 'latch'" (SIGMOD
'18, footnote 1, p. 1). Read "latch-free" and "lock-free" as synonyms
throughout.

The classic B-tree protocol, **latch coupling**, acquires the parent's
latch, then the child's, then releases the parent — correct, deadlock-free
by ordering, and a scaling disaster. Leis et al. name the mechanism
precisely (§3): "lock acquisition and release require writing to the shared
memory location that implements the lock. This write causes exclusive
ownership of the underlying cache line and invalidates copies of it on all
other processor cores… the lock of the root node becomes a point of
physical contention — even in read-only workloads and even when read/write
locks are used."

A **cache line** is the unit the memory system moves and owns. The
**coherence protocol** (MESI: Modified / Exclusive / Shared / Invalid) says
a line may be Modified on at most one core at a time, so writing it
requires invalidating every other copy. That invalidation is the cost, and
this topic measured it rather than quoting folklore:

```
  false_sharing lane, 8 threads × 5M increments on their OWN counters:
    packed  (all 8 in one line)  202.7 ms →  40.54 ns per increment
    pad128  (own line each)       11.4 ms →   2.28 ns per increment
    ───────────────────────────────────────────────────────────────
    one cache-line ownership transfer =  40.54 − 2.28  =  38.3 ns
```

38.3 nanoseconds, not "~100 cycles" — the cycle figure is machine-specific
folklore and this machine's real number is roughly 4× larger than it. Now
apply it to a 4-level tree: latch-coupling a lookup takes and releases 4
latches, ≈8 writes to shared words. If each is contended, 8 × 38.3 ns =
**306 ns of pure coherence traffic** for a lookup whose useful work is a
handful of binary searches.

Leis et al. isolate exactly this in their Table 4 (a 100M-key B-tree, 10
threads, per-operation counters):

| lookup, 10 threads | Mop/s | cycles | instructions | L1 misses |
|---|---|---|---|---|
| no synchronization | 15.48 | 2058 | 283 | 38.6 |
| optimistic lock coupling | 14.60 | 2187 | 370 | 43.8 |
| traditional lock coupling | 5.71 | **5591** | 379 | 54.2 |

Read the middle two columns together: lock coupling and OLC execute
**almost the same number of instructions** (379 vs 370) and lock coupling
burns **2.6× the cycles** (5591 vs 2187). The work is identical; the
difference is entirely the memory system, and it shows up as 10.4 extra L1
misses per lookup — about 2.6 per latch acquire/release pair on a
four-level tree. At 20 threads, "lookup with OLC is 3.9× faster than
traditional lock coupling" (§4).

Both designs below are answers to exactly this. They differ in how much of
the latch they remove: all of it (Bw-tree) or just the reader's writes
(OLC).

### Step 2 — the lock-free toolkit: CAS, and its one-pointer limit

> **In:** the wish to remove latches entirely.
> **Out:** what CAS can do, what "lock-free" does and does not promise,
> and the one-word constraint that dictates the whole Bw-tree design.

**CAS** (compare-and-swap) is the atomic CPU instruction "replace this
64-bit word with a new value only if it still equals the value I read". The
ICDE '13 paper defines it in a footnote on first use (footnote 1, §II.C),
which is the right instinct.

**Lock-free** is a *progress* guarantee, not a description of instructions:
some thread always makes progress in a bounded number of steps, whatever
any other thread does — including being descheduled mid-operation.
**Wait-free** is the stronger property that *every* thread finishes in a
bounded number of its own steps. A CAS retry loop is lock-free but not
wait-free: one unlucky thread can lose every race indefinitely while the
structure races ahead. This distinction is the whole argument of Step 7 —
lock-freedom promises the *system* progresses, not that *your* insert does.

The catch that shapes everything: **CAS swaps ONE word.** A B-tree update
often touches several — modify a node in place, or split one node and
update its parent (two nodes, two pointers). So a lock-free B-tree must
recast every multi-word operation as a chain of single-pointer
publications. That is precisely the Bw-tree's design, and the source of
both its elegance and its downfall.

The related hazard, named once: the **ABA problem**. A thread reads pointer
`A`, is descheduled; another frees `A`, allocates a new node at the same
address, links it in; the first thread's CAS against `A` *succeeds* — value
unchanged, meaning changed. Every scheme in this chapter avoids it by
delaying reuse (epochs), not by detecting it.

### Step 3 — the mapping table: indirection makes every change one CAS

> **In:** the one-word CAS limit from Step 2.
> **Out:** the Bw-tree's first move, and the second-order costs it buys.

The Bw-tree's first move: nodes are identified by logical **PIDs** (page
ids), and a central **mapping table** maps PID → pointer to the node's
current in-memory representation. All inter-node links store PIDs, never
raw pointers. The paper (ICDE '13, §II.B) puts it this way: "We use PIDs in
the Bw-tree to link the nodes of the tree. For instance, all downward
'search' pointers between Bw-tree nodes are PIDs, not physical pointers…
The mapping table severs the connection between physical location and
inter-node links."

Now "change node P17" = CAS the single mapping-table slot for P17 — one
word, exactly what CAS can do — and no parent or sibling ever needs
updating when a node's physical location changes. §II.B calls this "relocation" tolerance, and notes that it "directly
enables both delta updating of the node in main memory and log structuring
of our stable storage". (Wu/Pavlo's "logical pointers" verdict from topic 8
— same lesson: indirection decouples updaters.)

**What it costs, measured.** Every node access is now two dependent loads
instead of one: read the PID out of the parent, then read the mapping table
to get the pointer. SIGMOD '18 removed the indirection to price it (§6.3,
"Disabling the Mapping Table"): "Read performance increases by 18% due to
fewer cache misses for load instructions (L1: 32% lower; L3: 52% lower)."
Half the L3 misses come from the indirection alone.

### Step 4 — delta chains: updates without touching the node

> **In:** a mapping table whose slot can be CASed.
> **Out:** the delta-chain representation, and the read cost it creates.

Second move: never modify a node in place. An update allocates a small
**delta record** — a heap object describing one change, "insert k₁" or
"delete k₂" — that points at the node's current representation, and CASes
the mapping-table slot to point at the delta, prepending to a chain:

```
 mapping table: PID ─► pointer          update = CAS the PID slot:
 ┌─────┐                                   Δ(insert k) ──┐
 │ P17 ├──► Δ(delete k₂) ─► Δ(insert k₁) ─► base node    │
 └─────┘        newest ◄──────────────── oldest          │
 CAS(P17, old_head, Δnew) — ONE atomic pointer swap per update,
 no in-place writes, no latches anywhere.
```

Readers reconstruct the node by walking the chain down to the **base node**,
applying deltas as they go. When a chain grows too long, **consolidation**
folds it into a fresh base node — published, again, by one CAS — and the
old chain is retired. This pattern, **install-and-consolidate**, is the
whole idiom: writers install cheap increments, and someone occasionally
pays to fold them back into a compact form.

Reclamation of replaced deltas and nodes uses **epoch-based reclamation** —
the crossbeam-epoch guide's scheme, and ICDE '13 §II.C cites the same
lineage ("We use a form of epoch to accomplish safe garbage collection").
You know why it is needed: a reader may still be walking the old chain.

The 2013 paper argues this *helps* the cache (§II.A): "Avoiding
update-in-place reduces CPU cache invalidation, resulting in higher cache
hit ratios. Reducing cache misses increases the instructions executed per
cycle." Hold that claim; Step 7 measures it.

The cost is already visible if you have internalised topic 0 §2: a K-delta
chain turns one node read into K *dependent* pointer chases, each a
potential DRAM miss. And K is not small in practice. SIGMOD '18 Table 2,
Insert-only with 20 threads, measures the **average leaf delta chain length
at 11.38** on the Rand-Int workload (0 on Mono-Int, 0.34 on the
high-contention one). Eleven dependent misses to read one node.

### Step 5 — SMOs: multi-node changes as cooperative state machines

> **In:** a split, which must change a child, a new sibling, and a parent.
> **Out:** the half-split protocol, the helping rule, and why "just wait
> for the owner" is not available.

Splits and merges — **SMOs**, structure modification operations — touch two
nodes and a parent, but CAS publishes one word. ICDE '13 §II.D states the
problem and the fix: "we cannot install a page split with a single CAS…
To deal with this problem, we break an SMO into a sequence of atomic
actions, each installable via a CAS. We use a B-link design to make this
easier. With a side link in each page, we can decompose a node split into
two 'half split' atomic actions."

A half-split first posts a split-delta on the child, so readers route
around it via the side link; a separate CAS then installs the new separator
in the parent. Between the two, the tree is in a valid-but-incomplete
state, and any thread that stumbles on a partial SMO must **help complete
it** before proceeding — §II.D: "In order to make sure that no thread has
to wait for an SMO to complete, a thread that sees a partial SMO will
complete it before proceeding with its own operation."

Read that as a *consequence*, not a design flourish. Waiting for the owner
would reintroduce blocking, and blocking is what lock-freedom means you
gave up the right to do: the owner may have been descheduled by the OS for
a full time slice, so a waiter would be blocked by a thread that is not
running. Helping is the only option once you have committed to
lock-freedom — and it means every thread must contain a correct
implementation of every other thread's half-finished operation. Latched
critical sections become cooperative state machines: correct, and brutally
hard to write, test, and tune. (Question 2.)

### Step 6 — read the 2013 evidence on its own terms

> **In:** the ICDE '13 performance section (§VI).
> **Out:** what was actually measured, against what, on what — before you
> read what happened next.

Rule six of this repo's reading standard says report the negative result.
Here the negative result *is* the story, so the honest way to tell it is to
state the 2013 claims precisely first, and then let the follow-up land.

**The headline numbers (§VI.C, Fig. 6):** Bw-tree 10.4 M ops/s against
BerkeleyDB's 555 K on the Xbox LIVE workload — an **18.7× speedup**; 8.6×
on the deduplication trace; 5.8× on the synthetic workload. Against a
latch-free skip list (§VI.D, Table II): 3.83 vs 1.02 M ops/s on synthetic
(**3.7×**) and 5.71 vs 1.30 read-only (**4.4×**). Cache efficiency (§VI.E,
Fig. 7): "Almost 90% of its memory reads come from either the L1 or L2
cache, compared to 75% for the skip list."

**Now read the setup (§VI.A), which is where the claims live or die.**

- The baseline is **BerkeleyDB in B-tree mode, non-transactional**, using
  "page-level latching (the lowest latch granularity in BerkeleyDB)". A
  disk-oriented storage engine with whole-page latches is not a
  state-of-the-art in-memory index; it is topic 0's fair-benchmarking
  pitfall — an unoptimised baseline, and an apples-to-oranges one.
- The machine is an **Intel Xeon W3550, four cores hyperthreaded to eight**,
  and "we use 8 worker threads for each workload". A design whose entire
  premise is that core counts are exploding was never evaluated above four
  physical cores.
- The implementation is "approximately 10,000 lines of C++ code" (§VI.A) —
  a number worth remembering when you reach the code-size comparison.

**And read the retry data (§VI.B.2, Table I), because it is the premise
Step 7 overturns.**

| workload | failed splits | failed consolidates | failed updates |
|---|---|---|---|
| Dedup | 0.25% | 1.19% | 0.0013% |
| Xbox | 1.27% | 0.22% | 0.0171% |
| Synthetic | 8.88% | 7.35% | 0.0003% |

The paper's own reading: "The record update failure rate… is extremely low,
below 0.02% for all workloads… we believe these rates are still
manageable." **On the evidence presented, that is correct.** Work the
arithmetic: expected CAS attempts is `E[attempts] = 1/(1 − p)`, so at
p = 0.0002 you get 1.0002 attempts — one retry per five thousand updates.
Retries genuinely are free here.

The defect is not the arithmetic. It is that the three workloads never
contained the case where p is large — and Step 7 found it.

### Step 7 — the reality check: SIGMOD '18 measures it honestly

> **In:** the 2013 design, reimplemented by people who did not write it.
> **Out:** which premises survived, which did not, and the section number
> for each.

CMU rebuilt the design as **OpenBw-Tree**, because "the source code has not
been released" and the original description was "missing important details"
(§1). Their machine: two Intel Xeon E5-2680 v2, 10 cores × 2 HT each, 128
GB, threads pinned to one socket unless stated (§5). Workloads: YCSB A/C/E
over 52M keys with Zipfian skew (§5.1). Their own optimisations bought
1.1–2.5× over a good-faith reimplementation of the original (§1) — so this
is the *charitable* version of the design.

**Finding 1 — delta chains cost more than they save (§6.3).** They disabled
features one at a time: "by eliminating delta chains for the Read-only
workload, the performance increases by **23%**. If we backport this
modification to the original Bw-Tree, the performance improvement will
even be greater (**45%**)." Replacing delta updates with in-place updates
raises Insert-only throughput **40%**. That is 2013's §II.A claim —
avoiding update-in-place preserves caches — measured and reversed.

**Finding 2 — the mapping table costs 18% of reads (§6.3).** Quoted in Step
3: L1 misses 32% lower, L3 52% lower without it.

**Finding 3 — and this one is the deepest.** Disabling CAS "neither
Insert-only nor Read-only operations become significantly faster. This
seems to contradict the common belief that atomic operations like CaS
usually takes more cycles on some ISAs. Our experiments, however, pins the
worker thread on a single core, and therefore, the CPU can perform the CaS
locally, requiring almost no cache coherence overhead."

Read that twice, because it is this topic's whole thesis stated by
accident: **the atomic instruction is not the cost; the contended line
is.** This topic's own lanes say the same thing in nanoseconds — an atomic
RMW on a line you own is 2.28 ns; the same instruction on a line another
core owns is 40.54 ns. It also means the §6.3 decomposition, being
single-threaded and pinned, *cannot* measure coherence at all — a limitation
the authors state rather than hide.

**Finding 4 — lock-freedom loses hardest exactly where it was supposed to
win (§6.2).** They built a high-contention workload: every thread appends
monotonically increasing keys, the commonest OLTP insert pattern there is.
"OpenBw-Tree suffers from an extremely high abort rate as threads contend
for the head of the Delta Chain. Table 2 shows that the abort rate is over
1000%, i.e., on average there are more than 10 aborts for every insert."
And: "all lock-free indexes struggled more than any lock-based indexes; for
example, the SkipList failed to make progress in this high-contention
workload." Under contention the winners are Masstree, then ART, then the
OLC B+Tree — all lock-based.

Now finish the arithmetic Step 6 started. Table 2's exact abort rates are
1.05% (Mono-Int), 1.44% (Rand-Int) and **1078.63%** (Mono-HC):

```
  E[attempts] = 1/(1 − p)             p = failure probability per attempt
  ICDE'13 record updates, p = 0.0002 → 1.0002 attempts   (1 retry / 5,000)
  SIGMOD'18 Rand-Int,     p = 0.0142 → 1.0144 attempts
  SIGMOD'18 Mono-HC:  10.7863 aborts/insert ⇒ 11.79 attempts
                      ⇒ p = 1 − 1/11.79 = 0.915
```

From p = 2 × 10⁻⁴ to p = 0.915 — a **5,900× increase in attempts per
insert** produced by nothing but a change of key distribution. That is the
lesson: lock-free retry cost is not a property of the algorithm, it is a
property of the workload's contention on one word, and the 2013 evaluation
had no workload that exercised it. Both papers are honest; only one of them
tested the case.

**Finding 5 — the residue (§6.3, closing).** "Overall, after disabling
these lock-free features, the Bw-Tree is still **15%–19% slower** than the
B+Tree with OLC synchronization. We conjecture that the simplicity of OLC
upper bounds the number of instructions for every operation, while for the
Bw-Tree, even Read-only operations perform considerable bookkeeping."

**The verdict, stated exactly (§1 and §6.1).** §1: "the overhead of the
Bw-Tree's indirection layer and delta records causes it to under-perform
the lock-based indexes by **1.5–4.5×**." §6.1 breaks that down: "ART is more
than 4× faster than the OpenBw-Tree for point lookups (though ART is slower
on Scan/Insert)… The OpenBw-Tree is also slower than the Masstree and the
B+Tree, often by a factor of **∼2×**." So the 4× belongs to ART, not to the
OLC B+Tree, and against the B+Tree the honest figure is about 2×. §8
concludes: "lock-freedom does not always pay off in comparison with modern
lock-based synchronization techniques."

The code-size claim is from a different paper and should be cited there:
Leis et al. §3.2 — "OpenBw-Tree, an open source implementation of the
Bw-tree, requires an order of magnitude more code than a B-tree based on
OLC" (with both implementations named in footnote 5). Alongside ICDE '13's
own "approximately 10,000 lines of C++" (§VI.A), that is the substantiated
version of "≈10× simpler". "Lock-free" bought worse constants, not
scalability — and a great deal more code.

### Step 8 — OLC: the modest protocol that won

> **In:** Step 1's diagnosis — the problem is the *reader's write*.
> **Out:** the protocol that removes only that, read from LeanStore's
> shipped implementation.

**Optimistic lock coupling** keeps the latch but makes readers stop writing
it. Leis et al. §3.1 gives the whole protocol in six lines. A read-only
node access: (1) read the lock version, restart if the lock is not free;
(2) access the node; (3) read the version again and validate it has not
changed. A write: (1) acquire the lock, waiting if necessary; (2) write;
(3) increment the version and unlock.

LeanStore ships this as `HybridLatch`, and it is pinned, so read the real
thing (this is topic 6's HybridLatch, now with the concurrency):

```cpp
// backend/leanstore/sync-primitives/Latch.hpp:21-43 — the latch itself
    21  constexpr static u64 LATCH_EXCLUSIVE_BIT = 1ull;
    24  using VersionType = atomic<u64>;
    25  struct alignas(64) HybridLatch {
    26     VersionType version;
    27     std::shared_mutex mutex;
    41     bool isExclusivelyLatched() { return (version & LATCH_EXCLUSIVE_BIT) == LATCH_EXCLUSIVE_BIT; }
    42  };
    43  static_assert(sizeof(HybridLatch) == 64, "");
```

The lock bit is **the low bit of the version counter**, which makes the
whole protocol arithmetic. Acquiring adds 1 (setting the bit, `:161`,
`:147`); releasing adds 1 again (clearing it and bumping the version,
`:96-97`); so every completed write advances the version by exactly 2 and
an odd version means "locked". A reader that sees the same even version
before and after knows no writer touched the node in between.

```cpp
// backend/leanstore/sync-primitives/Latch.hpp:84-115 — validate, release, and the optimistic entry
    84     void recheck()
    85     {
    87        assert(state == GUARD_STATE::OPTIMISTIC || version == latch->ref().load());
    88        if (state == GUARD_STATE::OPTIMISTIC && version != latch->ref().load()) {
    89           jumpmu::jump();
    90        }
    91     }
    93     inline void unlock()
    94     {
    95        if (state == GUARD_STATE::EXCLUSIVE) {
    96           version += LATCH_EXCLUSIVE_BIT;
    97           latch->ref().store(version, std::memory_order_release);
    98           latch->mutex.unlock();
   106     inline void toOptimisticSpin()
   109        version = latch->ref().load();
   110        if ((version & LATCH_EXCLUSIVE_BIT) == LATCH_EXCLUSIVE_BIT) {
   112           do {
   113              version = latch->ref().load();
   114           } while ((version & LATCH_EXCLUSIVE_BIT) == LATCH_EXCLUSIVE_BIT);
   115        }
```

Note what the reader never does: **write shared memory.** `toOptimisticSpin`
(`:106-117`) and `recheck` (`:84-91`) are plain loads only, so the root's
cache line stays Shared in every core's L1 and Step 1's enemy is dead —
with a plain B+tree's memory layout intact. The spin at `:112-114` is
test-and-test-and-set again: spin on ordinary reads, never on the atomic
(the LWLock guide, Step 4, has the same loop in C).

"Coupling" survives as *validation order*: validate the parent's version
AFTER reading the child pointer — the pair (read child ptr, revalidate
parent) replaces "hold parent latch while grabbing child". `recheck()` at
`:84` is that call, and `jumpmu::jump()` at `:89` is LeanStore's restart:
a longjmp back to a registered restart point, i.e. the whole traversal
begins again.

Three pieces of fine print, all of which people forget:

- **Torn reads of freed memory must be survivable.** A speculatively-read
  pointer may be garbage, so dereferencing it can segfault; Leis §3.3 shows
  the extra validation (Figure 2, line 25) that prevents this, and §3.4
  confirms node reclamation still needs "epoch-based reclamation, hazard
  pointers, or optimized hazard pointers" — the crossbeam-epoch guide's
  subject, once more.
- **OLC can fall back; lock-free cannot.** §3.4: restarts can be capped and
  the operation can drop to pessimistic locking "in cases of very heavy
  contention. The ability to fall back to traditional locking is a major
  advantage of OLC in terms of robustness over lock-free approaches, which
  do not have this option." Compare Step 7's 1078% abort rate, which had
  nowhere to fall back to. LeanStore has this: `toOptimisticOrShared`
  (`:128-140`) and `toOptimisticOrExclusive` (`:141-154`) take the real
  `std::shared_mutex` when the version says the node is contended.
- **The alignment is a bug on this machine.** `alignas(64)` and the
  `static_assert(sizeof(HybridLatch) == 64)` at `:25`/`:43` pad each latch
  to one *64-byte* line — the textbook advice. This topic's `false_sharing`
  lane measures 64-byte padding at **1.8× slower** than 128-byte padding on
  Apple M-series (20.4 ms vs 11.4 ms), because M-series cores prefetch
  lines in 128-byte pairs, so two latches 64 B apart still travel together.
  Postgres uses 128 (`pg_config_manual.h:217`) and crossbeam's
  `CachePadded` is `repr(align(128))` on aarch64 *and* x86-64
  (`crossbeam-utils/src/cache_padded.rs:70-77`). LeanStore was written for
  server x86; on an M3 you would change this constant and measure again.

The arc, in one line: indirection + deltas (Bw) lost to versions +
restarts (OLC) because the memory hierarchy prices dependent pointer
chases higher than optimistic retries — and because a design that cannot
fall back has no answer when its retry probability goes to 0.9.

## How to read the papers (with the concepts in hand)

Read in arc order — design, autopsy, winner. Budget ~3 h. Download all
three; do not read summaries of them.

1. **Levandoski, Lomet, Sengupta, ICDE '13** — Steps 3–5 in the authors'
   words: architecture and the mapping table (§II.B), delta updating
   (§II.C), SMOs (§II.D, then §IV for the full protocols), in-memory
   latch-free pages and GC (§III). Skim §V (the log-structured store — half
   the original motivation was flash, which is easy to forget when the
   design is discussed as a pure in-memory index). **Then read §VI.A before
   §VI.C**, so you know what the 18.7× is against.
2. **Wang, Pavlo et al., SIGMOD '18** — the autopsy, Steps 6–7. §3 lists
   what the original description left out; §5.1 the workloads; §6.1 the
   head-to-head; §6.2 high contention (Table 2 is the retry data); **§6.3
   is the component decomposition** — the bill of costs, and the section to
   study. §8 for the conclusion in their own words.
3. **Leis, Haubenschild, Neumann, IEEE Data Eng. Bull. 2019** — short, ten
   pages. §3.1 is the protocol; §3.2 has the code-size comparison; §3.3 the
   dangling-pointer correctness argument; §3.4 the fallback and reclamation
   fine print; §4 Table 4 and Figure 3 for the numbers used in Step 1. Map
   every rule onto `Latch.hpp` as you go.

Where the papers disagree, note which one measured. §II.A of ICDE '13
argues delta records *improve* cache behaviour; §6.3 of SIGMOD '18 measures
+23%/+45% read throughput from removing them. Both statements are made in
good faith; only one is an experiment.

## Questions for notes.md

1. A Bw-tree point-read with a 6-delta chain: count likely cache misses
   vs an OLC B+tree of the same size (use your topic-0 numbers).
2. Why must helpers complete OTHER threads' SMOs? What deadlock/livelock
   does "just wait for the owner" reintroduce?
3. OLC readers restart on any concurrent write to a node on their path.
   Estimate restart probability for a 4-level tree under 1% node-write
   rate — why is it negligible? When isn't it (hot leaf)?
4. Delta chains ARE topic 20's delta matrices (pending updates folded on
   read, consolidated lazily). Why does the trade favor deltas for
   sparse matrices when it condemned them for B-tree nodes? (Hint:
   amortization unit — one row read vs one mxm over millions.)
5. M9/M13: FalkorDB's matrices already sit behind a "mapping table"
   (label → matrix pointer). Which Bw-tree lesson transfers: CAS the
   matrix pointer for CoW publication? Which does NOT (delta chains per
   node)?

## Done when

You can argue both sides — why Bw-tree looked inevitable in 2013 and why
OLC won by 2018 — with the cache-line-level reasons, not slogans.
Answer each before unfolding it.

- [ ] Give the 2018 verdict as a number, and say who it is against.
  <details><summary>Answer</summary>

  **1.5–4.5×**, and it is against "the lock-based indexes" *plural* — §1 of
  Wang & Pavlo et al.: "the overhead of the Bw-Tree's indirection layer and
  delta records causes it to under-perform the lock-based indexes by
  1.5–4.5×."

  Do not attribute the top of that range to the OLC B+Tree. §6.1 splits it:
  "ART is more than 4× faster than the OpenBw-Tree for point lookups
  (though ART is slower on Scan/Insert)… The OpenBw-Tree is also slower
  than the Masstree and the B+Tree, often by a factor of ∼2×." Against the
  OLC B+Tree specifically the honest figure is about **2×**. The
  "order of magnitude more code" claim is from a different paper entirely —
  Leis et al. §3.2, footnote 5.

  </details>

- [ ] Both papers measured CAS failure rates. Give both, convert each to
  expected attempts per operation, and explain the gap.
  <details><summary>Answer</summary>

  ICDE '13 Table I (§VI.B.2): record-update failures **below 0.02%** on all
  three workloads (splits and consolidates 0.22–8.88%). SIGMOD '18 Table 2
  (§6.2): 1.05% Mono-Int, 1.44% Rand-Int, and **1078.63%** on the
  high-contention Mono-HC workload — "more than 10 aborts for every
  insert".

  With `E[attempts] = 1/(1 − p)`: p = 0.0002 gives **1.0002** attempts, one
  retry per 5,000 inserts; 10.7863 aborts per insert means 11.79 attempts,
  i.e. p = 1 − 1/11.79 = **0.915**. A **5,900×** change, caused by nothing
  but the key distribution — Mono-HC has every thread appending
  monotonically increasing keys, so all of them CAS the same delta-chain
  head. The 2013 paper's arithmetic was right; its three workloads simply
  never contained the case. Contention is a property of the workload's
  concentration on one word, not of the algorithm.

  </details>

- [ ] SIGMOD '18 found that disabling CAS changed nothing. Why, and why is
  that the most important sentence in §6.3?
  <details><summary>Answer</summary>

  Because the experiment "pins the worker thread on a single core, and
  therefore, the CPU can perform the CaS locally, requiring almost no cache
  coherence overhead" (§6.3, "Disabling CaS"). A CAS on a line you already
  own is nearly free.

  It matters because it separates the two things people conflate. The
  *instruction* is cheap: this topic's `false_sharing` lane measures an
  uncontended atomic RMW at **2.28 ns**. The *contended line* is not: the
  same instruction on a line another core owns costs **40.54 ns**, a 38.3 ns
  ownership transfer. Every optimisation in this chapter — padding, pinning
  once per operation, readers that never write — targets the line, not the
  instruction. It also means the whole §6.3 decomposition, being
  single-threaded and pinned, cannot measure coherence effects at all;
  the authors say so rather than letting the reader assume otherwise.

  </details>

- [ ] What did the 2013 Bw-tree get compared against, on what machine, and
  why does that matter?
  <details><summary>Answer</summary>

  §VI.A: **BerkeleyDB in B-tree mode, non-transactional, with page-level
  latching** ("the lowest latch granularity in BerkeleyDB"), plus a
  latch-free skip list. The machine is an **Intel Xeon W3550 — four cores,
  hyperthreaded to eight** — and all workloads use 8 worker threads.

  It matters twice. First, a disk-oriented engine holding whole-page
  latches is not a state-of-the-art in-memory index; the 18.7× on the Xbox
  workload (§VI.C, Fig. 6) is measured against an unoptimised, differently-
  shaped baseline — topic 0's fair-benchmarking pitfalls in one sentence.
  Second, a design justified by exploding core counts was never tested
  above four physical cores; SIGMOD '18 ran 20 and 40 threads (§5), which
  is where §6.2's 1078% abort rate appeared.

  </details>

- [ ] An OLC reader traverses four nodes. How many shared cache lines does
  it write, and what is the measured consequence?
  <details><summary>Answer</summary>

  **Zero.** `toOptimisticSpin` (`Latch.hpp:106-117`) and `recheck`
  (`:84-91`) contain only `latch->ref().load()` — plain atomic loads. The
  latch lines stay in the Shared state in every core's L1.

  Leis et al. Table 4 measures the consequence at 10 threads: lock coupling
  and OLC execute nearly identical instruction counts (379 vs 370) but lock
  coupling burns **5591 cycles against OLC's 2187** — 2.6× — and 54.2 L1
  misses against 43.8. The work is the same; the difference is entirely the
  coherence traffic of writing four latches twice each. At 20 threads OLC
  is **3.9× faster** (§4). Priced with this topic's constant, 8 contended
  writes × 38.3 ns ≈ 306 ns of pure interconnect per lookup.

  </details>

- [ ] LeanStore's `HybridLatch` is `alignas(64)`. Is that right on the
  machine you are reading this on?
  <details><summary>Answer</summary>

  **No.** `Latch.hpp:25` and the `static_assert` at `:43` pin each latch to
  a 64-byte line, which is the textbook rule and correct for the x86
  servers LeanStore targets.

  On Apple M-series it leaves money on the table. This topic's
  `false_sharing` lane: 8 counters packed = 202.7 ms, padded to 64 B = 20.4
  ms, padded to 128 B = **11.4 ms**. So 64-byte padding recovers 9.9× of
  the available 17.8× and is still **1.8× slower** than 128 — M-series cores
  prefetch adjacent lines in 128-byte pairs, so two latches 64 B apart are
  dragged around together. Postgres already uses 128
  (`pg_config_manual.h:217`, with the reasoning at `:208-215`) and
  crossbeam's `CachePadded` is `repr(align(128))` on aarch64 *and* x86-64
  (`crossbeam-utils/src/cache_padded.rs:70-77`). "Pad to a cache line" is
  not the rule; "pad to 128 bytes, then measure" is.

  </details>

- [ ] Why must a thread that finds a half-finished SMO complete it rather
  than wait?
  <details><summary>Answer</summary>

  Because the owner may not be running. Lock-freedom's guarantee is that
  *some* thread progresses regardless of what any other thread does —
  including being descheduled by the OS mid-SMO. A waiter would then be
  blocked on a thread that will not run for a full time slice, which is
  precisely the blocking the design exists to eliminate. ICDE '13 §II.D:
  "In order to make sure that no thread has to wait for an SMO to complete,
  a thread that sees a partial SMO will complete it before proceeding with
  its own operation."

  The price is that every thread must contain a correct implementation of
  every other thread's half-finished operation, for every SMO, in every
  intermediate state. That is a large part of what Leis et al. mean by "an
  order of magnitude more code" (§3.2), and what SIGMOD '18's §3 ("Missing
  Components") had to reconstruct from a paper that did not state it.

  </details>

## References

**Papers** — download all three; the section numbers below are checked
against the PDFs.

| Paper | Sections | What is in them |
|---|---|---|
| Levandoski, Lomet, Sengupta — *The Bw-Tree: A B-tree for New Hardware Platforms* (ICDE 2013) | §II.A–II.D | modern-hardware rationale, mapping table, delta updating, SMO decomposition |
| | §III, §IV | latch-free pages, consolidation, epoch GC; full split/merge protocols |
| | §V | the log-structured flash store — skim, but know it was half the motivation |
| | **§VI.A** | the setup: BerkeleyDB baseline, 4-core machine, ~10,000 lines of C++ |
| | §VI.B.2, Table I | CAS failure rates: updates < 0.02%, splits/consolidates 0.22–8.88% |
| | §VI.C–VI.E | 18.7×/8.6×/5.8× vs BerkeleyDB; 3.7×/4.4× vs skip list; cache distribution |
| Wang, Pavlo et al. — *Building a Bw-Tree Takes More Than Just Buzz Words* (SIGMOD 2018) | §1 | the verdict: under-performs lock-based indexes by **1.5–4.5×** |
| | §2, §3 | Bw-tree essentials; what the original description omitted |
| | §5, §5.1 | machine (2× Xeon E5-2680 v2), YCSB A/C/E, 52M keys |
| | §6.1 | head-to-head: ART > 4×; Masstree and B+Tree ∼2× |
| | **§6.2, Table 2** | high contention: abort rate **1078.63%**; avg leaf delta chain 11.38 |
| | **§6.3, Fig. 18** | the decomposition: −DC +23%/+45%, −MT +18%, −DU +40%, −CAS ≈0; residue 15–19% |
| | §8 | "lock-freedom does not always pay off" |
| Leis, Haubenschild, Neumann — *Optimistic Lock Coupling* (IEEE Data Eng. Bull. 2019) | §3, §3.1 | why latch writes are the problem; the six-line protocol |
| | §3.2 | "an order of magnitude more code" than a B-tree with OLC |
| | §3.3, §3.4 | dangling speculative pointers; fallback to pessimistic locking; reclamation |
| | §4, Table 4 | 15.48 / 14.60 / 5.71 Mop/s and the cycle and miss counts used in Step 1 |

**Code** (pinned at `leanstore/leanstore@90fcf18`)

| File | Lines | What |
|---|---|---|
| `backend/leanstore/sync-primitives/Latch.hpp` | 21–43 | `HybridLatch` — version word with the lock in its low bit |
| | 84–91 | `recheck()` — the validation, and `jumpmu::jump()` as restart |
| | 93–104 | `unlock()` — release by incrementing, so a write advances the version by 2 |
| | 106–117 | `toOptimisticSpin()` — plain-load spin, no shared writes |
| | 128–154 | `toOptimisticOrShared` / `toOptimisticOrExclusive` — the fallback Leis §3.4 calls OLC's advantage |
| | 155–176 | `toExclusive()` — CAS the version, restart on failure |
| `backend/leanstore/sync-primitives/PageGuard.hpp` | — | how the guards compose into a traversal; read after `Latch.hpp` |

**Measurements** — from this topic's own lanes; `notes.md` has the full
output, `FINDINGS.md` row 9 the headline.

| Lane | Figure used above |
|---|---|
| `false_sharing` | uncontended padded atomic RMW **2.28 ns**; contended **40.54 ns**; transfer **38.3 ns** |
| `false_sharing` | pad64 20.4 ms vs pad128 11.4 ms — 64 B is **1.8× slower** on M-series |
| `scaling` | global mutex 8.65 → 2.96 Mops/s (1 → 16 threads): latching that scales *backwards* |

**Cross-topic** — topic 0 §2 for the memory hierarchy that prices all of
this and topic 0 §3 for the fair-benchmarking pitfalls Step 6 applies;
topic 6 for `HybridLatch` read as a buffer-manager primitive; topic 8 for
logical pointers and for the lock/latch distinction; the crossbeam-epoch
guide for the reclamation both designs still need.
