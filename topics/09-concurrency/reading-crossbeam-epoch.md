# Epoch reclamation: the GC that makes lock-free reads free

Lock-free deletion's boss fight is reclamation — when is it safe to
`free()` a node some reader might still hold? crossbeam-epoch answers with
three garbage bags and a global epoch counter, and it's the crate your
`concurrent_set.rs` builds on — read it first so `pin()` isn't magic. This
chapter builds the scheme one concept at a time — why `free()` is the hard
part, what a pin promises, where retired memory waits, and what the epoch
clock actually counts — then maps each piece onto the crate's three source
files.

## The problem in one sentence

A lock-free reader holds a raw pointer to a node another thread just
unlinked — free it immediately and you get a use-after-free; never free it
and a set retiring 1M nodes/s at 64 bytes each leaks **64 MB every
second** — so the whole game is deciding *when* an unlinked node becomes
untouchable by everyone.

## The concepts, step by step

### Step 1 — why free() is the hard part of lock-free

A **lock-free** structure lets readers traverse pointers while holding no
lock at all — that's the whole point (see the skiplists guide: RocksDB's
readers never write shared memory). But it means a deleter cannot know who
is looking:

```
 reader:   p = head.load(Acquire)  ───────────►  *p   ← use-after-free
 deleter:            unlink(p)  →  free(p)
                     └ safe: NEW readers          └ fatal: p was captured
                       can't reach p               a microsecond ago
```

Unlinking is safe — it only stops *future* readers from reaching the node.
Freeing is not: a *current* reader captured the pointer before the unlink.
Garbage-collected languages solve this with a tracing GC; in Rust/C you
need an explicit protocol, and getting it wrong is the worst bug class
there is (silent memory corruption, not a crash at the fault site).

### Step 2 — the pin: readers announce themselves for pennies

The protocol's reader side is one call. `epoch::pin()` (default.rs:42)
returns a `Guard` (guard.rs:70), and the promise is: **while a guard
lives, no garbage from the current epoch is freed**. Cost: ~one SeqCst
fence (a CPU ordering instruction that makes the announcement visible to
all cores before any subsequent load — a few ns) + a thread-local counter
bump. Crucially there is no shared-memory write per *pointer* — you pin
once per OPERATION, not per pointer, so a lookup traversing 40 skiplist
nodes pays the fence once. That is what makes lock-free reads effectively
free.

### Step 3 — retire now, free later: deferred destruction and the bags

The deleter side: after unlinking, don't free — **retire**. 
`Guard::defer_destroy(ptr)` (guard.rs:271) / `defer` (:90 — arbitrary
closures, unchecked variant :189) mean "free this when safe". Where the
retired memory waits: each thread has a `Local` (internal.rs:293) holding
its pinned epoch + a garbage bag, registered in a global intrusive list of
threads. `defer` (:382) drops garbage into the LOCAL bag first (no
contention with other threads), and only when the bag fills is it sealed
into the global queue, tagged with the current epoch — the tag is what
Step 4 needs.

### Step 4 — the epoch clock: three bags of garbage

The **global epoch** is a counter E that stands in for time. Every pin
records which epoch the thread pinned in; every sealed bag records which
epoch its garbage was retired in. The freeing rule is then purely
arithmetic — pop bags **≥ 2 epochs old**:

```
 global epoch: E
 thread A: pinned @ E      ─┐
 thread B: pinned @ E       ├─ all @ E ⇒ advance to E+1
 thread C: unpinned        ─┘
 bags: [E-2: freeable] [E-1: wait] [E: filling]
 one thread stuck pinned @ E-1 ⇒ epoch NEVER advances ⇒ unbounded garbage
 (the epoch weakness; hazard pointers bound garbage instead)
```

Why two epochs of grace and not one: a thread still pinned at E-1 may have
loaded pointers to nodes that were retired at E-1 *after* it pinned —
garbage from E-1 is only provably unreachable once nobody pinned at E-1
remains. (Question 1 has you construct the exact interleaving.)

### Step 5 — try_advance: the O(threads) scan that moves the clock

Someone has to move E forward, and it's the readers themselves: every
`PINNINGS_BETWEEN_COLLECT = 128` pins (:335, check at :454–456), the
pinning thread calls `collect` (:208) → `try_advance` (:237): scan ALL
registered threads; if anyone is pinned in an OLDER epoch, bail;
otherwise bump the global epoch.

```rust
fn try_advance(global: &Global) -> Epoch {
    let e = global.epoch.load(Acquire);
    for thread in global.registered_threads() {
        let local = thread.epoch.load(Acquire);
        if local.is_pinned() && local != e {
            return e;                 // a reader still lives in e-1:
        }                             // its pointers may reach that garbage
    }
    global.epoch.store(e.next(), Release); // everyone at e ⇒ advance;
    e.next()                                // bags two epochs back are free
}
```

Here the diagram's weakness becomes concrete: one thread that stays pinned
(blocked on I/O, a wedged scan) fails the scan forever, E never advances,
and garbage grows without bound. **Hazard pointers** (the main alternative
scheme, where readers publish each individual pointer they hold) bound
garbage instead — at a per-pointer cost epochs refuse to pay.

### Step 6 — the Rust twist: the borrow checker enforces the protocol

`Atomic<T>` / `Shared<'g, T>`: an atomic pointer whose loads are
lifetime-tied to a guard — the borrow checker enforces "no pointer
outlives its pin" *at compile time*. Unpin while still holding a
`Shared<'g, T>` and the program doesn't compile. This is the Rust-shaped
part that C++ epoch libraries and hazard pointers lack: Step 1's bug class
isn't just detected, it's unrepresentable (question 2).

### Step 7 — the costs, and where they're amortized

The scheme's economics, for your `concurrent_set.rs`:

- Amortize-and-batch AGAIN: local bag → sealed batch → global queue →
  collect every 128 pins. Compare valkey's SPSC batches (topic 7) and
  redis incremental rehash (topic 2).
- `try_advance` is O(threads) — that's the cost hazard pointers pay per
  FREE; epochs pay it per ADVANCE attempt. Amortization decides winners.
- Read `Guard`'s docs on repinning (`repin`/`repin_after`) — long-running
  readers (a full graph scan!) must repin or they wedge the collector.
  This is M9's "reader holds a snapshot for 10 s" problem in miniature.

## Where each step lives in the code

Read in this order — API surface first, machinery second; ~1.5 h total:
`default.rs` → `guard.rs` → `internal.rs`.

- **Step 2**: `epoch::pin()` — default.rs:42; `Guard` — guard.rs:70.
- **Step 3**: `defer_destroy` — guard.rs:271; `defer` — guard.rs:90
  (unchecked variant :189); `Local` — internal.rs:293; the local-bag path
  in `defer` — internal.rs:382.
- **Steps 4–5**: `PINNINGS_BETWEEN_COLLECT = 128` — internal.rs:335
  (checked at :454–456); `collect` — internal.rs:208; `try_advance` —
  internal.rs:237 (compare it line-by-line with the snippet above).
- **Steps 6–7**: `Atomic<T>` / `Shared<'g, T>` in atomic.rs; the
  repinning docs on `Guard` (`repin`/`repin_after`) in guard.rs — read
  them, they are the long-reader contract.

## Questions for notes.md

1. Why three epochs and not two? Construct the interleaving where a node
   retired in E is still reachable by a thread pinned in E-1.
2. What does `Shared<'g, T>`'s lifetime buy over C++ epoch libraries?
   Which bug class does it delete at compile time?
3. A reader pins, then blocks on disk I/O for 100 ms (topic 6's pool does
   this under a miss!). What happens to memory usage? What's the fix —
   repin, unpin-before-IO, or hazard pointers?
4. M9: FalkorDB queries can run for seconds. Is epoch-per-operation the
   right granularity, or epoch-per-morsel (topic 11 foreshadowing)?

## Done when

You can explain, without the source, why `defer_destroy` in epoch E can
free at E+2, and what single thread behavior wedges the whole scheme.

## References

**Code**
- [crossbeam](https://github.com/crossbeam-rs/crossbeam) —
  `crossbeam-epoch/src/`: `default.rs` (pin), `guard.rs` (Guard,
  defer_destroy — read its repinning docs), `internal.rs` (Local,
  try_advance); ~1.5 h
