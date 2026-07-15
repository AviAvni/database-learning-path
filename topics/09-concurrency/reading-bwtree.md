# Bw-tree vs OLC: why lock-free lost to optimistic latches

Three papers, one arc: the most radical lock-free index ever shipped
(the Bw-tree, ICDE '13), the paper that measured it honestly (SIGMOD '18),
and the modest protocol that won (optimistic lock coupling). Before the
papers, this chapter builds each design from its first principle — what
latches cost, what CAS can and cannot do, and how the memory hierarchy
prices the alternatives. The arc is this topic's thesis in miniature — the
memory hierarchy, not elegance, decides which concurrency scheme survives.

## The problem in one sentence

Every thread traversing a latched B-tree *writes* the root's latch — even
pure readers — so the root's cache line ping-pongs between all cores at
~100 cycles per bounce and read throughput stops scaling right when core
counts explode; the Bw-tree bet everything on eliminating latches, and
five years later an honest benchmark showed an optimistically-latched
B+tree beating it 1.5–4× with ~10× less code.

## The concepts, step by step

### Step 1 — the enemy: latch traffic on hot cache lines

A **latch** is a short-duration lock protecting a data structure's
physical integrity (nanoseconds, held across one node access — unlike
topic 8's transaction locks, which protect logical content for seconds).
The classic B-tree protocol, latch coupling, acquires the parent's latch,
then the child's, then releases the parent — correct, deadlock-free by
ordering, and a scaling disaster: acquiring even a *read* latch is a write
to the latch word, so every traversal by every thread writes the root's
cache line. Cache coherency (topic 0, Step 8) then bounces that line
between cores at ~100 cycles per transfer. With 64 cores doing lookups,
the root's latch line is the whole bottleneck — reads that share data
perfectly still serialize on the metadata.

Both designs below are answers to exactly this. They differ in how much
of the latch they remove: all of it (Bw-tree) or just the reader's writes
(OLC).

### Step 2 — the lock-free toolkit: CAS, and its one-pointer limit

**CAS** (compare-and-swap) is the atomic CPU instruction "replace this
64-bit word with a new value only if it still equals the value I read" —
the primitive from which all lock-free structures are built. A **lock-free**
structure guarantees system-wide progress with no latches at all: threads
publish changes by CASing a pointer; losers retry.

The catch that shapes everything: CAS swaps ONE word. A B-tree update
often touches several (modify a node in place, or split one node and
update its parent — two pointers, two nodes). So a lock-free B-tree must
recast every multi-word operation as a chain of single-pointer
publications — which is precisely the Bw-tree's design, and the source of
both its elegance and its downfall.

### Step 3 — the mapping table: indirection makes every change one CAS

The Bw-tree's first move: nodes are identified by logical **PIDs** (page
ids), and a central **mapping table** maps PID → pointer to the node's
current in-memory representation. All inter-node links store PIDs, never
raw pointers. Now "change node P17" = CAS the single mapping-table slot
for P17 — one word, exactly what CAS can do — and no parent or sibling
ever needs updating when a node's physical location changes. (Wu/Pavlo's
"logical pointers" verdict from topic 8 — same lesson: indirection
decouples updaters.)

### Step 4 — delta chains: updates without touching the node

Second move: never modify a node in place. An update allocates a small
**delta record** ("insert k₁", "delete k₂") pointing at the node's current
representation, and CASes the mapping-table slot to point at the delta —
prepending to a chain:

```
 mapping table: PID ─► pointer          update = CAS the PID slot:
 ┌─────┐                                   Δ(insert k) ──┐
 │ P17 ├──► Δ(delete k₂) ─► Δ(insert k₁) ─► base node    │
 └─────┘        newest ◄──────────────── oldest          │
 CAS(P17, old_head, Δnew) — ONE atomic pointer swap per update,
 no in-place writes, no latches anywhere.
```

Readers reconstruct the node by walking the chain down to the **base
node**, applying deltas as they go; when a chain grows too long,
**consolidation** folds it into a fresh base node (published, again, by
one CAS). Reclamation of replaced deltas/nodes uses epochs — the
crossbeam-epoch guide's scheme, and you know why: a reader may still be
walking the old chain. The cost is already visible if you've internalized
topic 0: a K-delta chain turns one node read into K dependent pointer
chases — K potential DRAM misses at ~100 ns each.

### Step 5 — SMOs: multi-node changes as cooperative state machines

Splits and merges (**SMOs** — structure modification operations) touch two
nodes and a parent, but CAS publishes one word — so the Bw-tree breaks
them into a sequence of individually-CASable steps: a half-split first
posts a split-delta on the child (readers now route around it), then a
separate CAS installs the new separator in the parent. Between steps, the
tree is in a valid-but-incomplete state — and any thread that stumbles on
a partial SMO must **help complete it** before proceeding (waiting for the
original thread would reintroduce blocking — question 2). Latched critical
sections become cooperative state machines: correct, and brutally hard to
write, test, and tune.

### Step 6 — the reality check: SIGMOD '18 measures it honestly

CMU rebuilt the design (OpenBw-Tree) and benchmarked it against an OLC
B+tree, Masstree, ART, and a skiplist. Findings:

- **Delta chains murder cache locality**: a point read is a pointer chase
  through K deltas (each hop a potential DRAM miss — topic 0's ladder) vs
  a B+tree's two cache-resident binary searches.
- **The mapping table just relocates contention**: under skew, the hot
  PID's slot is a hot cache line being CASed by everyone — you moved the
  ping-pong from the latch word to the mapping slot, not removed it.
- **Consolidation policy is a whole tuning surface** — their §4.2
  component breakdown is the useful table; read it as a bill of costs.
- Verdict: **the OLC B+tree is 1.5–4× faster** on most workloads and ~10×
  simpler. "Lock-free" bought worse constants, not scalability.

### Step 7 — OLC: the modest protocol that won

**Optimistic lock coupling** keeps the latch but makes readers stop
writing it. Per node: one u64 holding a version counter + a lock bit
(LeanStore's HybridLatch from topic 6 IS this). A writer CASes the lock
bit, mutates, and releases by incrementing the version. A reader never
acquires anything: it reads the version, reads the node, then re-checks
the version — if unchanged, nothing mutated underneath it; if changed,
RESTART from a safe ancestor:

```rust
fn read_node<T>(n: &Node, read: impl Fn(&Node) -> T) -> T {
    loop {
        let v1 = n.version.load(Acquire);
        if v1 & LOCKED != 0 { spin_wait(); continue; } // writer active
        let out = read(n);                    // read optimistically...
        if n.version.load(Acquire) == v1 {
            return out;                       // ...nothing moved: done
        }                                     // else a writer intervened:
    }                                         // restart — the only cost
}
```

Note what the reader never does: write shared memory. The root's cache
line stays Shared in every core's L1 — Step 1's enemy is dead, with a
plain B+tree's memory layout intact. "Coupling" survives as validation
order: validate the parent's version AFTER reading the child pointer —
the pair (read child ptr, revalidate parent) replaces "hold parent latch
while grabbing child". Two residual costs: restarts (rare — a restart
needs a writer to hit *your* path mid-read; question 3 has you compute
how rare), and torn reads of freed memory must be survivable, so node
reclamation still needs epochs or never-freed node memory.

The arc, in one line: indirection + deltas (Bw) lost to versions +
restarts (OLC) because the memory hierarchy prices pointer chases higher
than optimistic retries.

## How to read the papers (with the concepts in hand)

Read in arc order — design, autopsy, winner:

1. **Levandoski et al., ICDE '13 (§II–IV)** — Steps 3–5 in the authors'
   words: mapping table (§II), delta updates and consolidation (§III),
   SMOs and helping (§IV). Read it 2013-generously: latch-free looked
   inevitable, and the flash-friendly log-structured page store (§V, skim)
   was half the motivation.
2. **Wang, Pavlo et al., SIGMOD '18** — the reality check, Step 6. The
   §4.2 component breakdown is the table to study — read it as a bill of
   costs; then the head-to-head graphs. Note *which* workloads are closest
   for the Bw-tree and why (write-heavy, low skew).
3. **Leis et al., IEEE Data Eng. Bulletin 2019** — short; the OLC
   protocol, Step 7. Map every rule to `read_node()` above, and note the
   restart-safety requirements (survivable torn reads) — that's the fine
   print people forget.

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

## References

**Papers**
- Levandoski, Lomet, Sengupta — "The Bw-Tree: A B-tree for New Hardware
  Platforms" (ICDE 2013) — the design; §II–IV
- Wang, Pavlo et al. — "Building a Bw-Tree Takes More Than Just Buzz
  Words" (SIGMOD 2018) — the reality check; §4.2's component breakdown
  is the useful table, read it as a bill of costs
- Leis et al. — "Optimistic Lock Coupling: A Scalable and Efficient
  General-Purpose Synchronization Method" (IEEE Data Eng. Bulletin 2019)
  — short; the protocol that won
