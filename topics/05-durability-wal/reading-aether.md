# Aether: one log, no bottleneck

On a multicore, the log is ONE shared object every transaction must append to
and flush — so how does it not become the bottleneck? Aether's answer is four
independent fixes that compose, and two of them are the ancestors of how
postgres inserts WAL today. Before the paper, this chapter builds the four
bottlenecks one at a time — in the authors' own lettering, which is not the one
this chapter used to give — and then each fix in order of increasing cleverness,
ending with the measurement that says which one actually mattered.

Every claim below was checked against the paper as published: **Ryan Johnson,
Ippokratis Pandis, Radu Stoica, Manos Athanassoulis & Anastasia Ailamaki,
"Aether: A Scalable Approach to Logging", *PVLDB* 3(1), VLDB 2010, pp. 681–692.**
Section, figure and table numbers are cited for each. Every timing attributed to
*this machine* comes from `experiments/src/bin/fsync_ladder.rs` as recorded in
`notes.md`.

## Vocabulary, defined once, before it is used

| Term | Meaning |
|---|---|
| **WAL** (write-ahead logging) | a change's log record is made durable before the changed page may reach nonvolatile storage; see `reading-aries.md` |
| **LSN** (log sequence number) | a log record's offset in the log; its position in the total order |
| **log buffer** | the in-memory staging area records are copied into before they are written out |
| **buffer acquire** | reserving a byte range of the log buffer — assigns the LSN, must be serial |
| **buffer fill** | copying the record's bytes into that reserved range — needs no serialization |
| **group commit** | letting one durability call cover many transactions' commit records |
| **ELR** (early lock release) | releasing a transaction's database locks at commit-record *creation* rather than *durability* (§3.1) |
| **flush pipelining** | detaching the worker thread from the commit wait; a daemon acks clients after the flush (§4.1) |
| **consolidation array** | an auxiliary array of slots where contending threads combine their buffer requests before touching the log mutex (§5.1) |
| **decoupled buffer fill** | releasing the log mutex immediately after buffer acquire, so fills pipeline (§5.2) |
| `fsync` / `fdatasync` / `F_FULLFSYNC` | the three durability rungs — see below; the paper says "log flush" and means whichever one your system uses |

**The durability ladder, measured here**, because "log flush latency" is the
paper's central quantity and a bare number for it is worthless:

| call | what it guarantees | p50 on this machine | implied ops/s |
|---|---|---|---|
| `write()` alone | bytes in the OS page cache — survives `kill -9`, not power loss | 1.17 µs | 856 898 |
| `fdatasync()` / macOS `fsync()` | bytes handed to the drive, whose volatile cache may still hold them | 22.67 µs | 44 109 |
| macOS `fcntl(fd, F_FULLFSYNC)` | the drive flushed its cache to stable media | 2.97 ms | 337 |

**19.4×** from the first rung to the second; a further **131×** to the third;
**2 542×** end to end. The middle row was measured on macOS as `fsync(2)` —
there is no `fdatasync` on this machine, `fsync_ladder.rs` compiles that lane
out — and it is named above only because it occupies the same rung on Linux.
Aether's own device series (§3.2) is a different ladder —
0 ms ramdisk (40–80 µs of kernel round trip), 100 µs "fast flash drive", 1 ms
"fast magnetic drive", 10 ms "slow magnetic drive" — and this machine's
`F_FULLFSYNC` rung, at 2.97 ms, sits between the paper's last two.

## The problem in one sentence

Every committing transaction must append to a single serial log and wait for it
to reach disk, so on a many-core machine the log is every thread funneling into
one mutex and one flush — and on this machine that flush is 22.67 µs or 2.97 ms
depending only on which system call you chose, which is enough to leave the
paper's 64-context server **75% idle** on lock contention alone (§1.1, Fig. 2).

## The concepts, step by step

### Step 1 — why the log must be serial, and what that costs on a multicore

> **In:** the recovery requirements of topic 5 — replay in log order, commit
> order equals log order.
> **Out:** a single append point and a single flush frontier, i.e. exactly the
> shape of object that does not scale with core count.

The entire recovery story of topic 5 rests on the log being one totally ordered
sequence: records are replayed in log order, commit order *is* log order, and a
record is durable only if everything before it is. Aether states the constraint
sharply while explaining decoupled buffer fill (§5.2): "Log records must be
written to disk in LSN order because **recovery must stop at the first gap it
encounters**; in the event of a crash any committed transactions beyond a gap
would be lost."

That total order is bought with physical serialization — one append point, one
flush frontier. On one core in 1992 this was free; on a multicore it turns the
log into the single object every transaction must touch twice (once to insert
its records, once to await the flush). Aether's contribution starts with
*naming* the distinct ways that hurts.

*Why it matters:* "the gap" is the reason none of the four fixes below is
allowed to reorder anything. Every one of them preserves LSN order and attacks
only *waiting*.

### Step 2 — the four bottlenecks, separated

> **In:** the single observation "commit is slow" on a 64-context server.
> **Out:** four distinct waits with four distinct causes — a disk, a lock table,
> a scheduler, and a mutex — each with its own fix and its own measurement.

The paper's abstract names them, and this chapter now uses the paper's letters.
(An earlier version of this file assigned A–D in a different order; if you
remember "A = the buffer mutex", relabel.) Quoting the abstract:

> "(a) the high volume of small-sized I/O requests may saturate the disk, (b)
> transactions hold locks while waiting for the log flush, (c) extensive context
> switching overwhelms the OS scheduler with threads executing log I/Os, and (d)
> contention appears as transactions serialize accesses to in-memory log data
> structures."

```
 txn commits ──► (a) one small I/O per commit          saturates the device
             ──► (b) locks held WHILE waiting for (a)  lock contention amplified
             ──► (c) block/unblock per commit          scheduler overload
             ──► (d) contend on the log buffer         one mutex around append
```

Figure 1 is the paper's own picture of this; §1.1 gives the measurements. Two
are worth carrying: with locks held across the flush the system is left **75%
idle** even at 60 clients on a 64-hardware-context Niagara II; scheduler
overload alone leaves it **20% idle** (§1.1, Fig. 2). Idle, not busy — that is
the signature of a waiting bottleneck rather than a computational one.

| Bottleneck | Aether's fix | Section | Modern descendant |
|---|---|---|---|
| (a) one I/O per commit | group commit (assumed, not the paper's contribution) | — | postgres `XLogFlush`'s three early exits |
| (b) locks held across the flush | **Early Lock Release** | §3 | shipped almost nowhere — see Step 4 |
| (c) scheduler overload | **flush pipelining** | §4 | postgres `synchronous_commit=off`; redis `everysec` (cruder — see Step 5) |
| (d) log-buffer contention | **consolidation array** (§5.1) and **decoupled buffer fill** (§5.2) | §5 | postgres reserve-then-copy = §5.2, *not* §5.1 |

*Why it matters:* the taxonomy is the paper's most reusable output. When your
own commit path is slow, the first question is which of the four letters it is,
because the four fixes are independent and three of them are cheap.

### Step 3 — fix (a): group commit — one flush covers N commits

> **In:** a stream of committing transactions arriving at rate λ, and a
> durability call costing T.
> **Out:** λ·T commits riding each call, so the device stops being the ceiling —
> while each commit still waits about T.

Group commit attacks the flush *count* by noticing that one durability call
makes durable *every* log record written before it, not just yours. Transactions
that arrive while a flush is in progress simply wait for the next one, which
covers all of them. Every serious engine does this; the paper takes it as given
and does not claim it.

**Do the arithmetic on real numbers**, because "an fsync costs about a
millisecond" is exactly the kind of unanchored figure that makes this reasoning
useless. Batch size is `λ·T`; throughput with group commit is `λ` (the flush is
no longer the constraint); throughput without it is `1/T`.

```
Top rung — macOS F_FULLFSYNC, T = 2.967 ms
  offered λ      batch = λ·T      ceiling without group commit
      1 000/s          2.97                337/s
      5 000/s         14.84                337/s
     20 000/s         59.34                337/s
    100 000/s        296.70                337/s

Middle rung — fdatasync / macOS fsync, T = 22.67 µs
    100 000/s          2.27             44 109/s

The old claim in this file was "1 ms per fsync and 32 waiting committers
⇒ ~32K commits/s". Neither number came from anywhere. The honest local
form of the same sentence:
    32 committers riding one F_FULLFSYNC = 32 × 337  =  10 784 commits/s
    32 committers riding one fdatasync   = 32 × 44109 = 1 411 488 commits/s
```

Read what the table says: the batch grows with offered load *by itself*, which
is why group commit is stable and needs no tuning. What it does **not** fix is
latency — each commit still waits about `T`, so on the top rung every commit
still eats 2.97 ms. That residual latency, held across your locks, is
bottleneck (b); held by a blocked thread, it is bottleneck (c). Postgres's
version of group commit — recheck the flushed LSN after acquiring the write
lock, so most backends find their work already done — is dissected in
`reading-postgres-xlog.md` Step 4.

*Why it matters:* group commit converts a per-commit cost into a per-batch cost,
which is why every remaining fix in this paper is about *latency* and
*contention* rather than device throughput.

### Step 4 — fix (b): Early Lock Release — stop holding locks through the flush

> **In:** a transaction whose commit record is in the log buffer but not yet on
> disk, still holding all its database locks.
> **Out:** the locks, released immediately — with recoverability preserved by
> the log's own total order rather than by waiting.

ELR releases a transaction's locks at commit-record *creation* rather than
commit-record *durability*, so the flush wait no longer blocks every transaction
queued on those locks. §3.1 attributes the observation to DeWitt et al. [4] and
states it with its caveat attached: a transaction's locks can be released before
its commit record is written to disk, **"as long as it does not return results
to the client before becoming durable."**

The safety argument is elegant, and it is the reason the serial log — the thing
that looked like pure bottleneck — pays for itself: "Serial log implementations
preserve this property naturally, because the dependant transaction's log
records must always reach the log later than those of the pre-committed
transaction and will therefore become durable later also." A crash that loses
your commit necessarily loses theirs.

§3.1 gives the formal conditions, from [21] — both must hold:

1. "Every dependant transaction's commit log record is written to the disk after
   the corresponding log record of pre-committed transaction."
2. "When a pre-committed transaction is aborted all dependant transactions must
   also be aborted." The paper notes most systems meet this trivially, because
   they "do no work after inserting the commit record, except to release locks."

The catch is condition 1 read carefully: it only covers effects that escape
through *the log*. A read-only transaction writes no commit record, so it has no
place in the total order and can leak unflushed state to a user.

**Why nobody shipped it.** The paper's own answer (§3.1) is better than the
vague one this chapter used to give: "modern database engines do not implement
ELR and to our knowledge this is the first paper to analyze empirically ELR's
performance. We hypothesize that this is largely due to the effectiveness of
asynchronous commit, which obviates ELR and which nearly all major systems do
provide." In other words the industry bought the same latency win by *giving up
durability* (postgres's `synchronous_commit=off`, redis's `everysec`) rather
than by reasoning about log order.

**What it is worth** (§3.2, Fig. 3, TPC-B on the 64-context Niagara II, zipfian
skew on the x-axis): ELR's speedup is "maximized (35x) for slower devices, but
remains substantial (2x) even with flash drives if contention is present." §1.2
gives the headline as **15%–164%** "even when logging to fast flash disks". The
35× figure is not a general claim about ELR — it is the high-skew, 10 ms-device
corner of one figure, and quoting it without both qualifiers is exactly the
error this chapter exists to avoid.

*Why it matters:* ELR is the topic's cleanest example of a serialization
constraint doubling as a correctness proof. Even if you never implement it, the
argument — "the log order already encodes the dependency, so I do not need to
wait to know about it" — recurs everywhere.

### Step 5 — fix (c): flush pipelining — the thread leaves, the commit stays

> **In:** a worker thread that is about to block on a durability call.
> **Out:** the same thread, immediately running the next transaction; a daemon
> that acks each client after the flush covering its commit lands — with the
> durability contract intact.

Flush pipelining (§4.1) decouples the *worker thread* from the *commit wait*:
instead of blocking, the worker detaches the transaction state, enqueues it, and
picks up new work; a daemon acknowledges each client after the flush covering
its commit record completes. Throughput of asynchronous commit, durability of
synchronous commit — the cost is added ack latency and a more complex scheduler,
**not** a loss window.

The measurements (§4.2): the baseline leaves **12 of 64** hardware contexts idle
at peak; flush pipelining reaches all **64** (Fig. 4), and delivers "up to 22%
higher performance" (Fig. 5).

Contrast redis's `appendfsync everysec` (`reading-redis-aof-rdb.md`): same
"don't block the worker" instinct, but it acks *before* durability and accepts
roughly a second of loss. Postgres's `synchronous_commit=off` is the same trade.
Flush pipelining is the version that keeps the contract — which is precisely why
it is more complicated.

**And it is the fix that mattered most.** §6.4, Fig. 9 (Shore-MT running TATP's
`UpdateLocation`): "For systems today, flush pipelining provides the largest
single performance boost, **68% higher than the baseline**. The scalable log
buffer adds a modest **7%** further speedup by eliminating log contention."
Note also the dependency: "flush pipelining depends on ELR to prevent
log-induced lock contention which would otherwise limit scalability" — which is
why Fig. 9's middle curve is labelled *FlushPipelining + ELR*, not flush
pipelining alone.

*Why it matters:* this inverts the usual reading of the paper. The famous idea
is the consolidation array; the idea that bought the throughput was scheduler
relief.

### Step 6 — fix (d): two different ways to stop contending on the log buffer

> **In:** N threads that each want to append a small record to one shared
> buffer, currently taking one mutex each and memcpying inside it.
> **Out:** two orthogonal designs — one that reduces *how many* threads enter
> the critical section, one that shortens *how long* each stays — and a hybrid.

With (a), (b) and (c) fixed, the remaining wall is the log buffer itself. §5
splits the work into two phases and observes that only the first is inherently
serial:

- **buffer acquire** — reserve a byte range, which assigns the LSN. Serial.
- **buffer fill** — copy the record in. "Buffer fill operations are not
  inherently serial (records never overlap)" (§5.2).

Two independent attacks follow. The paper's Figure 6 labels them, and this
chapter uses those labels:

```
 (B) Baseline:  T1 ─lock─ memcpy ─unlock─ T2 ─lock─ memcpy ─unlock─ T3 …

 (C) Consolidation array (§5.1) — fewer threads enter the critical section
     T1,T2,T3 meet in a slot, sum their sizes with CAS (no mutex),
     ONE of them takes the mutex and reserves sum(bytes) once,
     all three fill their own slices in parallel.
     "…effectively bounding contention at the log buffer to the number of
      array entries protecting the log buffer, rather than the number of
      threads in the system."                                        (§5.1)
     Residual cost: groups are still serialized against each other.

 (D) Decoupled buffer fill (§5.2) — the critical section gets shorter
     Every thread takes the mutex, reserves its own range, and RELEASES THE
     MUTEX IMMEDIATELY; the memcpy happens outside. Fills pipeline.
     Cost: buffer *release* becomes a second serialization point, because
     regions must be released in LSN order — "recovery must stop at the
     first gap it encounters". No mutex needed, but each thread waits for
     its predecessor to release.

 (CD) Hybrid (§5.3) — both; bounded contention AND maximum pipelining.
```

The underlying principle is one sentence: decouple *sequencing* (assigning log
offsets — must be serial, so make it tiny) from *copying* (moving bytes —
needn't be serial, so make it parallel).

**Which one is postgres?** §5.2, not §5.1 — and this chapter used to say the
opposite. `ReserveXLogInsertLocation` (`xlog.c:1172–1184`) holds a spinlock for
one addition and four field moves, then releases it and copies outside; the
format conversions happen at `:1182–1184`, deliberately after the release. That
is decoupled buffer fill exactly. Postgres's answer to §5.2's release-in-order
requirement is `WaitXLogInsertionsToFinish`, which is why every insertion lock
publishes an `insertingAt` value.

The 8 WAL insertion locks (`NUM_XLOGINSERT_LOCKS` = 8, `xlog.c:157`) are *not* a
consolidation array. Compare how a thread finds its slot:

| | Aether's consolidation array | postgres's insertion locks |
|---|---|---|
| how you pick a slot | `idx = randn(ARRAY_SIZE)` — probe at random (Algorithm 5 line 3, Appendix A.2) | `MyProcNumber % NUM_XLOGINSERT_LOCKS` on first use, then reuse the same one for cache affinity (`xlog.c:1429–1431`) |
| on contention | join whatever OPEN slot you find; state machine FREE→OPEN→PENDING→COPYING→DONE (§A.2) | move to the next lock, `lockToTry = (lockToTry + 1) % 8` (`xlog.c:1448`), so inserters migrate apart |
| what it bounds | the number of threads reaching the mutex, to the array size | nothing — it partitions the waiting, it does not combine requests |
| requests combined? | **yes** — one reservation serves the whole group, "two or three atomic operations per participating thread" (§A.2) | **no** — every backend makes its own reservation |
| how many | peak performance at **3–4 slots** (§A.4, Fig. 12); the paper fixes it at four | 8, fixed |

Postgres got §5.2 and a lock-partitioning scheme; it did not get §5.1.

**What the log buffer is worth, in the paper's own numbers.** §6.3.1: the
average record in their workloads is about **120 B**, and "a high-performance
application generates between 100 and 200MBps of log, or between 800K and 1.6M
log insertions per second" — check the division, 100 MB/s ÷ 120 B = 833 K/s. The
baseline log buffer peaks at roughly **140 MB/s** and then *falls* as contention
grows. The abstract's headline is over **1.8 GB/s** for small records — an order
of magnitude past the baseline. §6.3.2, Fig. 8(right): (C) wins below ~1 kB
records where contention dominates, (D) wins above it where copy cost does, and
the hybrid beats both across the range until all three saturate the memory
system; with the records kept L1-resident the hybrid scales to about **21 GB/s**
before becoming CPU-limited.

*Why it matters:* this is the fix that attacks the only bottleneck that *grows
with core count* — (a) is constant per device, (c) is bounded by the scheduler,
but (d) gets worse with every core you add. §6.4's own conclusion is that it is
worth only 7% today and that "this bottleneck is growing rapidly with core
counts and will soon dominate."

## How to read the paper (with the concepts in hand)

1. **Abstract and §1.1** for the bottleneck taxonomy in the authors' letters
   (Step 2's table). Fig. 1 is the map; Fig. 2 is the evidence.
2. **§6.4 and Fig. 9 next, out of order** — 68% from flush pipelining, 7% from
   the log buffer. Knowing the scoreboard before you read the mechanisms stops
   you from over-weighting the clever one.
3. **§3** — ELR (Step 4). Read it for the *argument*: log order as a free
   dependency tracker, plus the two formal conditions and the
   "does not return results to the client" caveat.
4. **§4** — flush pipelining (Step 5). Short, and the fix that mattered.
5. **§5** — the log buffer. Read §5.1 and §5.2 as *two* designs, and hold
   Figure 6's four panels (B, C, D, CD) in view; then read
   `reading-postgres-xlog.md` Step 2 beside §5.2, not §5.1.
6. **Appendix A.2 and A.4** if you intend to build one: the slot state machine,
   Algorithm 5, and the finding that 3–4 slots is the peak.

## Questions to answer in notes.md

1. Why does ELR NOT violate durability for the *dependent* transaction? State
   the paper's two formal conditions (§3.1) and say which one the serial log
   satisfies for free.
2. ELR hazard: what if the dependent transaction's result escapes to the user by
   a channel other than its own commit ack — say a read-only transaction that
   never logs? Relate this to the paper's caveat, "as long as it does not return
   results to the client before becoming durable."
3. Consolidation array (§5.1) versus postgres's 8 insertion locks: both reduce
   time spent in the critical section, but only one *combines* requests. Work
   out what each does under 8 writers and under 80, and explain why the paper
   found 3–4 slots optimal (§A.4) while postgres uses 8 locks.
4. Redo Step 3's group-commit table for the durability rung *you* intend to ship
   on. At what offered load does the batch first exceed 10? What does that imply
   about when group commit is worth implementing at all?
5. Which bottleneck does your M5 group-commit design leave unfixed? (Likely (d)
   — a single mutex around the WAL buffer is fine at graph-workload commit rates;
   say at what commits/s it wouldn't be, using Step 6's 120 B / 800 K–1.6 M
   inserts-per-second figures as the yardstick.)

## Done when

Answer each before unfolding it.

- [ ] Name the four bottlenecks in the paper's own lettering, and say what kind
      of resource each one is.

  <details><summary>Answer</summary>

  (a) high volume of small I/O requests saturating the disk — a *device*; (b)
  transactions holding locks while waiting for the log flush — a *lock table*;
  (c) context switching overwhelming the OS scheduler — a *scheduler*; (d)
  contention serializing access to in-memory log data structures — a *mutex*.
  Straight from the abstract. Four different resources is the point: the fixes
  are independent and compose.

  </details>

- [ ] Which of Aether's fixes bought the most throughput, and by how much?

  <details><summary>Answer</summary>

  Flush pipelining — "the largest single performance boost, 68% higher than the
  baseline" (§6.4, Fig. 9), with the scalable log buffer adding "a modest 7%
  further speedup". Two caveats: that is on Shore-MT running TATP
  `UpdateLocation` on a 64-context Niagara II, and flush pipelining "depends on
  ELR to prevent log-induced lock contention", so the 68% curve is
  FlushPipelining + ELR. The paper expects the ranking to invert as core counts
  rise.

  </details>

- [ ] Sketch a consolidation array, and say what it bounds.

  <details><summary>Answer</summary>

  Contending threads back off to an array of slots, join one at random
  (`idx = randn(ARRAY_SIZE)`, Algorithm 5), and CAS their sizes together; the
  first thread in acquires the mutex once and reserves the *group's* total, then
  every member fills its own slice in parallel and the last one out releases the
  region. It bounds "contention at the log buffer to the number of array entries
  protecting the log buffer, rather than the number of threads in the system"
  (§5.1) — a constant instead of something that grows with core count. Peak at
  3–4 slots (§A.4).

  </details>

- [ ] Point at the postgres code that embodies Aether's log-buffer work — and
      name the right section.

  <details><summary>Answer</summary>

  `ReserveXLogInsertLocation` (`xlog.c:1172–1184`): a spinlock held for one
  addition and four field moves, with the conversions and the record copy done
  after the release. That is **§5.2, decoupled buffer fill**, not §5.1's
  consolidation array — postgres does not combine requests. Its answer to
  §5.2's release-in-LSN-order requirement is `WaitXLogInsertionsToFinish` plus
  the per-lock `insertingAt` values. The 8 insertion locks
  (`NUM_XLOGINSERT_LOCKS`, `xlog.c:157`) partition waiting via
  `MyProcNumber % 8` with migration on contention (`xlog.c:1429–1448`); they are
  a lock array, not a consolidation array.

  </details>

- [ ] Group commit converts a per-commit cost into a per-batch cost. On this
      machine's top rung, how many transactions ride one flush at 20 000
      commits/s offered, and what is the ceiling without group commit?

  <details><summary>Answer</summary>

  Batch = λ·T = 20 000 × 2.967 ms = **59.3** transactions per flush. Without
  group commit the ceiling is 1/T = **337 commits/s** flat, regardless of
  offered load. Note what group commit does not fix: each commit still waits
  about 2.97 ms, and that residual wait is bottlenecks (b) and (c).

  </details>

- [ ] Why must the log buffer's *release* be serialized even after §5.2 removes
      the mutex from the fill?

  <details><summary>Answer</summary>

  Because regions must be released in LSN order: "Log records must be written to
  disk in LSN order because recovery must stop at the first gap it encounters;
  in the event of a crash any committed transactions beyond a gap would be lost"
  (§5.2). No mutex is required, but each thread must wait for its predecessor to
  release before releasing its own region — which is why postgres publishes an
  `insertingAt` value per insertion lock and has `WaitXLogInsertionsToFinish`.

  </details>

## References

**Paper** — Johnson, Pandis, Stoica, Athanassoulis & Ailamaki, "Aether: A
Scalable Approach to Logging", *Proceedings of the VLDB Endowment* 3(1), 2010,
pp. 681–692.

| section / figure | what this chapter took from it |
|---|---|
| Abstract | the (a)–(d) lettering; 20–69% end-to-end; >1.8 GB/s log insert (Steps 2, 6) |
| §1.1, Figs. 1–2 | 75% idle from lock contention, 20% from scheduler overload (Step 2) |
| §1.2 | ELR worth 15%–164% on fast flash (Step 4) |
| §3.1 | ELR's definition, the DeWitt attribution [4], the client-results caveat, the two conditions from [21], and why nobody shipped it (Step 4) |
| §3.2, Fig. 3 | 35× on slow devices, 2× on flash; the 0/100 µs/1 ms/10 ms device series (Steps 4, and the ladder above) |
| §4.1–§4.2, Figs. 4–5 | flush pipelining; 12-of-64 idle contexts → 64; up to 22% (Step 5) |
| §5.1, §A.2, Algorithm 5 | the consolidation array; contention bounded to the array size; random slot probing (Step 6) |
| §5.2 | decoupled buffer fill; release-in-LSN-order; "recovery must stop at the first gap" (Steps 1, 6) |
| §5.3, Fig. 6 | the (B)/(C)/(D)/(CD) panels and the hybrid (Step 6) |
| §6.1 | platform and method — Sun T5220, Solaris 10, TATP 100K subscribers, TPC-B 100 tellers, ten 30 s runs, all within 2% |
| §6.3.1–§6.3.2, Fig. 8 | 120 B average record, 800 K–1.6 M inserts/s, 140 MB/s baseline, ~21 GB/s in L1 (Step 6) |
| §6.4, Fig. 9 | 68% from flush pipelining, 7% from the log buffer (Steps 5, 6) |
| §A.4, Fig. 12 | peak at 3–4 slots; the paper fixes the array at four (Step 6) |

**Code** — postgres/postgres@701f021: `src/backend/access/transam/xlog.c`
(`ReserveXLogInsertLocation` `:1172–1184`, `NUM_XLOGINSERT_LOCKS` `:157`,
`WALInsertLockAcquire` `:1411–1450`). Read alongside
`reading-postgres-xlog.md`.

**Measurements** — `topics/05-durability-wal/notes.md`, "Baseline (provided lane,
Apple M3 Pro / APFS, measured 2026-07-28)", from
`experiments/src/bin/fsync_ladder.rs`; headline in `FINDINGS.md` row 5.
