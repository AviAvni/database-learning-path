# mmap is not a buffer pool

mmap looks like a free buffer pool, and a famous position paper says that for
a general-purpose write-heavy DBMS every apparent win reverses. It is short,
punchy, and deliberately provocative — so read it adversarially, then
construct the counter-evidence yourself (LMDB exists and is excellent).
Before you open it, this chapter builds the concepts one at a time — what
mmap actually does, why it tempts database authors, and the four distinct
ways it betrays them — then hands you a section-by-section reading lens. The
payoff is knowing precisely *which* property of a workload makes mmap wrong.

## The problem in one sentence

If you let the kernel manage your database's memory via `mmap`, the kernel —
not you — decides when dirty pages hit disk, so write-ahead logging becomes
unenforceable; and even for pure reads, the paper measures mmap plateauing
far below the ~6 GB/s a single NVMe drive can deliver, then *degrading over
time* once eviction starts.

## The concepts, step by step

### Step 1 — what mmap actually does

`mmap` asks the OS to map a file directly into your process's virtual
address space: after the call, `file_bytes[i]` is just a pointer
dereference, no `read()` syscall. Nothing is copied up front. The first
touch of each 4 KB region triggers a **page fault** (a hardware trap into
the kernel), the kernel reads that page of the file into its **page cache**
(the kernel's own cache of file data in RAM) and wires the mapping; later
touches are plain memory access. Eviction is also the kernel's job: under
memory pressure it writes dirty pages back and unmaps them — whenever it
likes.

Why it matters: you got a demand-paged cache of the file for ~zero code.
Everything below is the bill for the words "whenever it likes".

### Step 2 — what a buffer pool is, and why mmap tempts

A **buffer pool** is the fixed-size in-memory cache of disk pages that the
database engine manages itself: a `page_id → frame` map, pin counts (a
counter saying "this page is in use, don't evict it"), an eviction policy,
and explicit read/write calls. That's thousands of lines of subtle
concurrent code — and mmap seems to make all of it free: no copies between
kernel and user space, no eviction code, pointer access, and the page cache
is shared with every other process.

Real systems took the bait: MongoDB (MMAPv1 — abandoned), LMDB (kept it,
happily), SQLite (optional), RavenDB. The paper's claim: for a
*general-purpose write-heavy DBMS*, every apparent win reverses. The next
four steps are the four reversals — memorize them:

```
 1. Transactional safety   kernel may flush a dirty page ANY time
    ────────────────────   ⇒ can't order page-write after log-write
                           ⇒ WAL rule unenforceable without COW tricks
 2. I/O stalls             page fault = your thread stops; no async,
    ────────────────────   no prefetch you control, no admission control
 3. Error handling         disk error = SIGBUS in the middle of a memcpy,
    ────────────────────   not an error code at a syscall boundary
 4. Performance (§4)       the surprise: even READ-ONLY mmap loses at scale
```

### Step 3 — problem 1: the kernel breaks the WAL rule

Write-ahead logging (topic 5) rests on one ordering invariant: a modified
page may reach disk only *after* the log record describing the modification
is durable — otherwise a crash leaves a page whose history the log doesn't
contain, and recovery cannot undo it. A buffer pool enforces this trivially:
it controls every page write, so it checks "is the log flushed up to this
page's LSN?" before each one.

With mmap the kernel flushes dirty pages on its own schedule — memory
pressure, periodic writeback, whenever. There is no hook that says "not this
page, not yet." Your only levers are `msync` gymnastics (flushing
*everything* at barriers) or copy-on-write shadow-paging tricks that give up
in-place updates entirely.

Why it matters: this problem alone disqualifies mmap for any engine with
in-place updates + WAL — which is postgres, MySQL, and your topic-3/5 stack.

### Step 4 — problem 2: page faults are I/O you can't schedule

When your thread touches an unmapped page, it stops dead until the kernel
finishes the disk read — a **stall**. There is no way to say "start fetching
these 8 pages, I'll do other work meanwhile": no async interface, no
prefetch you control (the kernel's readahead guesses, and guesses wrong for
random access), and no admission control (nothing stops 100 threads from
faulting at once and burying the disk). A buffer pool turns every miss into
an explicit I/O request it can batch, reorder, and overlap; mmap turns every
miss into a surprise nap of ~100 µs (NVMe) mid-instruction.

### Step 5 — problem 3: errors arrive as signals, not return codes

With explicit I/O, a failed read returns an error code at a syscall
boundary, where you have context and can respond. With mmap, a disk error
surfaces as a **SIGBUS** signal (a hardware-fault signal) delivered in the
middle of whatever instruction touched the page — possibly deep inside a
`memcpy` in a third-party library. Handling that means a process-wide signal
handler that somehow maps a faulting address back to a database operation
and unwinds safely. Nobody does this well; most mmap systems just crash.

### Step 6 — problem 4: even read-only mmap loses at scale

You might concede writes and still want mmap for read-only analytics. §4 is
the paper's surprise: three kernel bottlenecks cap read throughput, measured
with fio on multi-NVMe arrays:

- **page table contention** — parts of the kernel's page-fault path are
  effectively single-threaded; concurrent faulting cores serialize.
- **TLB shootdowns** — the TLB (the per-core cache of virtual→physical
  translations, topic 0) may hold a stale entry on *any* core after an
  unmapping, so evicting one page sends an interrupt (IPI) to every core to
  flush it. Eviction cost *scales with core count* — more cores, worse.
- **4 KB granularity** — mmap moves data one page-table-walk-managed 4 KB
  page at a time; one explicit 2 MB `pread` does the same work with a single
  syscall and no per-page kernel bookkeeping.

Result: explicit `pread`/O_DIRECT sustains device bandwidth; mmap plateaus
far below on NVMe arrays and *degrades over time* once eviction (and its
shootdowns) begins.

Why it matters: note the asymmetry for question 2 below — faulting a page
*in* touches only the faulting core's mappings; evicting must chase every
core that might have cached the translation.

### Step 7 — the rebuttal you must construct: LMDB and the escape hatches

LMDB (topic 3) is mmap-based and wins its niche, so "never mmap" is too
strong; the truth is a checklist. LMDB dodges each bullet: copy-on-write
pages are never overwritten, so problem 1's ordering is a non-problem — the
meta-page flip IS the commit; read-mostly workloads fault once, then it's
just memory (problem 2); a read-only mmap can't SIGBUS on writes (problem
3); and its scale target is "fits mostly in RAM" (problem 4). The paper's
own Table 1 concedes designs like this.

Map it to what you know:

| System | Uses | Escapes the trap because |
|---|---|---|
| LMDB | mmap everything | COW + read-mostly + single writer |
| SQLite | optional mmap for reads | WAL still explicit; mmap read-only |
| postgres | no mmap; shared_buffers | needs write ordering (FPIs, ckpts) |
| LeanStore/vmcache | anonymous mem / virt mapping | explicit residency control |

The honest conclusion: **mmap is wrong when the DB must control
WRITE-BACK.** Read-only/COW designs escape most of it. And vmcache
(SIGMOD '23, reading-leanstore-paper.md) is the synthesis: keep
virtual-memory *addressing*, but the DB — not the kernel — keeps explicit
control of residency and eviction.

## How to read the paper (with the concepts in hand)

It's a short CIDR position paper — one sitting.

- **§1–2 (the temptation)** — skim; this is Step 2. Note the list of
  systems that tried and where each landed.
- **§3 (the four problems)** — read carefully and memorize; Steps 3–5.
  For each problem, ask "which mechanism from my topic-5 WAL does this
  break?"
- **§4 (performance)** — the part worth re-reading; Step 6. Study the fio
  plots: where mmap plateaus, and the degradation-over-time curve once
  eviction starts. This is the section people skip and shouldn't.
- **Table 1** — the paper's own concession matrix; check LMDB's row against
  Step 7 and argue with any cell you disagree with.

Read it adversarially: the authors are deliberately provocative, and the
LMDB rebuttal (Step 7) is *yours* to construct — the paper won't do it for
you.

## Questions to answer in notes.md

1. Your topic-3 B+tree used explicit I/O. If you'd mmap'd it, which of your
   topic-5 WAL guarantees break, concretely? (Which test in
   `crash_test.rs` would start failing and why.)
2. TLB shootdowns: why does *eviction* trigger them but *faulting-in* not?
3. The paper measures read-only workloads losing. Reconcile with LMDB's
   read benchmarks winning — what's different in the setups (working set vs
   RAM, single NVMe vs array, pointer-chase vs scan)?
4. vmcache's answer: keep virtual-memory addressing, add explicit state.
   Which of the four problems does it solve, which does it merely soften?

## Done when

You can argue both sides for five minutes each — "never mmap" and "LMDB is
right" — and state precisely which property of your workload picks the side.

## References

**Papers**
- Crotty, Leis, Pavlo — "Are You Sure You Want to Use MMAP in Your DBMS?"
  (CIDR 2022) — short position paper; memorize the four problems of §3,
  re-read §4 (why even read-only mmap loses at scale)
