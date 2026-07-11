# Reading LeanStore — swizzling, cooling, hybrid latches (2 h)

Repo: `~/repos/leanstore` (the classic ICDE '18 codebase). Files under
`backend/leanstore/storage/buffer-manager/`: `Swip.hpp`, `BufferManager.cpp`,
`BufferFrame.hpp`, `PageProviderThread.cpp`, `Partition.hpp`; latches in
`backend/leanstore/sync-primitives/Latch.hpp`.

Read the paper guide (reading-leanstore-paper.md) first for the why; this is
the how.

## 1. Swip — Swip.hpp:17–67

One u64 that is EITHER a pointer or a page id:

- `evicted_bit = 1<<63`, `cool_bit = 1<<62` (:21–26).
- `isHOT()` (:45) — both bits clear ⇒ it's a raw `BufferFrame*`.
- `isCOOL()` (:46), `isEVICTED()` (:47); `warm()` clears the cool bit (:62),
  `cool()` sets it (:65), `evict(pid)` stores a page id + bit 63 (:67).

The buffer pool's mapping table is *distributed into the parent nodes*: no
hash lookup, no partition lock, on any hot access. The price: exactly one
swip may reference a page (else un/re-swizzling can't find all pointers).

## 2. resolveSwip — BufferManager.cpp:281–330

```
 isHOT   (:283) → return the pointed-to frame. Done. ~0 overhead.
 isCOOL  (:287) → frame exists but sits in the cooling FIFO:
                  latch parent, clear cool bit (second chance), return.
 EVICTED         → page fault: grab free frame, readPageSync (:317),
                  swizzle the swip, return.
```

Note the latching order comment — BufferManager.hpp:67–68: swizzle vs
coolPage acquire latches in *conflicting* order; the fix is jump-and-retry
(optimistic abort) instead of blocking. Deadlock avoidance by restart — the
same philosophy as optimistic latches below.

## 3. The cooling stage — PageProviderThread.cpp

Background thread keeps ~10% of frames "cool":

- Pick a **random** buffer frame (:44) — no LRU bookkeeping at all.
- Phase 1 (:52): unswizzle it — but only if all its children are evicted
  (:90–91, `iterateChildrenSwips`): evict leaves before parents, bottom-up.
- Cool frames enter a per-partition FIFO (Partition.hpp:65+). Touched while
  cool ⇒ resolveSwip warms it (cheap save). Reaches FIFO head ⇒ written back
  if dirty (AsyncWriteBuffer) and evicted.

Random + second-chance approximates LRU with zero per-access cost — compare
postgres (per-access usage bump) and DuckDB (per-unpin enqueue). LeanStore
pays *nothing* per access; that's the whole point of the paper.

## 4. Hybrid latches — Latch.hpp

- `HybridLatch` — :26–41: a version word; `LATCH_EXCLUSIVE_BIT` in the low
  bit (:41).
- `Guard` — :51+: OPTIMISTIC state reads the version, proceeds *without
  writing anything*, revalidates at the end; version changed ⇒ jump (longjmp
  -style unwind) and retry. Writers CAS the version odd.
- `BufferFrame` — BufferFrame.hpp:18–99: `latch` sits in the header (:27,
  "NEVER DECREMENT" — versions only grow); `isDirty()` = `page.PLSN !=
  last_written_plsn` (:84) — dirtiness derived from LSNs, not a flag. Nice
  WAL-integration detail for your M6.

This is topic 9's main subject making an early appearance — for now, note
that optimistic readers are what make swizzling safe: a reader holding no pin
can't block eviction, it just fails validation and retries.

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
