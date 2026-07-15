# One word, one CAS, one queue: postgres's production rwlock

lwlock.c is the latch under every buffer, WAL insert, and proc-array scan
you met in topics 5–8. One u32 of state, a CAS fast path, and an intrusive
wait queue — read it as the reference answer to "how do I build a fair
rwlock that doesn't melt at 128 cores". This chapter builds the lock one
concept at a time — what a latch even is, the packed state word, the CAS
fast path, the wait queue, and the lost-wakeup race the whole design
orbits — then pins each piece to its line in the file.

## The problem in one sentence

A postgres scan takes and releases a reader-writer lock on **every buffer
it touches** — millions of acquisitions per second per core, each held for
tens of nanoseconds — so a lock whose fast path costs a syscall (~1 µs) or
even one extra contended cache line would cost more than the work it
protects.

## The concepts, step by step

### Step 1 — a latch: a nanosecond-scale reader-writer lock

A **reader-writer lock** (rwlock) admits many simultaneous readers OR one
exclusive writer — the right shape for data that is read constantly and
written rarely. A **latch** is an rwlock used to protect a data
structure's *physical* integrity for nanoseconds — unlike topic 8's
transaction locks, which protect *logical* content for seconds and come
with deadlock detection, queues in the lock manager, and recursion.
Latches get none of that (question 3 asks why recursion in particular is
banned). lwlock.c — "lightweight lock" — is postgres's latch: the thing
under every buffer pin, WAL insert, and proc-array scan from topics 5–8.

### Step 2 — the packed state word (:49, :96–118)

**CAS** (compare-and-swap) is the atomic CPU instruction "replace this one
word with a new value only if it still equals the value I read" — it can
update exactly ONE word atomically. So the design's first move is to make
the entire lock state fit in one u32:

```
 u32 state:
 ┌─────────────┬──────────────┬──────────────────────────────┐
 │ FLAG bits   │ LW_VAL_EXCLUSIVE = MAX_BACKENDS+1           │
 │ HAS_WAITERS │ LW_VAL_SHARED = 1                           │
 │ RELEASE_OK  │ → shared holders are a COUNT in the low bits│
 └─────────────┴──────────────────────────────────────────────┘
 exclusive = add LW_VAL_EXCLUSIVE; shared = add 1.
 "is it free for X?" = (state & LW_LOCK_MASK) == 0 — one load.
```

The trick: shared holders are a *count* (each reader adds 1), and taking
it exclusive adds `LW_VAL_EXCLUSIVE = MAX_BACKENDS+1` — a value no count
of readers can reach, so one masked compare distinguishes "free",
"readers", and "writer". Same trick as postgres's buffer state (topic 6)
and Hekaton's end_ts-as-lock (topic 8): pack refcount + flags into one
atomic word so every protocol step is a single CAS. The static assert at
:117 is the kind of test bit-packing demands.

### Step 3 — fast path — LWLockAttemptLock (:764)

With the state in one word, acquiring in the uncontended case is one CAS
loop: load state, compute desired (:788 exclusive add, :792 free
check for shared), compare-exchange, retry on spurious/contended failure.
No syscall, no queue touch. THE hot path — every buffer pin in a scan
goes through here. This is what "doesn't melt" means: the common case is
a handful of instructions on a cache line that, for readers of a
read-mostly lock, everyone can keep shared.

### Step 4 — slow path furniture: the intrusive wait queue

When the CAS says "held", the thread must wait — and waiting needs a queue
of waiters. An **intrusive list** embeds the list links inside a structure
that already exists instead of allocating a node, and that's what postgres
uses:

- `LWLockQueueSelf` :1018 — add me to `proclist` (:680 — an intrusive
  list of PGPROC entries, no allocation: the waiter structure lives in
  the proc array, same idea as intrusive skiplist nodes).
- The wait-list itself is protected by a SPINLOCK with backoff:
  :860–880 `perform_spin_delay` — spin, then sleep escalation; stats
  count `spin_delay_count` (:246) so contention is observable.

No allocation on the slow path matters twice: the queue lives in shared
memory (any backend can wake any other), and a lock you take millions of
times per second cannot afford malloc on its unhappy path either.

### Step 5 — the lost wakeup, and the double-check dance

Here is the race the whole file orbits. Naive slow path: attempt, fail,
enqueue, sleep. But if the holder *releases between your failed attempt
and your enqueue*, it finds an empty queue, wakes nobody — and you sleep
forever on a free lock. That is a **lost wakeup**.

The fix — **the double-check dance** in `LWLockAcquire` :1150: attempt →
queue self → attempt AGAIN → only then sleep. Without the second attempt, a
release between attempt and enqueue leaves you sleeping forever.
`LWLockDequeueSelf` :1061 handles the "won on the recheck" undo. This
pattern (test, enqueue, re-test) is THE lesson of the file:

```rust
fn acquire(lock: &LwLock, mode: Mode) {
    loop {
        if try_cas(lock, mode) { return; }  // fast path: one CAS, no queue
        queue_self(lock);                   // slow: enqueue FIRST...
        if try_cas(lock, mode) {            // ...then attempt AGAIN —
            dequeue_self(lock);             // a release may have slipped in
            return;                         // between attempt and enqueue
        }
        sleep_until_woken();                // safe now: our queue entry is
    }                                       // visible, releaser must wake us
}
```

Why sleeping is safe *after* the recheck: your queue entry is visible
before your final attempt, so any release that happens after your failed
recheck must see `HAS_WAITERS` and wake you. The two orders (enqueue
before re-test; release checks waiters after clearing the lock) interlock
so that no release can fall in the gap.

### Step 6 — release, batched wakeups, and fairness

`LWLockRelease` :1767 → `LWLockWakeup` :904: wakes the queue head; a
released shared lock wakes waiting readers as a batch, and RELEASE_OK
prevents wakeup storms (a woken waiter that hasn't run yet shouldn't
trigger more wakeups). The queue is what buys **fairness**: waiters are
served in arrival order, so a stream of barging readers cannot starve a
queued writer forever — the failure mode a naive CAS-only rwlock hits at
128 cores.

## Where each step lives in the code

One file — `src/backend/storage/lmgr/lwlock.c`, ~1.5 h. Start at the
state-word definitions, then `LWLockAttemptLock`, then `LWLockAcquire`.

- **Step 2**: state-word layout and constants — :49, :96–118; the static
  assert — :117.
- **Step 3**: `LWLockAttemptLock` — :764; the exclusive add — :788; the
  shared free-check — :792.
- **Step 4**: `LWLockQueueSelf` — :1018; `proclist` — :680;
  `perform_spin_delay` — :860–880; `spin_delay_count` — :246.
- **Step 5**: the double-check dance in `LWLockAcquire` — :1150;
  `LWLockDequeueSelf` — :1061.
- **Step 6**: `LWLockRelease` — :1767; `LWLockWakeup` — :904.

### What to steal for M9

- one-word state + CAS fast path for your HybridLatch-style version latch
- intrusive wait queues (no allocation on the slow path)
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

## References

**Code**
- [postgres](https://github.com/postgres/postgres)
  `src/backend/storage/lmgr/lwlock.c` — ~1.5 h; start at the state-word
  definitions (:49, :96–118), then `LWLockAttemptLock` and `LWLockAcquire`
