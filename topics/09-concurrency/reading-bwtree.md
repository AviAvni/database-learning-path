# Bw-tree vs OLC: why lock-free lost to optimistic latches

Three papers, one arc: the most radical lock-free index ever shipped
(the Bw-tree, ICDE '13), the paper that measured it honestly (SIGMOD '18),
and the modest protocol that won (optimistic lock coupling). The arc is
this topic's thesis in miniature — the memory hierarchy, not elegance,
decides which concurrency scheme survives.

## 1. "The Bw-Tree" (Levandoski et al., ICDE '13)

The design (Hekaton's and DocumentDB's index):

```
 mapping table: PID ─► pointer          update = CAS the PID slot:
 ┌─────┐                                   Δ(insert k) ──┐
 │ P17 ├──► Δ(delete k₂) ─► Δ(insert k₁) ─► base node    │
 └─────┘        newest ◄──────────────── oldest          │
 CAS(P17, old_head, Δnew) — ONE atomic pointer swap per update,
 no in-place writes, no latches anywhere.
```

- **Mapping table** = indirection layer: nodes are logical PIDs, so
  relocating/consolidating a node is a CAS, and parent pointers never
  change. (Wu/Pavlo's "logical pointers" verdict, topic 8 — same lesson.)
- **Delta chains**: readers reconstruct the node by walking deltas until
  a base page; consolidation folds chains back into a base node when
  too long.
- **SMOs** (splits/merges) are multi-step: half-split posts a split-delta,
  then a separate CAS installs the parent entry; every THREAD that
  encounters a partial SMO must **help complete it** — cooperative
  state machines instead of latched critical sections.
- Epochs for reclamation (you know this now).

## 2. "Building a Bw-Tree Takes More Than Just Buzz Words" (SIGMOD '18)

CMU rebuilt it (OpenBw-Tree) and measured against OLC B+tree, Masstree,
ART, skiplist:

- Delta chains murder cache locality: a point read is a pointer chase
  through K deltas (recall topic 0's ladder — each hop is a potential
  DRAM miss) vs a B+tree's two cache-resident binary searches.
- The mapping table's CAS becomes the contention point under skew: hot
  PID = hot cache line — you moved contention, not removed it.
- Consolidation policy is a whole tuning surface (their §4.2 "component
  breakdown" is the useful table — read it as a bill of costs).
- Verdict: **OLC B+tree is 1.5–4× faster** on most workloads and ~10×
  simpler. "Lock-free" bought worse constants, not scalability.

## 3. "Optimistic Lock Coupling" (Leis et al.) — what won

- Per-node: version counter + lock bit (one u64 — LeanStore's
  HybridLatch, topic 6, IS this).
- Reader: read version (spin if locked) → read fields → validate version
  unchanged → proceed; else RESTART from a safe ancestor. Readers write
  no shared memory — root's cache line stays Shared in every core's L1.
- Writer: acquire lock bit (CAS), mutate, release = version+1.
- Coupling: validate parent's version AFTER reading the child pointer —
  the pair (read child ptr, revalidate parent) replaces "hold parent
  latch while grabbing child".
- Restarts need: no torn reads that can fault (reads of freed memory
  must be survivable ⇒ epochs again, or never-free node memory).

The entire reader protocol fits in a loop — note what it never does:
write shared memory.

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

## The arc, in one line

Indirection + deltas (Bw) lost to versions + restarts (OLC) because the
memory hierarchy prices pointer chases higher than optimistic retries.

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
