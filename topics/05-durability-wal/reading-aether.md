# Aether: one log, no bottleneck

On a multicore, the log is ONE shared object every transaction must append to
and flush — so how does it not become the bottleneck? Aether's answer is four
independent fixes that compose, and one of them (consolidation arrays) is the
ancestor of how postgres inserts WAL today. Before the paper, this chapter
builds the four bottlenecks one at a time and then each fix in order of
increasing cleverness — ending with the one that shipped everywhere.

## The problem in one sentence

Every committing transaction must append to a single serial log and wait for
it to reach disk, so on a 32-core machine the log is 32 threads funneling
into one mutex and one ~1 ms fsync — a hard ceiling of ~1K commits/s and
worsening lock contention, no matter how many cores you add.

## The concepts, step by step

### Step 1 — why the log must be serial, and what that costs on a multicore

The entire recovery story of topic 5 rests on the log being one totally
ordered sequence: records are replayed in log order, commit order *is* log
order, and a record is durable only if everything before it is. That total
order is bought with physical serialization — one append point, one flush
frontier. On one core in 1992 this was free; on a multicore it turns the log
into the single object every transaction must touch twice (once to insert
its records, once to await the flush). Aether's contribution starts with
*naming* the distinct ways that hurts.

### Step 2 — the four bottlenecks, separated

Four different waits hide inside "commit is slow", with four different
causes — a mutex, a disk, a lock table, and a scheduler:

```
 txn commits ──► [A] contend on log-buffer insert   (one mutex around append)
             ──► [B] wait for fsync                  (I/O latency per commit)
             ──► [C] hold locks WHILE waiting for [B] (lock contention amplified)
             ──► [D] context switches around the wait
```

[A] is CPU-side: every append serializes on the buffer mutex, and memcpy
happens *inside* the critical section. [B] is the disk: ~1 ms of fsync per
commit. [C] is the multiplier: the transaction still holds its row/page
locks while waiting on [B], so one fsync delay cascades into every
transaction queued behind those locks. [D] is the OS: blocking on IO means
descheduling and rescheduling threads at ~µs each. Read the paper as four
independent fixes that compose:

| Bottleneck | Fix | Modern descendant |
|---|---|---|
| B: fsync per commit | group commit | postgres `XLogFlush` recheck |
| C: locks held across flush | **Early Lock Release** (ELR) | controversial; see Q2 |
| D: scheduling | flush pipelining (async commit queues) | redis everysec (cruder) |
| A: buffer insert mutex | **consolidation array** | postgres reserve-then-copy |

### Step 3 — fix B: group commit — one fsync covers N commits

Group commit attacks the fsync count by noticing that one disk flush makes
durable *every* log record written before it, not just yours. So instead of
one fsync per commit, transactions that arrive while a flush is in progress
simply wait for the *next* flush, which covers all of them: at 1 ms per
fsync and 32 waiting committers, that's 32 commits per fsync ⇒ ~32K
commits/s through the same disk. Every serious engine does this; postgres's
version (recheck the flushed-LSN after acquiring the write lock — most
backends find their work already done) is dissected in
reading-postgres-xlog.md §3. Group commit fixes the *throughput* of [B] but
leaves latency (you still wait ~1 fsync), locks held ([C]) and the insert
mutex ([A]) untouched.

### Step 4 — fix C: Early Lock Release — stop holding locks through the flush

ELR releases a transaction's locks at commit-record *creation* (the record
is in the log buffer, ordered) rather than commit-record *durability* (the
record is on disk) — so the ~1 ms flush wait no longer blocks every
transaction queued on those locks. The safety argument is elegant: a
dependent transaction that read your uncommitted-but-logged data cannot
*acknowledge* before you, because its commit record sits **after** yours in
the serial log — a crash that loses your commit necessarily loses theirs
too. The serial log, the thing that seemed like pure bottleneck, doubles as
a free dependency tracker. The catch (question 2): the argument only covers
effects that escape through the log — a read-only transaction that never
writes a commit record can leak unflushed state to a user. Real systems
mostly didn't ship ELR.

### Step 5 — fix D: flush pipelining — the thread leaves, the commit stays

Flush pipelining decouples the *worker thread* from the *commit wait*:
instead of blocking on fsync ([D]'s context switches), the worker enqueues
the commit, detaches, and immediately picks up new work; a background daemon
acknowledges each client after the flush covering its commit lands.
Throughput of asynchronous commit, durability of synchronous commit — the
cost is added ack latency and a more complex scheduler, **not** a loss
window. Contrast redis's `appendfsync everysec` (reading-redis-aof-rdb.md),
which acks *before* durability and accepts up to ~1 s of loss — pipelining
is the same "don't block the worker" instinct with the contract kept intact.

### Step 6 — fix A: consolidation arrays — combine before you contend

With B, C, D fixed, the remaining wall is the log-buffer mutex itself:
every append still serializes, memcpy included. The insight: even with
group commit, the *insertions* contend one at a time. Fix: threads combine
their requests *before* touching the lock.

```
 naive:      T1 ─lock─ memcpy ─unlock─ T2 ─lock─ memcpy ─unlock─ T3 …
 consolidated:
   T1,T2,T3 meet in a slot array, add up sizes (CAS, no lock),
   ONE of them acquires the lock, reserves sum(bytes) once,
   each thread memcpys into its own slice IN PARALLEL.
```

The principle: decouple *sequencing* (assigning log offsets — must be
serial, so make it tiny: one addition) from *copying* (moving the bytes —
needn't be serial, so make it parallel). Postgres's
`ReserveXLogInsertLocation` (a spinlock held for 3 arithmetic ops) + 8
parallel insertion locks is this idea in production — read
reading-postgres-xlog.md §2 side by side with the paper's §5. The slot
dance, in code:

```rust
// Combine appends BEFORE the lock; only sequencing stays serial.
fn append(&self, rec: &[u8]) -> Lsn {
    let slot = self.slots.join();                    // CAS onto an open slot
    let my_off = slot.size.fetch_add(rec.len());     // add my bytes — no lock
    if my_off == 0 {                                 // first in = group leader
        let total = slot.close();                    // no more joiners
        let base = {
            let _g = self.buffer_lock.lock();        // tiny critical section:
            self.reserve(total)                      // ONE reservation for all
        };
        slot.publish(base);
    }
    let base = slot.wait_for_base();
    self.buf_write(base + my_off, rec);              // everyone copies IN PARALLEL
    Lsn(base + my_off)
}
```

This is the fix that shipped everywhere, because it attacks the only
bottleneck that *scales with core count* — [B] is constant per disk, but
[A] gets worse with every core you add.

## How to read the paper (with the concepts in hand)

1. §1–2 for the bottleneck taxonomy (Step 2's table, in the authors' words).
2. §5 consolidation arrays (Step 6) — the durable contribution.
3. §3 ELR (Step 4) — for the *argument* about log order as dependency
   tracking.
4. Skim §4 (flush pipelining, Step 5) + evaluation (§6): note which fix buys
   what at which core count.

## Questions to answer in notes.md

1. Why does ELR NOT violate durability for the *dependent* transaction?
   (Its commit record is behind yours; a crash that loses yours loses its too.)
2. ELR hazard: what if the dependent txn's result escapes to the user by a
   channel other than its own commit ack (e.g. a read-only txn that never
   logs)? This is why real systems mostly didn't ship it.
3. Consolidation vs postgres's 8 insert locks: both parallelize the copy —
   what's the difference in HOW threads find a slot? (CAS-combining into a
   shared slot vs hashing onto a fixed lock array; contrast under 8 vs 80
   writers.)
4. Which bottleneck does your M5 group-commit design leave unfixed? (Likely A
   — a single mutex around the WAL buffer is fine at graph-workload commit
   rates; say at what commits/s it wouldn't be.)

## Done when

You can name the four bottlenecks from memory, sketch a consolidation array,
and point at the postgres code that embodies it.

## References

**Papers**
- Johnson, Pandis, Stoica, Athanassoulis, Ailamaki — "Aether: A Scalable
  Approach to Logging" (VLDB 2010) — ~12 pages; §1–2 for the bottleneck
  taxonomy, §5 (consolidation arrays) is the durable contribution, §3 for
  the ELR argument, skim §4 and the evaluation
