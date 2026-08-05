# One word, one CAS, one queue: postgres's production rwlock

lwlock.c is the latch under every buffer, WAL insert, and proc-array scan
you met in topics 5–8. One u32 of state, a CAS fast path, and an intrusive
wait queue — read it as the reference answer to "how do I build a
reader-writer lock that doesn't melt at 128 cores". This chapter builds the
lock one concept at a time — what a latch even is, the packed state word,
the CAS fast path, the wait queue, the lost-wakeup race the whole design
orbits, and the padding that keeps neighbouring locks off each other's
cache lines — then pins each piece to its line in the file.

Everything below is read at the pinned commit **`postgres/postgres@701f021`**
(`python3 tools/pinned-source.py ref postgres`). `lwlock.c` is 1939 lines
there. Line numbers in other releases will differ; three of the flag names
changed as recently as PG 17, so check before you trust a number from a
blog post.

## The problem in one sentence

A postgres sequential scan takes and releases a reader-writer lock on
**every buffer it touches** — millions of acquisitions per second per core,
each held for tens of nanoseconds — so a lock whose fast path costs a
syscall (~1 µs) or even one extra contended cache line would cost more than
the work it protects. Topic 9's own `scaling` lane measures exactly that
failure in miniature: one global `Mutex` around a `BTreeSet` runs at
**8.65 Mops/s on one thread and 2.96 Mops/s on sixteen** — 2.9× *slower*
with 16× the hardware.

## The concepts, step by step

### Step 1 — a latch: a nanosecond-scale reader-writer lock

> **In:** a shared data structure that many backends read and few mutate.
> **Out:** the vocabulary — lock, latch, reader-writer lock, spinlock, futex
> — and the reason database people insist on the lock/latch distinction.

A **reader-writer lock** (rwlock) admits many simultaneous readers OR one
exclusive writer — the right shape for data that is read constantly and
written rarely.

Database papers split the word "lock" in two, and the split is not
pedantry, it is two different subsystems:

| | **lock** (topic 8) | **latch** (this file) |
|---|---|---|
| protects | *logical* content — a row, a range | *physical* integrity — a page, a list |
| held for | milliseconds to seconds | tens of nanoseconds |
| deadlock | detected, waits-for graph | avoided by protocol, never detected |
| recursion | allowed | banned (question 3) |
| lives in | the lock manager, hash table of lock tags | one word next to the object |

The Bw-tree papers state the convention outright: "in this paper, we always
use the term 'lock' when referring to 'latch'" (*Buzz Words*, SIGMOD 2018,
footnote 1 on p. 1). Read "latch-free" and "lock-free" as the same word.

Two more terms you need before the code. A **spinlock** is a lock whose
waiter burns CPU re-reading the lock word instead of sleeping — correct
only when the hold time is far shorter than the cost of sleeping. A
**futex** ("fast userspace mutex") is the Linux primitive that lets a lock
be a plain word in memory *until* it is contended, at which point one
syscall parks the waiter on that address; `std::sync::Mutex` on Linux is
built on it. Postgres does not use a futex — its waiters park on a per-backend
SysV semaphore (`PGSemaphoreLock(proc->sem)`, `lwlock.c:1269`), because the
queue has to work between *processes* sharing a memory segment, not threads
in one address space.

**LWLock** — "lightweight lock" — is postgres's latch: the thing under every
buffer pin, WAL insert, and proc-array scan from topics 5–8. "Lightweight"
is relative to the heavyweight lock manager, not to a mutex.

### Step 2 — the packed state word

> **In:** a lock that must record "free / N readers / one writer / someone
> is queued" and be updatable atomically.
> **Out:** why all of that fits in one `uint32`, and what each bit is.

**CAS** (compare-and-swap) is the atomic CPU instruction "replace this one
word with a new value only if it still equals the value I read". It updates
exactly ONE word atomically. Everything else in this file follows from that
constraint: if the whole lock state does not fit in one word, no single
instruction can move the lock from one legal state to another, and you need
a lock to protect your lock.

So the design's first move is to make the entire state fit in one u32:

```c
/* lwlock.c:96-108 — the whole lock state, one word */
    96  #define LW_FLAG_HAS_WAITERS			((uint32) 1 << 31)
    97  #define LW_FLAG_WAKE_IN_PROGRESS	((uint32) 1 << 30)
    98  #define LW_FLAG_LOCKED				((uint32) 1 << 29)
    99  #define LW_FLAG_BITS				3
   100  #define LW_FLAG_MASK				(((1<<LW_FLAG_BITS)-1)<<(32-LW_FLAG_BITS))
   101
   102  /* assumes MAX_BACKENDS is a (power of 2) - 1, checked below */
   103  #define LW_VAL_EXCLUSIVE			(MAX_BACKENDS + 1)
   104  #define LW_VAL_SHARED				1
   105
   106  /* already (power of 2)-1, i.e. suitable for a mask */
   107  #define LW_SHARED_MASK				MAX_BACKENDS
   108  #define LW_LOCK_MASK				(MAX_BACKENDS | LW_VAL_EXCLUSIVE)
```

Read the layout off those eight lines:

```
 u32 state, high bits first:
 ┌──────────────┬───────────────────┬──────────────┬───────────────────────┐
 │ 31 HAS_      │ 30 WAKE_IN_       │ 29 LOCKED    │ 28..0  LW_LOCK_MASK   │
 │    WAITERS   │    PROGRESS       │  (wait list) │  = count + EXCLUSIVE  │
 └──────────────┴───────────────────┴──────────────┴───────────────────────┘
 exclusive = add LW_VAL_EXCLUSIVE (= MAX_BACKENDS+1); shared = add 1.
 "is it free for a writer?"  (state & LW_LOCK_MASK) == 0     — one load.
 "is it free for a reader?"  (state & LW_VAL_EXCLUSIVE) == 0 — one load.
```

The trick: shared holders are a *count* (each reader adds 1), and taking it
exclusive adds `LW_VAL_EXCLUSIVE = MAX_BACKENDS + 1` — a value no count of
readers can reach, because there are at most `MAX_BACKENDS` backends. So one
masked compare distinguishes "free", "readers", and "writer", and both
acquire modes are an *add*, not a branchy read-modify-write. Same trick as
postgres's buffer state (topic 6) and Hekaton's `end_ts`-as-lock (topic 8):
pack refcount and flags into one atomic word so every protocol step is a
single CAS.

Bit-packing this tight demands tests, and the file has them as compile-time
assertions — `LW_VAL_EXCLUSIVE` must not overlap the flag bits (`:117-118`),
`MAX_BACKENDS` must not either (`:114-115`), and `MAX_BACKENDS + 1` must be
a power of two (`:111-112`). If someone raises `MAX_BACKENDS` past 2²⁹ the
build fails rather than the lock silently mistaking a reader count for the
`LOCKED` flag.

**Three flags, and the middle one is not what older write-ups call it.**
At `701f021` the second flag is `LW_FLAG_WAKE_IN_PROGRESS`; guides written
against PG ≤ 16 call it `LW_FLAG_RELEASE_OK` and give it the opposite sense.
Same job, inverted polarity — see Step 6.

### Step 3 — the fast path is a CAS loop, and it always writes

> **In:** the state word from Step 2 and a mode (shared or exclusive).
> **Out:** `LWLockAttemptLock`, and the measured cost of the cache line it
> touches.

With the state in one word, acquiring in the uncontended case is one CAS
loop and nothing else — no syscall, no queue touch, no allocation:

```c
/* lwlock.c:774-808, comments elided — the entire fast path */
   774  	old_state = pg_atomic_read_u32(&lock->state);
   776  	/* loop until we've determined whether we could acquire the lock or not */
   777  	while (true)
   778  	{
   782  		desired_state = old_state;
   784  		if (mode == LW_EXCLUSIVE)
   785  		{
   786  			lock_free = (old_state & LW_LOCK_MASK) == 0;
   787  			if (lock_free)
   788  				desired_state += LW_VAL_EXCLUSIVE;
   789  		}
   790  		else
   791  		{
   792  			lock_free = (old_state & LW_VAL_EXCLUSIVE) == 0;
   793  			if (lock_free)
   794  				desired_state += LW_VAL_SHARED;
   795  		}
   807  		if (pg_atomic_compare_exchange_u32(&lock->state,
   808  										   &old_state, desired_state))
```

`LWLockAttemptLock` returns `false` when it got the lock and `true` when it
must wait (`:817`, `:820`) — read the return as "mustwait", which is what
the caller names it.

**A claim to unlearn: a shared acquisition does *not* leave the cache line
shared.** It is tempting to say that on a read-mostly lock every reader can
keep the line in the Shared state and nobody pays coherence. That is false
here, twice over. First, a reader *increments* the count, which is a write.
Second, the comment at `:797-806` says the code deliberately swaps in the
value even when it saw the lock as busy, "the reason that we always swap in
the value is that this doubles as a memory barrier". Every acquisition,
shared or exclusive, successful or not, is a `compare_exchange` — an atomic
read-modify-write that must take the line exclusive on the acquiring core
and invalidate every other copy.

That is the traffic this topic measures. A **cache line** is the unit the
memory system moves and owns — 64 B on x86-64, 64 B (with 128 B pairing,
Step 7) on Apple M-series. The **coherence protocol** (MESI: Modified /
Exclusive / Shared / Invalid) is the hardware rule that a line may be
Modified on at most one core at a time; to write, a core must first get the
others to Invalidate their copies. **False sharing** is when two logically
independent variables land in the same line and therefore fight over that
one ownership token.

Work it on this machine's numbers. The `false_sharing` lane has 8 threads
each doing 5 000 000 `fetch_add`s on *its own* counter:

```
                       total time   per increment   what the line is doing
  packed (8 × u64)      202.7 ms      40.54 ns      one line, 8 owners
  pad64  (64 B apart)    20.4 ms       4.08 ns      one line each, sort of
  pad128 (128 B apart)   11.4 ms       2.28 ns      one line each, really

  cost of one ownership transfer = 40.54 − 2.28 = 38.3 ns
```

Now price a *contended* LWLock with that. Every acquire and every release
is one RMW on `lock->state`. If two backends on different cores are hitting
the same lock, each pays ~38 ns of coherence on top of the ~2 ns the
instruction would cost uncontended — 19× — and the lock protects work
measured in *tens* of nanoseconds. This is why the file's entire fast path
is one word: not to save memory, but because every extra word touched is
another line to drag across the interconnect.

The `scaling` lane puts a number on the handoff, too. A global mutex runs
at 8.65 Mops/s single-threaded — 115.6 ns per operation, all of it real
work plus an uncontended lock. At 16 threads it runs at 2.96 Mops/s —
337.8 ns per operation. The extra **222 ns per operation** is pure handoff:
about six line transfers at 38.3 ns each, and comfortably *under* the ~1 µs
a syscall would cost, which tells you most of those handoffs never park the
waiter at all. They spin, bounce the line, and win. That is what a lock
costs when it is doing nothing wrong.

### Step 4 — the wait list, and the lock inside the lock

> **In:** a failed `LWLockAttemptLock` — the thread has to wait.
> **Out:** where the waiter record lives, and why guarding the queue costs
> no extra cache line.

Waiting needs a queue of waiters. An **intrusive list** embeds the links
inside a structure that already exists instead of allocating a node, and
that is what postgres uses: the links live in the waiter's own `PGPROC`.
`LWLockQueueSelf` (`:1018`) pushes the current backend onto
`lock->waiters`.

Two details make this shared-memory-correct rather than merely tidy:

- The list links are **backend index numbers, not pointers**
  (`src/include/storage/proclist_types.h:28-42`). The proc array is mapped
  at a different virtual address in every backend, so a pointer written by
  one backend would be meaningless to another. An index into the array is
  not.
- There is no allocation anywhere on the slow path. A lock taken millions
  of times per second cannot afford `malloc` on its unhappy path either.

The queue must itself be mutated atomically, and here the file does
something worth stealing: **the wait-list guard is a bit in the same word**,
not a separate spinlock object.

```c
/* lwlock.c:845-866, LWLockWaitListLock — test-and-test-and-set on LW_FLAG_LOCKED */
   845  	while (true)
   846  	{
   847  		/*
   848  		 * Always try once to acquire the lock directly, without setting up
   849  		 * the spin-delay infrastructure. ...
   850  		 */
   852  		old_state = pg_atomic_fetch_or_u32(&lock->state, LW_FLAG_LOCKED);
   853  		if (likely(!(old_state & LW_FLAG_LOCKED)))
   854  			break;				/* got lock */
   855
   856  		/* and then spin without atomic operations until lock is released */
   857  		{
   858  			SpinDelayStatus delayStatus;
   860  			init_local_spin_delay(&delayStatus);
   862  			while (old_state & LW_FLAG_LOCKED)
   863  			{
   864  				perform_spin_delay(&delayStatus);
   865  				old_state = pg_atomic_read_u32(&lock->state);
   866  			}
```

Line 852 is the atomic attempt; line 865 is a **plain load**. That shape —
one atomic try, then spin on ordinary reads until the word looks free —
is *test-and-test-and-set*, and its whole purpose is coherence traffic. A
naive spinlock re-runs the atomic RMW in the loop, so N spinners generate N
ownership transfers per iteration; here the spinners sit in the Shared
state reading their own cached copy and generate **zero** traffic until the
holder's release invalidates them once. With the 38.3 ns figure from Step
3: 16 spinners × 1000 iterations costs ~610 µs of interconnect the naive
way and essentially nothing this way.

`perform_spin_delay` (`:864`, defined in `s_lock.c`) escalates: a few
`pg_spin_delay()` pause instructions, then `pg_usleep`, doubling. It also
bumps `spin_delay_count` (`:246`), which `LWLOCK_STATS` builds print — the
file makes contention *observable*, which is the point topic 0 keeps
making about measurement.

### Step 5 — the lost wakeup, and the double-check dance

> **In:** a waiter that has failed the fast path and wants to sleep.
> **Out:** the interleaving that would lose its wakeup, and the enqueue →
> re-attempt → sleep ordering that closes it.

Here is the race the whole file orbits. The naive slow path is: attempt,
fail, enqueue, sleep. Now interleave two backends:

```
  T1 (waiter)                          T2 (holder)
  ─────────────────────────────────    ─────────────────────────────────
  LWLockAttemptLock  → mustwait
                                       release: state -= LW_VAL_EXCLUSIVE
                                       HAS_WAITERS not set → wake nobody
                                       (T2 leaves; lock is FREE)
  LWLockQueueSelf                      ...
  sleep on proc->sem                   ...
  ── sleeps forever on a free lock ──
```

That is a **lost wakeup**. The fix is to make the queue entry visible
*before* the last check, so that any release able to slip into the gap is
forced to see it:

```c
/* lwlock.c:1207-1247 — the loop in LWLockAcquire, comments elided */
  1207  	for (;;)
  1208  	{
  1215  		mustwait = LWLockAttemptLock(lock, mode);
  1217  		if (!mustwait)
  1218  		{
  1220  			break;				/* got the lock */
  1221  		}
  1234  		/* add to the queue */
  1235  		LWLockQueueSelf(lock, mode);
  1237  		/* we're now guaranteed to be woken up if necessary */
  1238  		mustwait = LWLockAttemptLock(lock, mode);
  1240  		/* ok, grabbed the lock the second time round, need to undo queueing */
  1241  		if (!mustwait)
  1242  		{
  1245  			LWLockDequeueSelf(lock);
  1246  			break;
  1247  		}
```

The file's own comment at `:1223-1232` states the invariant precisely: "if
we still couldn't grab it, we know that the other locker will see our queue
entries when releasing since they existed before we checked for the lock."

Read it as an ordering argument, not as luck. `LWLockQueueSelf` sets
`LW_FLAG_HAS_WAITERS` in the state word; `LWLockRelease` reads the state
word as it subtracts. Both touch the same u32. So either T1's
`HAS_WAITERS` write lands before T2's release-read (T2 sees a waiter and
wakes it), or it lands after — in which case T2's decrement landed before
T1's second `LWLockAttemptLock`, and T1 sees a free lock and takes it.
There is no third case, because a single memory location has a single
modification order. The comment at `:1237` — "we're now guaranteed to be
woken up if necessary" — is that argument in five words.

`LWLockDequeueSelf` (`:1061`) is the undo for the "won on the recheck"
branch, and it is not free: it has to take the wait-list lock and walk the
list. That is fine, because it only runs when a release landed in a window
a few nanoseconds wide.

Two smaller facts that fall out of the same design:

- `LWLockQueueSelf` **PANICs** if the backend is already queued on another
  lock (`:1028-1029`). One `PGPROC`, one queue link, so one wait at a time.
- The sleep at `:1267-1273` loops. Semaphores can be signalled for other
  reasons, so waking is not proof of acquisition; the code re-checks
  `proc->lwWaiting` and counts `extraWaits` to put back afterwards.

### Step 6 — release, batched wakeups, and the fairness you do *not* get

> **In:** a held lock and a queue of waiters.
> **Out:** what `LWLockRelease` actually promises about ordering — which is
> much less than "arrival order".

Release is not a CAS loop. It is a single atomic subtract, and then a
decision made from the value that subtract returned:

```c
/* lwlock.c:1793-1815 — release, then decide whether anyone needs waking */
  1793  	/*
  1794  	 * Release my hold on lock, after that it can immediately be acquired by
  1795  	 * others, even if we still have to wakeup other waiters.
  1796  	 */
  1797  	if (mode == LW_EXCLUSIVE)
  1798  		oldstate = pg_atomic_sub_fetch_u32(&lock->state, LW_VAL_EXCLUSIVE);
  1799  	else
  1800  		oldstate = pg_atomic_sub_fetch_u32(&lock->state, LW_VAL_SHARED);
  1808  	/*
  1809  	 * Check if we're still waiting for backends to get scheduled, if so,
  1810  	 * don't wake them up again.
  1811  	 */
  1812  	if ((oldstate & LW_FLAG_HAS_WAITERS) &&
  1813  		!(oldstate & LW_FLAG_WAKE_IN_PROGRESS) &&
  1814  		(oldstate & LW_LOCK_MASK) == 0)
  1815  		check_waiters = true;
```

Three things to take from that.

**The `WAKE_IN_PROGRESS` flag is a wakeup-storm damper.** A backend that
has been signalled is not running yet — it is on a run queue. Without the
flag, every release in that window would take the wait-list lock and signal
the same waiter again. `LWLockWakeup` sets the flag when it dispatches
signals; the woken backend clears it when it loops round to retry
(`:1276`, `pg_atomic_fetch_and_u32(&lock->state, ~LW_FLAG_WAKE_IN_PROGRESS)`).
Older postgres spelled this `LW_FLAG_RELEASE_OK` with the sense inverted —
same mechanism, complemented bit.

**Wakeups are batched by mode.** `LWLockWakeup` (`:904`) walks the queue
under the wait-list lock and wakes a *run* of waiters: it stops adding
after the first exclusive waiter (`:954-955`) and skips further exclusive
waiters behind it (`:920-921`). So a released lock hands a whole block of
readers through at once, and never signals two writers who can only
serialise.

**Postgres LWLocks are not FIFO-fair. They allow barging.** The guide you
are replacing said waiters are served in arrival order; the code says
otherwise, in three places:

1. `LWLockAttemptLock` never inspects `LW_FLAG_HAS_WAITERS` (re-read Step
   3 — there is no such test). A backend arriving fresh takes the lock if
   the count is zero, no matter how many backends are queued.
2. `:1793-1795`: "after that it can immediately be acquired by others, even
   if we still have to wakeup other waiters."
3. The design NOTE at `:1195-1205` chose this deliberately: handing the
   lock to the woken waiter "means a process swap for every lock
   acquisition when two or more processes are contending", and since a
   backend must be able to "acquire and release the same lock many times
   during a single CPU time slice", throughput beats fairness. The
   reference is to a pgsql-hackers thread from 29-Dec-01.

What the queue *does* buy is that a queued waiter is eventually signalled
and re-tries, so no waiter is silently dropped — and among waiters that are
already queued, `LWLockWakeup` walks the list in order
(`src/backend/storage/lmgr/README:29-30` describes this intent). Progress,
not fairness. If you need FIFO under sustained load you build it on top; you
do not get it from here.

### Step 7 — padding: postgres pads to 128 bytes, and so should you

> **In:** an array of thousands of LWLocks (one per buffer, per WAL insert
> slot, per lock-manager partition).
> **Out:** why each gets its own line, and why 64 B is the wrong number on
> this machine.

Steps 2–6 shrank the lock to one word. That creates a new hazard: 512
LWLocks now fit in one 64 B run of memory, and two *unrelated* locks
sharing a cache line contend as hard as one lock would. That is false
sharing, and postgres avoids it by padding:

```c
/* src/include/storage/lwlock.h:62-72 — every lock gets a whole line */
    62  #define LWLOCK_PADDED_SIZE	PG_CACHE_LINE_SIZE
    63
    64  StaticAssertDecl(sizeof(LWLock) <= LWLOCK_PADDED_SIZE,
    65  				 "Miscalculated LWLock padding");
    66
    67  /* LWLock, padded to a full cache line size */
    68  typedef union LWLockPadded
    69  {
    70  	LWLock		lock;
    71  	char		pad[LWLOCK_PADDED_SIZE];
    72  } LWLockPadded;
```

The actual `LWLock` is 12 bytes (`lwlock.h:41-50`: a `uint16` tranche, a
`pg_atomic_uint32` state, a `proclist_head` of waiters). The union pads it to
`PG_CACHE_LINE_SIZE`, and the comment at `:52-61` gives both reasons —
alignment "ensures that individual LWLocks don't cross cache line
boundaries", and "in some cases, it's useful to add even more padding so
that each LWLock takes up an entire cache line… for example, in the main
LWLock array, where the overall number of locks is small but some are
heavily contended."

So what is a cache line, according to postgres?

```c
/* src/include/pg_config_manual.h:217 — with the reasoning at :208-215 */
   217  #define PG_CACHE_LINE_SIZE		128
```

The comment above it is explicit that this is a chosen upper bound, not a
measurement: "Too small a value can hurt performance due to false sharing,
while the only downside of too large a value is a few bytes of wasted
memory. The default is 128, which should be large enough for all supported
platforms." Postgres spends **10.7× the size of the lock** on padding, on
purpose.

**Postgres's 128 is right and the folklore 64 is wrong, and this topic
measured it.** Re-read the table in Step 3. Padding the eight counters to
64 B apart — one *nominal* cache line each — takes the lane from 202.7 ms
to 20.4 ms, a 9.9× win, and it still leaves it **1.8× slower** than padding
to 128 B (20.4 ms vs 11.4 ms). Only the 128 B version reaches the
uncontended 2.28 ns/increment. Apple M-series cores prefetch adjacent lines
in **128-byte pairs**, so two variables 64 B apart are still dragged around
together. crossbeam draws the same conclusion in its own source:
`CachePadded` is `#[repr(align(128))]` on x86-64 (Intel's spatial
prefetcher, `crossbeam-utils/src/cache_padded.rs:70-71`) **and** on aarch64
("big" cores have 128-byte cache lines, `:77`).

So: "pad to a cache line" is not the rule. The rule is **pad to 128 bytes**,
and if you write 64 you have bought 9.9× of the available 17.8× and left the
rest on the table.

## Where each step lives in the code

One file — `src/backend/storage/lmgr/lwlock.c` (1939 lines at `701f021`),
about 1.5 h. Read it in this order, not top to bottom: the file's own
header comment at `:60-74` sketches the four-phase protocol before any of
it, and is worth the two minutes.

| Step | What | Where |
|---|---|---|
| 1 | latch vs lock, hold times | `lwlock.c:36-58`; `README:1-40` |
| 1 | the waiter's sleep primitive | `lwlock.c:1269` `PGSemaphoreLock` |
| 2 | state-word constants | `lwlock.c:96-108` |
| 2 | the compile-time assertions | `lwlock.c:111-118` |
| 2 | the `LWLock` struct itself | `src/include/storage/lwlock.h:41-50` |
| 3 | `LWLockAttemptLock` | `lwlock.c:764`; exclusive add `:788`; shared check `:792` |
| 3 | "always swap in, it doubles as a barrier" | `lwlock.c:797-806` |
| 4 | `LWLockQueueSelf` | `lwlock.c:1018`; the one-wait PANIC `:1028-1029` |
| 4 | proclist links are indexes | `src/include/storage/proclist_types.h:28-42` |
| 4 | `LWLockWaitListLock`, test-and-test-and-set | `lwlock.c:835`, atomic try `:852`, plain spin `:862-866` |
| 4 | `perform_spin_delay`, `spin_delay_count` | `lwlock.c:864`, `:246` |
| 5 | the double-check dance | `lwlock.c:1207-1247`; the invariant `:1223-1232` |
| 5 | `LWLockDequeueSelf` | `lwlock.c:1061` |
| 5 | the semaphore wait loop | `lwlock.c:1267-1273` |
| 6 | `LWLockRelease` — atomic subtract | `lwlock.c:1767`, `:1797-1800` |
| 6 | the wake decision | `lwlock.c:1812-1814` |
| 6 | `LWLockWakeup`, batching | `lwlock.c:904`, `:916-956` |
| 6 | barging is deliberate | `lwlock.c:1195-1205`, `:1793-1796` |
| 7 | `LWLockPadded` | `src/include/storage/lwlock.h:62-72` |
| 7 | `PG_CACHE_LINE_SIZE 128` | `src/include/pg_config_manual.h:208-217` |
| — | non-recursion: `held_lwlocks` | `lwlock.c:157`, `:167`, `:1301-1302`, `:1778-1789` |

### What to steal for M9

- one-word state + CAS fast path for your HybridLatch-style version latch
- intrusive wait queues (no allocation on the slow path), and links that are
  *indexes* if the queue could ever cross an address space
- test-and-test-and-set for any spin loop — spin on plain loads, never on
  the atomic
- pad to **128 B**, not 64, and put the number behind one named constant so
  a future machine can move it
- observable contention counters from day one

## Questions for notes.md

1. Why must the shared count live in the SAME word as the exclusive bit?
   Sketch the race if they were two atomics.
2. The recheck-after-enqueue: write the lost-wakeup interleaving it
   prevents, as a 2-thread timeline.
3. LWLocks are non-recursive and panic on double-acquire in assert
   builds. Why is recursion banned for latches but fine for locks?
4. Compare with `std::sync::RwLock` on macOS (pthread rwlock): what does
   postgres gain by rolling its own? (Think: fairness policy, no
   syscall on fast path, stats, and the queue living in shared memory.)

## Done when

You can draw the full acquire path — fast CAS, queue, recheck, sleep,
wakeup — from memory, and name the race each step exists to close.
Answer each before unfolding it.

- [ ] Name the three flag bits at `701f021` and say what each one is for.
  <details><summary>Answer</summary>

  `LW_FLAG_HAS_WAITERS` (bit 31, `:96`) — at least one backend is queued, so
  a releaser must take the wait-list lock and look. `LW_FLAG_WAKE_IN_PROGRESS`
  (bit 30, `:97`) — a wakeup has been signalled but the woken backend has not
  been scheduled yet, so further releases should not signal again; it is set
  by `LWLockWakeup` and cleared by the waiter at `:1276`. `LW_FLAG_LOCKED`
  (bit 29, `:98`) — the wait-list guard, taken with
  `pg_atomic_fetch_or_u32` at `:852`.

  If you answered `LW_FLAG_RELEASE_OK` you read an older tree. That flag
  carried the same information with the opposite polarity.
  </details>

- [ ] A backend takes the lock in SHARED mode on a lock nobody else holds.
  How many cache lines change ownership, and roughly what does that cost on
  this machine?
  <details><summary>Answer</summary>

  One line, and it goes exclusive on the acquiring core — not shared. The
  acquisition is `pg_atomic_compare_exchange_u32` (`:807`), an atomic
  read-modify-write, and it adds `LW_VAL_SHARED = 1` to the count. The
  comment at `:797-806` notes the code swaps in a value even when the lock
  looked busy, because that doubles as the memory barrier.

  Cost: if the line was already owned by this core, ~2.28 ns — the
  `false_sharing` pad128 figure. If another core owned it, ~40.5 ns (the
  packed figure), of which **38.3 ns is the ownership transfer**. There is
  no configuration in which readers of an rwlock all keep the line Shared;
  a counter-based rwlock writes on every read acquisition by construction.
  </details>

- [ ] Write the lost-wakeup interleaving as a two-thread timeline, then say
  which single line of `LWLockAcquire` makes it impossible.
  <details><summary>Answer</summary>

  T1 calls `LWLockAttemptLock` and is told to wait. Before T1 enqueues, T2
  releases: `state -= LW_VAL_EXCLUSIVE` at `:1798`, reads back an `oldstate`
  with `LW_FLAG_HAS_WAITERS` clear (`:1812`), decides `check_waiters =
  false`, and leaves. T1 then enqueues and sleeps — on a lock that is free,
  with nobody left to wake it.

  Line **1238** — the *second* `LWLockAttemptLock`, after `LWLockQueueSelf`
  at `:1235`. Both the enqueue (which sets `HAS_WAITERS`) and the release
  (which reads the state as it subtracts) touch the same `uint32`, and a
  single location has one modification order: either the release-read sees
  `HAS_WAITERS` and wakes T1, or the decrement preceded T1's second attempt
  and T1 takes the free lock. The file states this at `:1223-1232`.
  </details>

- [ ] `LWLockWaitListLock` spins. Why does the loop at `:862-866` read with
  `pg_atomic_read_u32` instead of retrying the `fetch_or`?
  <details><summary>Answer</summary>

  Because retrying the atomic would be the thing it is trying to avoid. A
  `fetch_or` is a read-modify-write: it must take the line exclusive, so N
  spinners generate N ownership transfers per loop iteration and the holder
  — who also needs the line — is starved by the very threads waiting for it.
  A plain load leaves every spinner in the Shared state, reading its own
  cached copy at L1 speed and generating **zero** interconnect traffic until
  the holder's release invalidates them all once.

  That is test-and-test-and-set: one atomic attempt (`:852`), then a
  non-atomic wait (`:862-866`). At 38.3 ns per transfer, 16 spinners
  ×1000 iterations is the difference between ~610 µs of coherence traffic
  and essentially none.
  </details>

- [ ] True or false: a backend that has been queued on an LWLock for a
  while is guaranteed to get it before a backend that arrives now. Cite the
  code.
  <details><summary>Answer</summary>

  **False.** Postgres LWLocks allow barging. `LWLockAttemptLock` (`:764-822`)
  tests only `LW_LOCK_MASK` / `LW_VAL_EXCLUSIVE` and never looks at
  `LW_FLAG_HAS_WAITERS`, so a fresh arrival takes a free lock over the heads
  of the whole queue. `LWLockRelease` says so out loud at `:1793-1795`:
  "after that it can immediately be acquired by others, even if we still
  have to wakeup other waiters."

  It is a deliberate trade, argued at `:1195-1205`: granting the lock to the
  woken waiter "means a process swap for every lock acquisition when two or
  more processes are contending", and LWLocks are meant to be taken and
  released many times inside one time slice. What you *do* get is that
  queued waiters are signalled and re-try (progress, not starvation-freedom),
  and that `LWLockWakeup` walks the queue in order among those already on
  it.
  </details>

- [ ] Your own lock array is 8 bytes per lock. What alignment do you give
  it, and what does the wrong answer cost?
  <details><summary>Answer</summary>

  **128 bytes.** Postgres pads *every* LWLock to `PG_CACHE_LINE_SIZE`, which
  is **128** (`pg_config_manual.h:217`), via `LWLockPadded`
  (`lwlock.h:62-72`) — 128 B of storage for a 12-byte lock. crossbeam's
  `CachePadded` is `repr(align(128))` on x86-64 *and* aarch64
  (`cache_padded.rs:70-77`).

  The wrong answers, priced from the `false_sharing` lane: no padding costs
  **17.8×** (202.7 ms vs 11.4 ms) because eight counters share one line and
  every increment is an ownership transfer. Padding to 64 B — the textbook
  "one cache line" — recovers most of it but is still **1.8× slower** than
  128 B (20.4 ms vs 11.4 ms), because M-series cores prefetch lines in
  128-byte pairs, so 64 B-apart variables still travel together. Half the
  advice, most of the win, and a residual you will never find by reading
  about MESI.
  </details>

## References

**Code** (pinned at `postgres/postgres@701f021`)

| File | Lines | What |
|---|---|---|
| `src/backend/storage/lmgr/lwlock.c` | 36–58 | design comment: why one atomic word |
| | 60–74 | the four-phase locking protocol, in the file's own words |
| | 96–118 | the state word, and the assertions that keep it packed |
| | 764–824 | `LWLockAttemptLock` — the whole fast path |
| | 835–880 | `LWLockWaitListLock` — test-and-test-and-set on a flag bit |
| | 904–956 | `LWLockWakeup` — batched, mode-aware |
| | 1018–1105 | `LWLockQueueSelf` / `LWLockDequeueSelf` |
| | 1150–1300 | `LWLockAcquire` — the double-check dance and the sleep |
| | 1767–1830 | `LWLockRelease` — subtract, then decide |
| `src/include/storage/lwlock.h` | 41–50, 62–72 | the struct, and `LWLockPadded` |
| `src/include/storage/proclist_types.h` | 28–42 | queue links are backend indexes, not pointers |
| `src/include/pg_config_manual.h` | 208–217 | `PG_CACHE_LINE_SIZE 128`, with its reasoning |
| `src/backend/storage/lmgr/README` | 1–40 | lock manager vs LWLock, in postgres's own words |

Read order: `README` → the `lwlock.c` header comment → `:96-118` →
`LWLockAttemptLock` → `LWLockAcquire`. About 1.5 h.

**Measurements** — every timing above is from this topic's own lanes; see
`notes.md` for the full output and `FINDINGS.md` row 9 for the headline.

| Lane | Figure |
|---|---|
| `false_sharing` | packed 202.7 ms / pad64 20.4 ms / pad128 11.4 ms → 17.8× and 1.8× |
| `false_sharing` | one ownership transfer = 40.54 − 2.28 = **38.3 ns** |
| `scaling` | global mutex 8.65 → 2.96 Mops/s from 1 to 16 threads (2.9× *slower*) |
| `scaling` | handoff cost = 337.8 − 115.6 = **222 ns/op** |

**Cross-topic** — topic 0 §2 for the memory hierarchy these numbers sit in;
topic 6 for the buffer-state word that uses the same packing trick; topic 8
for the lock manager this file is explicitly *not*.
