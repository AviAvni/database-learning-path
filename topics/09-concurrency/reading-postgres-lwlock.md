# Reading guide — postgres lwlock.c: a production rwlock (~1.5 h)

Local clone: [`~/repos/postgres`](https://github.com/postgres/postgres), file `src/backend/storage/lmgr/lwlock.c`.
This is the latch under every buffer, WAL insert, and proc-array scan you
met in topics 5–8. One u32 of state, a CAS fast path, and a wait queue —
read it as the reference answer to "how do I build a fair rwlock that
doesn't melt at 128 cores".

## 1. The packed state word (:49, :96–118)

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

Same trick as postgres's buffer state (topic 6) and Hekaton's
end_ts-as-lock (topic 8): pack refcount + flags into one atomic word so
every protocol step is a single CAS. The static assert at :117 is the
kind of test bit-packing demands.

## 2. Fast path — LWLockAttemptLock (:764)

CAS loop: load state, compute desired (:788 exclusive add, :792 free
check for shared), compare-exchange, retry on spurious/contended failure.
No syscall, no queue touch. THE hot path — every buffer pin in a scan
goes through here.

## 3. Slow path — queue then sleep

- `LWLockQueueSelf` :1018 — add me to `proclist` (:680 — an intrusive
  list of PGPROC entries, no allocation: the waiter structure lives in
  the proc array, same idea as intrusive skiplist nodes).
- The wait-list itself is protected by a SPINLOCK with backoff:
  :860–880 `perform_spin_delay` — spin, then sleep escalation; stats
  count `spin_delay_count` (:246) so contention is observable.
- **The double-check dance** in `LWLockAcquire` :1150: attempt → queue
  self → attempt AGAIN → only then sleep. Without the second attempt, a
  release between attempt and enqueue leaves you sleeping forever (lost
  wakeup). `LWLockDequeueSelf` :1061 handles the "won on the recheck"
  undo. This pattern (test, enqueue, re-test) is THE lesson of the file.
- `LWLockRelease` :1767 → `LWLockWakeup` :904: wakes the queue head; a
  released shared lock wakes waiting readers as a batch, and
  RELEASE_OK prevents wakeup storms.

## 4. What to steal for M9

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
