# mmap is not a buffer pool

mmap looks like a free buffer pool, and a famous position paper says that for
a general-purpose write-heavy DBMS every apparent win reverses. It is short,
punchy, and deliberately provocative — so read it adversarially, then
construct the counter-evidence yourself (LMDB exists and is excellent).
Before you open it, this chapter builds the concepts one at a time — what
mmap actually does, what it costs *here* on the machine this repo measures,
why it tempts database authors, and the four distinct ways it betrays them —
then hands you a section-by-section reading lens. The payoff is knowing
precisely *which* property of a workload makes mmap wrong.

The paper is Crotty, Leis and Pavlo, *"Are You Sure You Want to Use MMAP in
Your Database Management System?"*, CIDR 2022 — 7 pages. Every figure quoted
below carries the section, figure or table it came from in that paper. Every
*measured* number is this repo's own: topic 6's `pool_vs_mmap` lane,
[FINDINGS.md](../../FINDINGS.md) row 6, with the full output in
[`notes.md`](notes.md). Nothing here is remembered.

## The problem in one sentence

If you let the kernel manage your database's memory via `mmap`, the kernel —
not you — decides when dirty pages reach disk, so write-ahead logging becomes
unenforceable; and even for pure reads the cost of an access is bimodal, this
repo measuring **42 ns at the median against 182 µs at the maximum** on the
same instruction ([FINDINGS.md](../../FINDINGS.md) row 6), with the database
unable to tell the two apart in advance.

## The concepts, step by step

### Step 1 — what mmap actually does

> **In:** nothing yet — this step fixes the vocabulary every later step
> leans on.
> **Out:** the seven-stage access path of the paper's Fig. 1, and the two
> words (*page fault*, *TLB shootdown*) that Steps 2 and 7 turn into numbers.

`mmap` asks the OS to map a file into your process's **virtual address
space** — the range of addresses your process can name, which the hardware
translates to physical RAM addresses. After the call, `file_bytes[i]` is a
pointer dereference, not a `read()` syscall. Nothing is copied up front.

The paper's Fig. 1 walks the seven stages (§2.1). Condensed: the program
calls `mmap` and gets a pointer ①; the OS reserves address space but loads no
data ②; the program dereferences the pointer ③; the OS looks for a mapping
④; finding none it takes a **page fault** — a hardware trap into the kernel
raised when a touched address has no valid translation ⑤; the kernel loads
the page into the **page cache**, its own RAM cache of file data ⑥; and adds
the translation to the **page table** (the kernel's per-process map from
virtual to physical addresses) and to the faulting core's **TLB** — the
translation lookaside buffer, a small per-core hardware cache of recent
virtual→physical translations ⑦.

Two faults, not one, and the distinction runs through this whole chapter. A
**major page fault** needs a disk read because the data is not in RAM at all.
A **minor page fault** needs no I/O: the bytes are already in the page cache,
only *this* process's page-table entry is missing, so the kernel just wires
the mapping up. Minor faults are the cheap kind — and Step 2 shows that
"cheap" still means three orders of magnitude worse than a hit.

Eviction is the kernel's job too, and it is where the asymmetry lives. When
the OS evicts a page it must remove the translation from the page table *and*
from every core's TLB. Flushing the local core's TLB is easy; remote cores
are the problem, because — as §2.1 states — current CPUs provide no coherence
for remote TLBs, so the OS must send an **inter-processor interrupt** (an
IPI: one core forcibly interrupting another) to make each remote core flush.
That is a **TLB shootdown**, and §3.4 prices it at thousands of cycles,
citing Villavieja et al.

Why it matters: you got a demand-paged cache of the file for ~zero code.
Everything below is the bill — and note already that faulting a page *in*
touches one core, while throwing one *out* touches all of them.

### Step 2 — the cost, measured on this machine

> **In:** the fault vocabulary from Step 1.
> **Out:** the two numbers — 42 ns resident, 4459 ns at p99.9 — that make
> "the kernel decides" a latency problem rather than an aesthetic one. Steps
> 5 and 7 spend them.

This repo's topic 6 lane does the smallest honest version of the paper's
experiment: `cargo run --release --bin pool_vs_mmap` maps a 1 GiB file
(262,144 pages of 4 KiB) and performs 2,000,000 Zipf(0.99)-distributed page
reads, touching 8 bytes per page so that the access — not a `memcpy` —
dominates. On an Apple M3 Pro it prints ([`notes.md`](notes.md), baseline
measured 2026-07-28):

```
mmap    p50 42 ns    p99 1500 ns    p99.9 4459 ns    max 181887 ns
```

The file is 1 GiB and the machine's page cache is far larger, so essentially
none of these are major faults. The tail is minor faults: pages the kernel
holds but has not mapped into this process, plus the eviction traffic that
mapping them provokes.

Now the division that makes the spread mean something:

```
spread, max over median:      181887 / 42                = 4330×
```

A single instruction — the same load, in the same loop — costs either 42 ns
or 182 µs, and nothing in the program can tell which. Ask next how *rare* the
bad case has to be before it stops mattering. Let f be the fraction of
accesses that fault, 42 ns the resident cost, and C the cost of a fault; then

```
mean = 42(1 − f) + C·f = 42 + (C − 42)·f          [ns per access]

C = 1500 ns  (the measured p99, the cheapest fault in the run):
    mean doubles at f = 42 / 1458   = 0.0288  = 2.88%   ≈ 1 access in 35
C = 4459 ns  (the measured p99.9):
    mean doubles at f = 42 / 4417   = 0.0095  = 0.95%   ≈ 1 access in 105
C = 181887 ns (the measured max):
    mean doubles at f = 42 / 181845 = 0.00023 = 0.023%  ≈ 1 access in 4330
```

One access in a hundred is enough to double the average cost of every access
in the program. Turn it around and price the tail that was actually measured:
the run's slowest 0.1% is 2,000 samples of at least 4459 ns, so those alone
contribute at least `0.001 × 4459 = 4.46 ns` to the mean — more than a tenth
of the median's *entire* cost, contributed by one access in a thousand. The
slowest 1% (20,000 samples at ≥ 1500 ns) accounts for at least 30 ms, against
the 84 ms an all-median run of 2,000,000 × 42 ns would have taken: **1% of
the accesses, 36% again of the whole idealised run.**

Why it matters: mmap's median is genuinely excellent, and that is the trap.
The paper's four problems are all arguments about the other 1%, and the other
1% is where the run's time is.

### Step 3 — what a buffer pool is, and why mmap tempts

> **In:** the fault costs from Step 2.
> **Out:** the machinery mmap appears to make unnecessary, and the list of
> real systems that took the bet — the setup for the four reversals in Steps
> 4 to 7.

A **buffer pool** is the fixed-size in-memory cache of disk pages that the
database engine manages itself. Its parts, each of which has a step of its
own in this topic's other chapters:

- a **page**: the fixed-size unit of transfer between disk and memory
  (8 KB in postgres, 4 KiB in this topic's lane, 16 KB in LeanStore's
  experiments);
- a **frame**: one slot of RAM that holds one page, plus its bookkeeping;
- a `page_id → frame` map, so a page reference can find its frame;
- **pin** and **unpin**: taking and releasing a reference count on a frame
  that makes it ineligible for eviction while you hold a pointer into it;
- a **dirty page**: one modified in RAM whose changes are not yet on disk;
- an **eviction policy**: the rule choosing which unpinned frame to reuse
  when a new page must be read in.

That is thousands of lines of subtle concurrent code — and mmap appears to
make all of it free: no copy between kernel and user space, no eviction code,
no pin counts, pointer access, and a page cache shared with every other
process.

Real systems took the bait. Table 1 lists ten and the years each used mmap:
MonetDB (2002–), MongoDB (2009–2019), LevelDB (2011–), LMDB (2011–), SQLite
(2013–), SingleStore (2013–2015), QuestDB (2014–), RavenDB (2014–), InfluxDB
(2015–2020) and WiredTiger (2020–). **Table 1 is that list and nothing more**
— it is not a table of verdicts, and the paper's own concessions live in §6,
which Step 8 quotes.

§2.3 tells the cautionary half. MongoDB's MMAPv1 needed "an overly complex
copying scheme" and could not compress on-disk data, and was deprecated in
2015 and removed in 2019. SingleStore's `mmap` calls took 10–20 ms per query
— *nearly half of total query runtime* — on contention over a shared mmap
write lock, and switching to `read` made the queries CPU-bound. InfluxDB hit
write I/O spikes past a few GB and dropped mmap for IOx. RocksDB was forked
out of LevelDB partly over read bottlenecks caused by the latter's mmap use.

Why it matters: this is not a hypothesis about what might go wrong. It is a
list of engineering teams who paid for the discovery.

### Step 4 — problem #1 (§3.1): the kernel breaks the WAL rule

> **In:** the buffer pool vocabulary from Step 3 — dirty pages in
> particular.
> **Out:** the ordering invariant mmap cannot express, and the three
> workarounds the paper catalogues, one of which is LMDB's and returns in
> Step 8.

Write-ahead logging (topic 5) rests on one ordering invariant: a modified
page may reach disk only *after* the log record describing the modification
is durable. Otherwise a crash leaves a page whose history the log does not
contain, and recovery cannot undo it. A buffer pool enforces this trivially
because it performs every page write itself, so it can check "is the log
flushed past this page's LSN?" first.

§3.1 states the core issue exactly: "due to transparent paging, the OS can
flush a dirty page to secondary storage at any time, irrespective of whether
the writing transaction has committed. The DBMS cannot prevent these flushes
and receives no warning when they occur."

The obvious lever does not work either. `mlock` pins pages in memory, but
§2.2 records that POSIX (and Linux) permit the OS to flush a dirty page to
the backing file *even while it is locked*. Pinning stops eviction, not
write-back. There is no call that means "not this page, not yet".

So mmap-based systems buy the ordering back with a protocol. §3.1 classifies
all of them into three:

| Protocol | Who uses it (§3.1) | What it costs |
|---|---|---|
| **OS copy-on-write** — a second `MAP_PRIVATE` mapping as a private workspace, changes applied there, WAL for durability, a background thread propagating to the primary | MongoDB MMAPv1 | bookkeeping for pages with pending updates; the private workspace grows toward a *second full copy* of the database, needing periodic `mremap` compaction that must itself block pending changes |
| **User space copy-on-write** — copy the affected pages out of the mapping into a user-space buffer, update and log there, copy back after the WAL is durable | SQLite, MonetDB, RavenDB | a page copy per update (some systems apply WAL records straight into the mapping to avoid it) |
| **Shadow paging** — primary and shadow copies both mmap'd; copy the page to the shadow, change it, `msync` the shadow, then flip a pointer to install it as primary | LMDB | the copy per updated page, plus (in LMDB) only a single writer, which is how it keeps transactions from seeing partial updates |

Every one of those is machinery — which is the paper's rhetorical point: the
thing you adopted mmap to avoid writing, you end up writing anyway, in a
harder form.

Why it matters: this problem alone disqualifies mmap for any engine that does
in-place updates under a WAL — postgres, MySQL, and your topic-3/5 stack.

### Step 5 — problem #2 (§3.2): page faults are I/O you cannot schedule

> **In:** Step 2's measured tail and Step 1's fault taxonomy.
> **Out:** the three workarounds §3.2 evaluates and rejects, plus the
> read-amplification arithmetic of the default `madvise` hint.

With a buffer pool a miss is an explicit request, so the engine can issue it
asynchronously (`libaio`, `io_uring`), batch it, reorder it, or overlap it
with computation. §3.2's example is a B+tree leaf scan: the reads for
non-contiguous leaves could all be issued at once to mask latency — but
"mmap does not support asynchronous reads". Worse, since the OS may
transparently evict, §3.2 notes that even a *read-only* query can trigger a
blocking fault, "because the DBMS cannot know whether the page is in memory".
That is Step 2's bimodality restated as a scheduling problem: 42 ns or
182 µs, and no way to ask which before committing to the load.

The paper works through the escape hatches:

- **`mlock`** — pin the pages you will need again. But the OS limits how much
  memory a process may lock, and you must track and unlock pages yourself.
- **`madvise`** — **`madvise` is the call that hands the kernel a hint about
  an expected access pattern**, per file or per page range. §2.2 covers three
  hints: `MADV_NORMAL`, `MADV_RANDOM`, `MADV_SEQUENTIAL`. They are hints; the
  OS may ignore them, and §3.2 warns that the wrong one "can have dire
  implications for performance".
- **Prefetch threads** — spawn helpers to touch pages so *they* block instead
  of the query thread. It works, and it is a thread pool you now maintain to
  simulate the async I/O interface you gave up.

The default hint is worth doing the arithmetic on. §2.2: under `MADV_NORMAL`
a fault fetches the accessed page **plus the next 16 and the previous 15**.
With 4 KB pages:

```
pages moved per fault:   1 + 16 + 15            = 32 pages
bytes moved per fault:   32 × 4 KB              = 128 KB   (§2.2's figure)
read amplification for a 4 KB random read:      = 32×

what that costs on the paper's own drive (§4: Samsung PM1733, rated
7000 MB/s read), if every fault moves its full 128 KB:
    7,000,000,000 B/s ÷ 131,072 B  =    53,406 faults/s
against 4 KB per fault:
    7,000,000,000 B/s ÷   4,096 B  = 1,708,984 faults/s
```

For a random-access OLTP workload the default hint spends 97% of the device's
bandwidth on pages nobody asked for — which is why §2.2 recommends
`MADV_RANDOM` for larger-than-memory OLTP and `MADV_SEQUENTIAL` for scans.

Why it matters: every workaround is *more* code than the buffer-pool call it
replaces, and none of them restores the one thing you wanted: knowing, before
you dereference a pointer, whether it will cost 42 ns or a trip to the
kernel.

### Step 6 — problem #3 (§3.3): errors arrive as signals, not return codes

> **In:** Steps 4 and 5 — the kernel writes when it likes and faults when it
> likes.
> **Out:** the third problem, which is about *correctness plumbing* rather
> than performance, and is the one most often shortened to "SIGBUS" and
> thereby under-stated.

§3.3 makes three distinct points, and the third is the famous one.

1. **Checksums must be re-validated on every access.** A DBMS that keeps a
   page checksum normally verifies it once, when the page is read into the
   pool. Under mmap the OS may have transparently evicted and re-read the
   page since your last access, so the check has to happen on *every* access
   to mean anything.
2. **Corruption is persisted silently.** These systems are typically written
   in memory-unsafe languages, and a stray pointer write lands in a mapped
   page. A buffer pool can check pages before it writes them out, because it
   performs the write; mmap "will silently persist corrupted pages to the
   backing file".
3. **I/O errors become signals.** With explicit I/O a failed read returns an
   error code at a syscall boundary, and handling can be contained in one
   module. With mmap, any code that touches mapped memory can raise a
   **`SIGBUS`** — a hardware-fault signal — delivered in the middle of
   whatever instruction touched the page, possibly deep inside a third-party
   `memcpy`. The handler must map a faulting address back to a database
   operation and unwind safely.

Why it matters: the first two are the ones people forget. mmap does not just
make error *handling* awkward; it moves the checksum from the miss path,
where it is free, onto the hit path, where Step 2 says you have 42 ns to
spend.

### Step 7 — problem #4 (§3.4, §4): even read-only mmap loses at scale

> **In:** the TLB-shootdown mechanism from Step 1.
> **Out:** the paper's three named bottlenecks and the measured gaps —
> the numbers Step 8's rebuttal has to survive.

You might concede writes and still want mmap for read-only analytics. §3.4
is the surprise, and it names exactly three bottlenecks — worth getting
right, because a fourth is often invented for this list:

1. **Page table contention** — the OS must synchronize the page table, "which
   becomes highly contended with many concurrent threads" (§4.1).
2. **Single-threaded page eviction** — "the OS uses only a single process
   (`kswapd`) for page eviction, which was CPU-bound in our experiments"
   (§4.1). One kernel thread against 128 hardware threads of demand.
3. **TLB shootdowns** — Step 1's IPI storm, thousands of cycles each (§3.4),
   measured through `/proc/interrupts` and plotted in Fig. 2b.

§3.4 adds the crucial asymmetry: shootdowns "occur during page eviction when
a core needs to invalidate mappings in a remote TLB". Faulting a page *in*
installs a translation on one core. Evicting one must chase every core that
might hold it — so eviction cost grows with core count. That is question 2
below, and it is why the paper's plots are flat until the page cache fills
and then fall off a cliff.

The measurements (§4), on an AMD EPYC 7713 (64 cores, 128 hardware threads),
512 GB RAM of which 100 GB was available to Linux 5.11 for its page cache,
and 10 × 3.8 TB Samsung PM1733 SSDs rated 7000 MB/s read, accessed as raw
block devices; the baseline is `fio` 3.25 with `O_DIRECT`:

| Experiment | fio | mmap | Where |
|---|---|---|---|
| Random reads, 2 TB range, 100 threads (95% of accesses fault) | ~900K reads/s, stable | matched fio for the first 27 s, **dropped to nearly zero for ~5 s**, recovered to about **half** of fio | §4.1, Fig. 2a |
| Sequential scan, 1 SSD | full device bandwidth, stable | matched fio, then fell off after ~17 s | §4.2, Fig. 3 |
| Sequential scan, 10 SSDs (RAID 0) | scales | **~20× worse**, "virtually no improvement over the results from using one SSD" | §4.2, Fig. 4 |

The fio baseline is worth checking rather than trusting, and §4.1 shows its
work: 100 threads each with one outstanding I/O against an NVMe latency of
roughly 100 µs gives

```
100 outstanding I/Os ÷ 0.000100 s = 1,000,000 reads/s   (the ceiling)
measured: ~900,000 reads/s                              = 90% of it
```

so `fio` really is saturating the device, and mmap's post-collapse half is
half of a real number. The paper's summary sentence (§4.2): mmap "performs
well only on a single SSD during the initial loading phase. Once page
eviction begins or when using multiple SSDs, mmap is 2–20× worse than fio."

Note what triggers every collapse: the page cache filling up, i.e. **the
moment eviction starts**. Before that, mmap is fine. This is the same shape
as Step 2's spread on a laptop, three orders of magnitude larger.

Why it matters: the read-only case was supposed to be mmap's safe harbour,
and it is the case the paper measures losing.

### Step 8 — the rebuttal you must construct: LMDB and the escape hatches

> **In:** all four problems (Steps 4–7).
> **Out:** the checklist that decides, per workload, whether the paper's
> conclusion applies to you — and the paper's own version of that checklist.

LMDB (topic 3) is mmap-based and wins its niche, so "never mmap" is too
strong. The honest form is a checklist, and LMDB dodges each bullet:

- **Problem 1** — LMDB uses shadow paging (§3.1's third protocol): pages are
  never overwritten in place, so no ordering rule can be violated by an
  untimely flush. Commit is `msync` of the modified shadow pages followed by
  the pointer flip that installs them as primary. A single writer removes the
  conflict cases.
- **Problem 2** — read-mostly workloads fault once per page and then run at
  Step 2's 42 ns.
- **Problem 3** — a read-only mapping cannot silently persist a corrupted
  page, which removes §3.3's second point; the SIGBUS exposure remains.
- **Problem 4** — the collapse in §4 starts when the page cache fills.
  LMDB's target is a working set that fits.

| System | Uses | Escapes the trap because |
|---|---|---|
| LMDB | mmap everything | shadow paging + read-mostly + single writer |
| SQLite | optional mmap for reads | WAL still explicit; mapping used read-only |
| postgres | no mmap; `shared_buffers` | needs write ordering (FPIs, checkpoints) |
| LeanStore / vmcache | anonymous memory, explicit residency | the DB, not the OS, decides eviction |

And the paper says the same thing itself, in §6, which is where its
concessions actually live (not Table 1). *When you should not use mmap*: you
need transactionally safe updates; you need to handle page faults without
blocking or need explicit control over what is in memory; you care about
error handling; you require high throughput on fast storage. *When you might*:
"Your working set (or the entire database) fits in memory and the workload is
read-only" — or "you need to rush a product to the market and do not care
about data consistency or long-term engineering headaches. Otherwise, never."

The honest conclusion: **mmap is wrong when the DB must control write-back,
and unpredictable whenever the page cache is under pressure.** Read-only,
shadow-paged, fits-in-RAM designs escape most of it. vmcache (SIGMOD '23,
[`reading-leanstore-paper.md`](reading-leanstore-paper.md) Step 6) is the
synthesis the paper itself gestures at in §5 when it endorses "lightweight
buffer management techniques" like **pointer swizzling** — storing the
in-memory frame pointer where the page id would go, so a resident access
needs no lookup: keep virtual-memory *addressing*, but let the DB keep
explicit control of residency and eviction.

## How to read the paper (with the concepts in hand)

Seven pages, one sitting.

| Section | How to read it | Step |
|---|---|---|
| §1, §2.1 | The mmap mechanism and Fig. 1's seven stages. Skim if Step 1 landed, but read the TLB-shootdown paragraph at the end of §2.1 twice — it is the mechanism behind §4. | 1 |
| §2.2 | The POSIX API. The two sentences worth memorising: `mlock` does not prevent flushes, and `MADV_NORMAL` fetches 32 pages. | 4, 5 |
| §2.3 + Table 1 | Who tried it and what happened. Table 1 is a *list of systems and years*, not a verdict matrix. | 3 |
| §3.1–3.4 | The four problems. For each, ask "which mechanism from my topic-5 WAL does this break?" | 4–7 |
| §4 | The part people skip. Study *when* each line falls over — always at the moment the page cache fills — and Fig. 2b's shootdown rate next to Fig. 2a's throughput. | 7 |
| §5 | Two paragraphs, easily missed: the authors endorse pointer swizzling as the right alternative. That is topic 6's LeanStore thread. | 8 |
| §6 | The prescription. This, not Table 1, is where the paper concedes. | 8 |

Read it adversarially: the authors are deliberately provocative, and the LMDB
rebuttal (Step 8) is *yours* to construct — the paper won't do it for you.

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

Answer each before unfolding it.

- [ ] You can argue both sides for five minutes each — "never mmap" and "LMDB is right" — and state precisely which property of your workload picks the side.

  <details><summary>Answer</summary>

  The "never mmap" case is §3.1's sentence — the OS can flush a dirty page at
  any time, irrespective of whether the writing transaction committed, and
  `mlock` does not stop it (§2.2) — plus §4's measurements: mmap matched fio
  for 27 seconds and then dropped to nearly zero for five, recovering to half
  of fio's ~900K reads/s (Fig. 2a), and was ~20× worse across 10 SSDs
  (Fig. 4).

  The "LMDB is right" case is that all four problems are conditional. Shadow
  paging (§3.1's third protocol) never overwrites a page, so there is no
  ordering to violate; a single writer removes the conflict cases; read-mostly
  access faults once per page and then runs at this repo's measured 42 ns
  ([FINDINGS.md](../../FINDINGS.md) row 6); and every collapse in §4 begins
  when the page cache fills, which a fits-in-RAM working set never does.

  The property that picks the side is **who must control write-back, and
  whether the working set fits**. In-place updates under a WAL need the
  engine to order page writes against log writes, and mmap cannot express
  that order at all. A larger-than-memory working set puts you on the far
  side of §4's cliff, where eviction — and its TLB shootdowns — runs
  continuously.

  </details>

- [ ] You can name the paper's four problems and the *three* bottlenecks behind the fourth, without inventing a fourth bottleneck.

  <details><summary>Answer</summary>

  The four problems are §3.1 transactional safety, §3.2 I/O stalls, §3.3
  error handling, §3.4 performance. The three bottlenecks §3.4 names behind
  the fourth are: **page table contention** (the OS must synchronize the page
  table, and it becomes highly contended with many concurrent threads);
  **single-threaded page eviction** (Linux evicts with the single `kswapd`
  process, which was CPU-bound in the paper's runs); and **TLB shootdowns**
  (thousands of cycles each, measured via `/proc/interrupts` in Fig. 2b).

  "4 KB granularity" is *not* on that list, though the temptation to add it
  is understandable — §2.2's `MADV_NORMAL` behaviour (fetch the accessed page
  plus the next 16 and previous 15, so 128 KB per fault at 4 KB pages) is a
  read-amplification argument, and it appears in the paper as a *madvise*
  discussion, not as a §3.4 bottleneck. Keep them separate: the first three
  are properties of the kernel's paging machinery under concurrency, the
  fourth is a tunable hint.

  </details>

- [ ] You can explain why a page fault the database cannot see is worse than a slow read it can, using this topic's own measured numbers.

  <details><summary>Answer</summary>

  Because the cost is bimodal and unannounced. The same load instruction in
  the same loop costs 42 ns at the median and 181,887 ns at the maximum
  ([FINDINGS.md](../../FINDINGS.md) row 6, full output in
  [`notes.md`](notes.md)) — a 4330× spread — and there is no call the engine
  can make beforehand to learn which it is about to pay. An explicit read is
  slower than 42 ns but it is *scheduled*: it can be issued asynchronously,
  batched with its neighbours, or overlapped with computation, all of which
  §3.2 says mmap cannot do because "mmap does not support asynchronous
  reads".

  The arithmetic says how little of the bad case it takes. With f the
  fraction of accesses that fault, the mean is `42 + (C − 42)·f`; at the
  measured p99 fault cost of 1500 ns the mean doubles at f = 42/1458 = 2.88%,
  and at the p99.9 cost of 4459 ns it doubles at f = 42/4417 = 0.95% — one
  access in a hundred. In the run as measured, the slowest 1% (20,000
  accesses at ≥ 1500 ns) accounts for at least 30 ms against the 84 ms an
  all-median run would have taken.

  </details>

- [ ] You can state what `mlock` does and does not guarantee, and why that single fact kills the simplest fix for problem #1.

  <details><summary>Answer</summary>

  `mlock` pins pages in memory so the OS will not evict them. §2.2 is
  explicit that this is *all* it does: "according to the POSIX standard (and
  Linux's implementation), the OS is permitted to flush dirty pages to the
  backing file at any time, even if the page is pinned."

  The simplest imagined fix for problem #1 is "lock the dirty pages until the
  log is flushed, then unlock them" — a two-line change that would make WAL
  ordering enforceable. It does not work, because locking controls
  *residency* and the WAL rule is about *write-back*, and mmap exposes no
  call that separates the two. That is why §3.1's three protocols are all
  structural — a private `MAP_PRIVATE` workspace, a user-space copy, or
  shadow paging — rather than a flag.

  </details>

- [ ] You can say which single event triggers every performance collapse in §4, and why it is the eviction side rather than the fault side.

  <details><summary>Answer</summary>

  The page cache filling up. In §4.1 mmap tracked fio for 27 seconds and then
  dropped to nearly zero for about five; in §4.2's single-SSD scan the drop
  came after about 17 seconds. Both are the moment the OS must start evicting
  to make room, and §4.1 says so directly: "This sudden drop in performance
  occurred when the page cache filled up, forcing the OS to begin evicting
  pages from memory."

  It is the eviction side because of the asymmetry in §2.1. Faulting a page
  *in* installs one translation in the page table and caches it in the
  faulting core's TLB — a local operation. Evicting must remove the
  translation from the page table *and* from every core's TLB, and since
  current CPUs provide no coherence for remote TLBs, the OS sends an
  inter-processor interrupt to each — a TLB shootdown, thousands of cycles
  (§3.4). So fault-in cost is independent of core count while eviction cost
  grows with it, which is why Fig. 2b's shootdown rate rises exactly where
  Fig. 2a's throughput falls.

  </details>

- [ ] You wrote answers to all four questions in notes.md, including naming the specific `crash_test.rs` case that would fail under mmap.

  <details><summary>Answer</summary>

  There is no answer to unfold here — tracing your own topic-5 tests against
  §3.1 is the exercise. The bar: name a test whose assertion is about
  *ordering* rather than content, because those are the ones an untimely
  kernel flush breaks. A test that crashes after a page is modified but
  before its log record is durable, then asserts recovery can undo the
  change, is asserting exactly the invariant §3.1 says the OS may violate
  "at any time, irrespective of whether the writing transaction has
  committed."

  An answer that says "all of them" has not done the work: tests that only
  check that committed data survives are fine under mmap, because `msync` at
  commit is enough for those.

  </details>

## References

**Papers**
- Crotty, Leis, Pavlo — *"Are You Sure You Want to Use MMAP in Your Database
  Management System?"* (CIDR 2022) —
  [PDF](https://db.cs.cmu.edu/papers/2022/p13-crotty.pdf) — 7 pages, one
  sitting. Memorize the four problems of §3; re-read §4.

| Section | What this chapter took from it |
|---|---|
| §2.1, Fig. 1 | the seven-stage access path; TLB shootdowns as IPIs, because CPUs give no coherence for remote TLBs |
| §2.2 | `MAP_SHARED`/`MAP_PRIVATE`; the three `madvise` hints and `MADV_NORMAL`'s 128 KB (page + next 16 + previous 15); `mlock` does not prevent flushes; `msync` |
| §2.3, Table 1 | ten mmap-based DBMSs and their years; MongoDB MMAPv1's removal; SingleStore's 10–20 ms per query on a shared mmap write lock |
| §3.1 | the OS may flush a dirty page at any time; the three update protocols (OS COW / user-space COW / shadow paging) and who uses each |
| §3.2 | no asynchronous reads; read-only queries can block on faults; `mlock`, `madvise` and prefetch threads as partial workarounds |
| §3.3 | checksums must be revalidated per access; corrupted pages persisted silently; SIGBUS mid-instruction |
| §3.4 + §4.1 | the three bottlenecks: page table contention, single-threaded `kswapd` eviction, TLB shootdowns at thousands of cycles |
| §4 preamble | EPYC 7713 (64c/128t), 512 GB RAM with 100 GB page cache, 10 × PM1733 SSDs at 7000 MB/s, Linux 5.11, fio 3.25 `O_DIRECT` |
| §4.1, Fig. 2 | ~900K reads/s for fio at 100 threads (≈100 µs NVMe latency); mmap's 27 s / ~5 s collapse / half-speed recovery; shootdown rate in Fig. 2b |
| §4.2, Figs. 3–4 | the single-SSD drop after ~17 s; ~20× gap on 10 SSDs; "2–20× worse than fio" |
| §5 | the authors' own endorsement of pointer swizzling as the right alternative |
| §6 | the prescription — when not to use mmap, and the two cases where you might |

**Measured in this repo**
- [FINDINGS.md](../../FINDINGS.md) row 6 — mmap page reads, p50 **42 ns**,
  max **182 µs**; full lane output and the handicap note in
  [`notes.md`](notes.md), lane source in
  `experiments/src/bin/pool_vs_mmap.rs`.

**Next**
- [`reading-leanstore-paper.md`](reading-leanstore-paper.md) — vmcache, the
  design that keeps mmap's addressing and takes back residency control.
- [`reading-postgres-bufmgr.md`](reading-postgres-bufmgr.md) — what the
  thousands of lines mmap promised to save actually look like.
