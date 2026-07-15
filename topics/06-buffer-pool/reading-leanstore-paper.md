# LeanStore & vmcache: pay only on the miss

Two papers, one arc: how to make a buffer-managed system as fast as an
in-memory one. LeanStore (ICDE '18) eliminates the per-access costs with
pointer swizzling, a cooling stage, and optimistic latches; vmcache
(SIGMOD '23), from the same group, is "what we'd do differently five years
later" — same goal, mechanism moved into the MMU. This chapter builds the
ideas one at a time — the tax a classic pool charges on every hit, then the
three LeanStore ingredients that zero it out, then vmcache as the
retraction-and-fix — before pointing you at the sections that matter.

## The problem in one sentence

A classic buffer pool charges a hash lookup + a latch + a pin-count update
on *every page access, even when the page is already in RAM* — measured on
in-memory TPC-C, that overhead is a large fraction of total runtime, which
is why in-memory systems like HyPer simply deleted the buffer manager and
gave up larger-than-RAM data to get the speed back.

## The concepts, step by step

### Step 1 — the per-access tax of a classic buffer pool

A buffer pool (the fixed-size in-memory cache of disk pages that the engine
manages itself) translates every page reference through a map: given a
**page id** (the page's number in the file), find the **frame** (the RAM
slot currently holding it). In postgres that means, on every access: hash
the page id, take a partition lock, probe the table, then **pin** the frame
(atomically bump a reference count so eviction can't take it while you look
at it), and unpin after. That's ~2 atomic operations plus a probable cache
miss on the hash bucket — per access, forever, even when 100% of the data
is in RAM and none of this machinery ever does anything useful.

LeanStore's goal (§I–II): **pay for translation and replacement only on
misses** — the hot path should look like an in-memory system. Three
ingredients, each killing one cost:

```
 1. pointer swizzling   translation cost → 0    (parent holds raw pointer)
 2. cooling stage       replacement cost → 0    (no per-access bookkeeping;
                        random candidates + second-chance FIFO)
 3. optimistic latches  pinning cost → 0        (readers validate versions,
                        hold nothing)
```

### Step 2 — pointer swizzling: the parent's pointer IS the translation

**Swizzling** means storing, in the place where a page id would go, the
actual in-memory pointer to the frame — so following a B-tree parent-to-child
link is one pointer dereference, zero lookups. Each reference slot (a
**swip**) is one u64 that is *either* a raw `BufferFrame*` (page resident:
"hot") *or* a page id with a tag bit set (page on disk: "evicted"). The
buffer pool's mapping table is thereby distributed into the data structure
itself; there is no central hash table on the hot path at all.

Why it matters: a hot access costs literally what an in-memory system
charges — a dereference. The cost moved entirely to the miss, where a disk
read dwarfs it anyway.

### Step 3 — the price of swizzling: one parent, bottom-up eviction

If two swips pointed at the same page, evicting it would require finding
and un-swizzling *both* raw pointers — but there's no central table to find
them with. So LeanStore imposes the **one-swip-per-page rule**: every page
has exactly one owner reference. That's natural for a B-tree (each node has
one parent) and awkward for anything graph-shaped.

Same logic forces **bottom-up eviction**: a parent may only be evicted
after all its children are — an evicted parent's swip slot holds a page id,
and a page id can't point at a hot child's frame; the child would become
unreachable. (§III.B is this argument; hold it for capstone question 4 —
GraphBLAS tiles referenced by row *and* column are a DAG, not a tree.)

### Step 4 — the cooling stage: replacement with zero per-access work

Every classic policy (LRU lists, CLOCK usage bits) does a little
bookkeeping on *each access* to know what's cold later. LeanStore refuses:
instead, a background thread picks buffer frames **at random**, un-swizzles
them into a **cooling FIFO** (a queue holding ~10% of the pool, §III.D's
sizing heuristic). A cooling page is still in RAM; if anyone touches it, the
access path notices the "cool" tag and re-swizzles it cheaply — a **second
chance** that rescues hot pages that were unluckily sampled. Reach the end
of the FIFO untouched and you're written back (if dirty) and evicted.

```
 random sample ──► cooling FIFO (~10% of pool) ──► evict at the end
                        │
                        └── touched while cool? re-swizzle: second chance
```

Why it matters: randomness replaces bookkeeping. Fig. 6 shows random +
second-chance FIFO tracks true LRU's hit rate closely on Zipf-skewed
workloads — while charging the hot path *nothing*.

### Step 5 — optimistic latches: readers hold nothing

Pinning exists so eviction can't yank a page mid-read. LeanStore replaces it
with **optimistic latches**: each frame carries a version counter; a reader
notes the version, reads *without writing any shared state*, then
re-checks the version — unchanged means the read was consistent, changed
means retry. Writers bump the version. A reader that holds nothing can't
block eviction and costs zero coherence traffic on the hot path (compare a
pin: an atomic increment that bounces the cache line between every reading
core, topic 0's false-sharing lesson). This is topic 9's main subject making
an early appearance.

### Step 6 — vmcache: keep the goal, drop the swizzling

Swizzling works but *infects the whole codebase*: every data structure must
know swips, honor one-parent, cooperate with cooling. vmcache (SIGMOD '23)
keeps "pay only on the miss" and moves translation into the hardware:

- mmap an **anonymous** virtual range (address space backed by no file —
  the CIDR '22 trap doesn't apply because the kernel never sees your file):
  `page(pid)` is just `virt + pid * 4096` — the MMU (the address-translation
  hardware) is the translation layer, for free.
- BUT the DB — not the kernel — decides residency: an explicit per-page
  state word (Evicted/Marked/Locked/Unlocked + version counter), explicit
  `pread` into the fixed virtual address on fault, `madvise(DONTNEED)` on
  evict.
- The page-state word doubles as the hybrid latch (Step 5's optimistic
  version counter — same bits, new home).
- Any page can have any number of references — the one-parent rule dies;
  arbitrary graphs are fine (relevant to a graph-store capstone!).

The whole design fits in one state machine:

```rust
// Translation is the MMU's job; RESIDENCY is the DB's.
fn page(&self, pid: u64) -> *mut u8 { unsafe { self.virt.add(pid as usize * 4096) } }

fn fix(&self, pid: u64) {
    loop {
        let s = self.state[pid].load();      // Evicted/Marked/Locked/Unlocked + version
        match s.kind() {
            Evicted => if self.state[pid].cas(s, s.locked()) {
                pread(self.fd, self.page(pid), 4096, pid * 4096); // into the FIXED addr
                self.state[pid].store(s.unlocked_bumped());       // word doubles as
                return;                                           // the hybrid latch
            },
            Marked | Unlocked => if self.state[pid].cas(s, s.locked()) { return; },
            Locked => core::hint::spin_loop(),  // someone else is faulting it in
        }
    }
}
// evict: write back if dirty, madvise(DONTNEED, page(pid)), state → Evicted
```

### Step 7 — the map of the design space

Put the three readings of this topic side by side:

```
 LeanStore:  translation in POINTERS  (swips, invasive, tree-shaped data)
 vmcache:    translation in the MMU   (virt addressing, any ref-graph)
 both:       replacement + residency decided by the DB, never the kernel
```

The CIDR '22 mmap paper (reading-mmap-paper.md) is the missing middle: mmap
with *kernel*-controlled residency is the trap; vmcache is mmap-style
addressing with DB-controlled residency. One cost remains: madvise-heavy
eviction pays syscalls and TLB shootdowns — the **exmap** kernel module in
the vmcache paper fixes that; without it, plain vmcache still beats classic
pools.

## How to read the papers (with the concepts in hand)

Read LeanStore first, then vmcache as the retraction-and-fix. You've read
the code (reading-leanstore.md) — in the LeanStore paper focus on:

- **§I–II** — Step 1's problem statement; skim, you know it.
- **§III.B — read carefully.** The one-swip-per-page ownership rule and why
  eviction is bottom-up (Step 3).
- **§III.D — read carefully.** Cooling-stage sizing (the 10% heuristic) and
  the hit/second-chance probabilities — Fig. 6 shows random+FIFO tracks LRU
  closely on Zipf (Step 4).
- **§V** — evaluation: in-memory TPC-C at parity with a
  no-buffer-manager build; the graceful degradation curve as data exceeds
  RAM (the money plot).

In vmcache: the page-state machine (Step 6's code is its skeleton), the
argument for why dropping swizzling gives nothing back, and the exmap
numbers for eviction-heavy workloads.

## Questions to answer in notes.md

1. Reproduce LeanStore Fig. 1's argument as arithmetic: hash probe (topic-0
   DRAM numbers) + latch CAS per access, × accesses per TPC-C txn — what
   fraction of in-memory runtime is the classic pool?
2. Why does the cooling stage need to be a FIFO and not a stack? (Second
   chance requires *time* between cool and evict.)
3. vmcache's page-state word vs postgres's packed buffer state
   (buf_internals.h) — same bits, different home. What does colocating
   state-with-translation (vmcache) buy over a separate descriptor array?
4. For the capstone: GraphBLAS matrix tiles referenced by row and column
   indexes = a DAG, not a tree. Which of the two designs is even admissible,
   and what would the swizzling workaround cost?

## Done when

You can state what each of the three LeanStore ingredients eliminates, and
explain in two sentences why vmcache can drop swizzling without giving back
the hot-path win.

## References

**Papers**
- Leis, Haubenschild, Kemper, Neumann — "LeanStore: In-Memory Data
  Management Beyond Main Memory" (ICDE 2018) — focus on §III.B (one swip
  per page, bottom-up eviction), §III.D (cooling-stage sizing, Fig. 6),
  §V (the graceful-degradation money plot)
- Leis, Alhomssi, Ziegler, Loeck, Dietrich — "Virtual-Memory Assisted
  Buffer Management" (vmcache/exmap, SIGMOD 2023) — read after LeanStore,
  as the retraction and the fix
