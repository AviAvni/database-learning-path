# Reading guide — LeanStore (ICDE '18) + vmcache (SIGMOD '23)

Two papers, one arc: how to make a buffer-managed system as fast as an
in-memory one. Budget 3 h total. Read LeanStore first, then vmcache as
"what we'd do differently five years later" — same group (Leis et al.).

## LeanStore: the problem statement (§I–II)

A traditional buffer pool costs a hash lookup + latch + pin *per page
access* even when everything is in RAM. In-memory systems (HyPer) skip all
of it. LeanStore's goal: **pay for translation and replacement only on
misses** — the hot path should look like an in-memory system.

Three ingredients:

```
 1. pointer swizzling   translation cost → 0    (parent holds raw pointer)
 2. cooling stage       replacement cost → 0    (no per-access bookkeeping;
                        random candidates + second-chance FIFO)
 3. optimistic latches  pinning cost → 0        (readers validate versions,
                        hold nothing)
```

You've read the code (reading-leanstore.md) — in the paper focus on:
- §III.B: the one-swip-per-page ownership rule and why eviction is bottom-up.
- §III.D: cooling-stage sizing (the 10% heuristic) and the hit/second-chance
  probabilities — Fig. 6 shows random+FIFO tracks LRU closely on Zipf.
- §V: evaluation — in-memory TPC-C at parity with a no-buffer-manager build;
  the graceful degradation curve as data exceeds RAM (the money plot).

## vmcache: the retraction and the fix

Swizzling works but infects the whole codebase: every data structure must
know swips, honor one-parent, cooperate with cooling. vmcache keeps the goal
and drops the mechanism:

- mmap an **anonymous** (or exmap) virtual range: `page(pid)` is just
  `virt + pid * 4096` — the MMU is the translation layer, for free.
- BUT the DB — not the kernel — decides residency: explicit per-page state
  word (Evicted/Marked/Locked/Unlocked + version counter), explicit reads
  into the fixed virtual address, `madvise(DONTNEED)` on evict.
- The page-state word doubles as the hybrid latch (optimistic version).
- Any page can have any number of references — the one-parent rule dies;
  arbitrary graphs are fine (relevant to a graph-store capstone!).
- The exmap kernel module fixes the syscall/TLB costs of madvise-heavy
  eviction; without it, plain vmcache still beats classic pools.

```
 LeanStore:  translation in POINTERS  (swips, invasive, tree-shaped data)
 vmcache:    translation in the MMU   (virt addressing, any ref-graph)
 both:       replacement + residency decided by the DB, never the kernel
```

The CIDR '22 mmap paper (reading-mmap-paper.md) is the missing middle: mmap
with kernel-controlled residency is the trap; vmcache is mmap-style
addressing with DB-controlled residency.

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
