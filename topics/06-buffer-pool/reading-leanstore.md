# LeanStore in code: swips, cooling, hybrid latches

The paper claims a hot page access can cost zero atomics; this chapter walks
the classic ICDE '18 codebase to see how — a u64 that is either a pointer or
a page id, a background thread that cools random frames, and latches whose
readers hold nothing. Read the paper guide
([reading-leanstore-paper.md](reading-leanstore-paper.md)) first for the why;
this chapter rebuilds the mechanism step by step, then hands you the file
and line anchors to watch each piece work.

## The problem in one sentence

Postgres charges every page access a hash probe + partition lock + CAS pin
(~2 atomics and a likely cache miss) even when the page is in RAM;
LeanStore's code must deliver the same page, crash-safe and evictable, for
the cost of a single pointer dereference.

## The concepts, step by step

### Step 1 — the swip: one u64 that is either a pointer or a page id

A **swip** is a reference slot inside a parent node that holds *either* a
raw in-memory pointer to a buffer frame (a **BufferFrame** — the RAM slot
holding a page plus its header) *or* an on-disk page id, distinguished by
the top two bits of the word:

```
 bit 63 (evicted)  bit 62 (cool)
      0                 0        HOT     — raw BufferFrame*: dereference it
      0                 1        COOL    — frame exists, sits in cooling FIFO
      1                 -        EVICTED — low bits hold the page id, on disk
```

The buffer pool's mapping table is thereby *distributed into the parent
nodes*: no hash lookup, no partition lock, on any hot access. The price:
exactly one swip may reference a page — if two parents held raw pointers,
un-swizzling on eviction couldn't find them both (no central table to
consult).

### Step 2 — resolveSwip: the three-arm hot path

`resolveSwip` is the single function every page access goes through, and
its three arms are the swip's three states — only the third ever touches
the disk:

```
 isHOT   (:283) → return the pointed-to frame. Done. ~0 overhead.
 isCOOL  (:287) → frame exists but sits in the cooling FIFO:
                  latch parent, clear cool bit (second chance), return.
 EVICTED         → page fault: grab free frame, readPageSync (:317),
                  swizzle the swip, return.
```

The same three arms, as code:

```rust
// The hot path is a pointer dereference — nothing else.
fn resolve(&self, parent: &HybridGuard, swip: &mut Swip) -> &BufferFrame {
    if swip.is_hot() { return swip.frame(); }         // raw pointer: ~0 overhead
    if swip.is_cool() {
        parent.upgrade_exclusive();                   // touched while cooling ⇒
        swip.warm();                                  // second chance: clear the
        return swip.frame();                          // bit, dodge the FIFO
    }
    let frame = self.free_frames.pop();               // EVICTED ⇒ page fault:
    self.read_page_sync(swip.pid(), frame);           // the ONLY case that pays
    swip.swizzle(frame);                              // pid → pointer, in place —
    frame                                             // next access is hot
}
```

Why it matters: the HOT arm is the whole point of the design — the case
that runs 99%+ of the time costs what an in-memory system costs. Note also
the latching-order comment at BufferManager.hpp:67–68: swizzle vs coolPage
acquire latches in *conflicting* order; the fix is jump-and-retry
(optimistic abort) instead of blocking — deadlock avoidance by restart, the
same philosophy as Step 4's latches.

### Step 3 — the cooling stage: replacement with zero per-access cost

Classic eviction policies do bookkeeping on every access (postgres bumps a
usage counter; DuckDB enqueues on every unpin). LeanStore's
`PageProviderThread` does the bookkeeping *for* you, in the background,
keeping ~10% of frames "cool":

- Pick a **random** buffer frame (:44) — no LRU metadata exists at all.
- Phase 1 (:52): unswizzle it (turn the parent's raw pointer back into a
  tagged frame reference with the cool bit set) — but only if all its
  children are already evicted (:90–91, `iterateChildrenSwips`): evict
  leaves before parents, bottom-up. (An evicted parent's swip slot can't
  hold a hot child's pointer — the child would be unreachable.)
- Cool frames enter a per-partition FIFO (Partition.hpp:65+). Touched while
  cool ⇒ resolveSwip warms it (Step 2's second chance — a hot page that was
  unluckily sampled gets rescued for the cost of one bit flip). Reaches the
  FIFO head untouched ⇒ written back if dirty (AsyncWriteBuffer) and
  evicted.

Random + second-chance approximates LRU with zero per-access cost — compare
postgres (per-access usage bump) and DuckDB (per-unpin enqueue). LeanStore
pays *nothing* per access; that's the whole point of the paper.

### Step 4 — hybrid latches: readers that hold nothing

A **latch** is a short-lived lock protecting an in-memory structure; a
classic read latch is an atomic increment that bounces the cache line
between every reading core. LeanStore's `HybridLatch` is a version word
(writers CAS it odd, bump on release), and readers in OPTIMISTIC mode
proceed *without writing anything*: read the version, do the work,
revalidate — version changed ⇒ jump (longjmp-style unwind) and retry.

This is what makes swizzling safe: a reader holding no pin can't block
eviction — the page can be cooled or evicted under it, and the reader just
fails validation and retries. It's also topic 9's main subject making an
early appearance.

### Step 5 — the frame header: dirtiness derived, not flagged

`BufferFrame` (BufferFrame.hpp:18–99) carries the latch in its header (:27
— annotated "NEVER DECREMENT": versions only grow), and defines dirty
without a flag: `isDirty()` = `page.PLSN != last_written_plsn` (:84) — a
page is dirty exactly when its latest change-LSN (log sequence number, the
WAL position of its last modification) is newer than the LSN it was last
written back at. No flag to keep in sync with the WAL — the WAL position
*is* the flag. Nice integration detail to steal for your M6 pool.

## Where each step lives in the code

All under `backend/leanstore/` in the classic ICDE '18 repo (local clone
at `~/repos/leanstore`):

| File | What | Steps |
|------|------|-------|
| `storage/buffer-manager/Swip.hpp` | the tagged u64 | 1 |
| `storage/buffer-manager/BufferManager.cpp` | resolveSwip | 2 |
| `storage/buffer-manager/PageProviderThread.cpp` | cooling | 3 |
| `storage/buffer-manager/Partition.hpp` | cooling FIFO | 3 |
| `sync-primitives/Latch.hpp` | hybrid latches | 4 |
| `storage/buffer-manager/BufferFrame.hpp` | frame header, LSN-dirty | 5 |

- **Step 1**: `Swip.hpp:17–67` — `evicted_bit = 1<<63`, `cool_bit = 1<<62`
  (:21–26); `isHOT()` (:45), `isCOOL()` (:46), `isEVICTED()` (:47);
  `warm()` clears the cool bit (:62), `cool()` sets it (:65), `evict(pid)`
  stores a page id + bit 63 (:67).
- **Step 2**: `resolveSwip` — BufferManager.cpp:281–330 (HOT :283, COOL
  :287, EVICTED page fault + `readPageSync` :317); the conflicting
  latch-order comment — BufferManager.hpp:67–68.
- **Step 3**: PageProviderThread.cpp — random pick :44, phase 1 unswizzle
  :52, children check :90–91 (`iterateChildrenSwips`); cooling FIFO —
  Partition.hpp:65+.
- **Step 4**: `HybridLatch` — Latch.hpp:26–41 (`LATCH_EXCLUSIVE_BIT` :41);
  `Guard` and the OPTIMISTIC read protocol — :51+.
- **Step 5**: `BufferFrame` — BufferFrame.hpp:18–99; latch in header :27;
  `isDirty()` from LSNs :84.

## Questions to answer in notes.md

1. The one-parent constraint: why exactly does swizzling forbid two swips to
   the same page? Walk the eviction of a doubly-referenced page. Then decide:
   do FalkorDB's tensor/matrix blocks form a tree or a DAG?
2. Bottom-up eviction (children before parents): what breaks top-down?
   (An evicted parent's swip can't hold a hot child's pointer — the child
   would be unreachable.)
3. Random candidate selection: estimate hit-rate loss vs true LRU on a Zipf
   workload (then measure — experiments/benches/eviction.rs has a FIFO
   arm you can extend with random-cooling).
4. vmcache (SIGMOD '23) removes swizzling — pages live at `virt[pid]`, the
   mapping is the MMU's problem, explicit state machine per page. What of
   LeanStore survives in it? (Cooling idea stays; swips go; one-parent
   constraint gone — that's the headline win.)

## Done when

You can draw the swip state machine (HOT/COOL/EVICTED with transitions and
who performs each) and explain why a hot hit costs zero atomics.

## References

**Code**
- [leanstore/leanstore](https://github.com/leanstore/leanstore) (the
  classic ICDE '18 codebase) —
  `backend/leanstore/storage/buffer-manager/`: `Swip.hpp`,
  `BufferManager.cpp`, `BufferFrame.hpp`, `PageProviderThread.cpp`,
  `Partition.hpp`; latches in
  `backend/leanstore/sync-primitives/Latch.hpp`. Local clone at
  `~/repos/leanstore`.
