# Epoch reclamation: the GC that makes lock-free reads free

Lock-free deletion's boss fight is reclamation — when is it safe to
`free()` a node some reader might still hold? crossbeam-epoch answers with
a global epoch counter and a queue of sealed garbage bags, and it's the
crate your `concurrent_set.rs` builds on — read it first so `pin()` isn't
magic. This chapter builds the scheme one concept at a time — why `free()`
is the hard part, what a pin promises, where retired memory waits, what the
epoch clock actually counts, and what the whole thing costs — then maps
each piece onto the crate's three source files.

Everything below is read at the pinned commit
**`crossbeam-rs/crossbeam@6b7458d`**
(`python3 tools/pinned-source.py ref crossbeam`); `crossbeam-epoch/src/internal.rs`
is 636 lines there.

## The problem in one sentence

A lock-free reader holds a raw pointer to a node another thread just
unlinked — free it immediately and you get a use-after-free; never free it
and a set retiring 1M nodes/s at 64 bytes each leaks **64 MB every
second** — so the whole game is deciding *when* an unlinked node becomes
untouchable by everyone, without making the reader pay to say so.

## The concepts, step by step

### Step 1 — why free() is the hard part of lock-free

> **In:** a shared structure whose readers hold no lock.
> **Out:** the exact reason unlinking is easy and freeing is not, plus the
> lock-free / wait-free / obstruction-free vocabulary.

**Lock-free** is a progress guarantee, not a description of the
instructions used: a structure is lock-free if *some* thread always makes
progress in a bounded number of steps, no matter what any other thread
does — including being descheduled mid-operation. **Wait-free** is the
stronger guarantee that *every* thread makes progress in a bounded number
of its own steps. The distinction is not academic: a CAS retry loop is
lock-free but not wait-free, because one unlucky thread can lose every race
forever while the structure as a whole races ahead. Postgres uses the term
precisely in its own header — `lwlock.c:38-39` claims "wait-free shared
lock acquisition for locks that aren't exclusively locked", because a
shared acquisition is a bounded number of CAS attempts against a bounded
number of contenders.

Lock-freedom means readers traverse pointers while holding nothing (see the
skiplists guide: RocksDB's readers never write shared memory). But it also
means a deleter cannot know who is looking:

```
 reader:   p = head.load(Acquire)  ───────────►  *p   ← use-after-free
 deleter:            unlink(p)  →  free(p)
                     └ safe: NEW readers          └ fatal: p was captured
                       can't reach p               a microsecond ago
```

Unlinking is safe — it only stops *future* readers from reaching the node.
Freeing is not: a *current* reader captured the pointer before the unlink.
Garbage-collected languages solve this with a tracing GC; in Rust or C you
need an explicit protocol, and getting it wrong is the worst bug class
there is — silent memory corruption, discovered somewhere else entirely,
minutes later.

This is also where the **ABA problem** lives, and it is worth naming now
because reclamation and ABA are the same wound. A thread reads pointer `A`,
is descheduled; another thread frees `A`, allocates a new node at the same
address, and links it in; the first thread's CAS comparing against `A`
*succeeds* — the value is unchanged, the meaning is not. Any scheme that
delays reuse long enough (which is what epochs do) makes ABA impossible for
free; schemes that don't must smuggle a version tag into spare pointer bits
instead.

### Step 2 — the pin: readers announce themselves for pennies

> **In:** a thread about to traverse the structure.
> **Out:** `Guard`, and the price of announcing "I am reading" — measured
> in cache lines, not in locks.

The protocol's reader side is one call. `epoch::pin()` (`default.rs:42`)
returns a `Guard` (`guard.rs:70`), and the promise is: **while a guard
lives, no garbage retired in the current epoch (or the one before) is
freed**.

What that costs is the interesting part, and it is best read straight out
of `Local::pin`:

```rust
// crossbeam-epoch/src/internal.rs:403-459 — Local::pin, cfg branches elided
   403      pub(crate) fn pin(&self) -> Guard {
   404          let guard = Guard { local: self };
   406          let guard_count = self.guard_count.get();
   407          self.guard_count.set(guard_count.checked_add(1).unwrap());
   409          if guard_count == 0 {
   410              let global_epoch = self.global().epoch.load(Ordering::Relaxed);
   411              let new_epoch = global_epoch.pinned();
   446                  self.epoch.store(new_epoch, Ordering::Relaxed);
   447                  atomic::fence(Ordering::SeqCst);
   451              let count = self.pin_count.get();
   452              self.pin_count.set(count + Wrapping(1));
   456              if count.0 % Self::PINNINGS_BETWEEN_COLLECT == 0 {
   457                  self.global().collect(&guard);
   458              }
   459          }
```

Three facts fall out of those lines, and together they are the reason
epoch reclamation is cheap:

1. **A nested pin is free.** `guard_count` is a plain `Cell<usize>`
   (`:306`) — not an atomic — because only the owning thread touches it. If
   the thread is already pinned, `pin()` bumps a non-atomic counter and
   returns. The whole `if guard_count == 0` body is skipped.
2. **The announcement is a store to the thread's *own* line.** `Local.epoch`
   is `CachePadded<AtomicEpoch>` (`:317`), so it lives alone on its own
   128-byte line; `Global.epoch` is `CachePadded` too (`:173`). A reader
   writes nothing that another reader is also writing, which is the whole
   difference from Step 3 of the LWLock guide, where every shared
   acquisition contends for one word.
3. **The ordering is one fence.** Line 446–447 is `store(Relaxed)` followed
   by `fence(SeqCst)`.

**Memory ordering, defined once.** `Relaxed` guarantees only atomicity —
no ordering with respect to any other access. `Release` on a store, paired
with `Acquire` on a load of the same location, guarantees that everything
the storing thread wrote *before* the store is visible to a thread that
*sees* the store — that pairing is how you publish an initialised node.
`SeqCst` additionally puts the operation into one total order that all
threads agree on. A `SeqCst` *fence* orders the accesses around it without
naming a location, which is exactly what a pin needs: the announcement must
land before any subsequent pointer load, or a reader could load a pointer
the collector already believes nobody can see.

`pin()` is a *per-operation* cost, not a per-pointer one. A skiplist lookup
traversing 31 nodes (see the skiplists guide's arithmetic) pays the fence
once. Compare hazard pointers, where each pointer a reader holds is
individually published with a store-plus-fence — 31 fences for the same
traversal. That trade is the entire design space, and Step 7 prices it.

### Step 3 — retire now, free later: the local bag

> **In:** a node that has just been unlinked and must eventually be freed.
> **Out:** where it waits, and why it does not touch shared memory on the
> way there.

The deleter side: after unlinking, don't free — **retire**.
`Guard::defer_destroy(ptr)` (`guard.rs:271`) means "drop and free this
when safe"; `Guard::defer` (`guard.rs:90`, with the unchecked variant at
`:189`) does the same for an arbitrary closure.

**Epoch-based reclamation** (EBR), stated in one sentence: readers stamp
themselves with the current epoch while they read, retired objects are
stamped with the epoch they were retired in, and an object may be freed
once no reader is stamped with an epoch old enough to have seen it.

Where the retired memory waits:

```rust
// crossbeam-epoch/src/internal.rs:382-389 — Local::defer, into the thread-local bag
   382      pub(crate) unsafe fn defer(&self, mut deferred: Deferred, guard: &Guard) {
   383          let bag = self.bag.with_mut(|b| unsafe { &mut *b });
   385          while let Err(d) = unsafe { bag.try_push(deferred) } {
   386              self.global().push_bag(bag, guard);
   387              deferred = d;
   388          }
   389      }
```

`self.bag` is an `UnsafeCell<Bag>` in the thread's own `Local` (`:303`),
holding at most `MAX_OBJECTS = 64` deferred functions (`:66`). Retiring is
therefore an append to a thread-private array **63 times out of 64**. Only
on the 64th does `push_bag` run, and only then does anything shared get
written:

```rust
// crossbeam-epoch/src/internal.rs:191-198 — sealing a full bag with the epoch
   191      pub(crate) fn push_bag(&self, bag: &mut Bag, guard: &Guard) {
   192          let bag = mem::replace(bag, Bag::new());
   194          atomic::fence(Ordering::SeqCst);
   196          let epoch = self.epoch.load(Ordering::Relaxed);
   197          self.queue.push(bag.seal(epoch), guard);
   198      }
```

`bag.seal(epoch)` (`:197`) is where the timestamp gets attached — this is
the tag Step 4 reads. Note the fence at `:194` *precedes* the epoch load:
the sealing thread must not read an epoch older than the unlinking it just
performed, or it would under-stamp its own garbage.

This is amortize-and-batch, the same move as valkey's SPSC batches (topic
7) and redis's incremental rehash (topic 2): make the common case
thread-local and pay the shared cost once per N.

### Step 4 — the epoch clock, and why the answer is two

> **In:** a queue of bags, each stamped with a retirement epoch, and a
> global counter.
> **Out:** the exact predicate that decides a bag is freeable, and the
> proof sketch behind its constant.

The **global epoch** is a counter E that stands in for time. Every pin
records the epoch the thread pinned in; every sealed bag records the epoch
its garbage was retired in. The freeing rule is then pure arithmetic, and
it is six lines of source:

```rust
// crossbeam-epoch/src/internal.rs:155-162 — the entire freeing rule
   155  impl SealedBag {
   156      /// Checks if it is safe to drop the bag w.r.t. the given global epoch.
   157      fn is_expired(&self, global_epoch: Epoch) -> bool {
   158          // A pinned participant can witness at most one epoch advancement. Therefore, any bag that
   159          // is within one epoch of the current one cannot be destroyed yet.
   160          global_epoch.wrapping_sub(self.epoch) >= 2
   161      }
   162  }
```

```
 global epoch: E
 thread A: pinned @ E      ─┐
 thread B: pinned @ E       ├─ all pinned participants @ E ⇒ advance to E+1
 thread C: unpinned        ─┘
 bags:  [E-2 and older: FREE]  [E-1: wait]  [E: filling]
 one thread stuck pinned @ E-1 ⇒ E never advances ⇒ unbounded garbage
```

**Why the constant is 2, in the comment's own words: "a pinned participant
can witness at most one epoch advancement".** Unpack that. A thread pins at
epoch `p`. While it stays pinned, `try_advance` (Step 5) refuses to move
past `p+1`, because the scan would see this thread pinned at `p ≠ p+1` and
bail. So the global epoch can reach at most `p+1` — one advancement — while
that thread lives. Now take a bag stamped `b` with `E − b ≥ 2`. Any thread
still pinned satisfies `E ≤ p + 1`, hence `p ≥ E − 1 > b`: every live
reader pinned strictly *after* the bag was sealed, so it cannot have loaded
a pointer into that bag. One epoch of grace would not be enough — a thread
pinned at `E−1` may have loaded pointers to nodes retired at `E−1` after it
pinned. (Question 1 has you construct that interleaving explicitly.)

The comparison is `wrapping_sub`, not `-`: the epoch is a wrapping counter,
so the arithmetic must be too.

### Step 5 — try_advance: the O(threads) scan that moves the clock

> **In:** a global epoch nobody is advancing.
> **Out:** who advances it, how often, and the one thread behaviour that
> wedges the whole scheme.

Someone has to move E forward, and it is the readers themselves — on a
cold path, every 128th pin. Look back at `Local::pin` in Step 2, lines
456–457: `if count.0 % Self::PINNINGS_BETWEEN_COLLECT == 0` calls
`Global::collect`, where `PINNINGS_BETWEEN_COLLECT = 128` (`:335`).
`collect` (`:208`) calls `try_advance` and then pops at most
`COLLECT_STEPS = 8` expired bags (`:178`, loop at `:217-225`) — bounded
work, so no single pin can be ambushed by a huge free storm.

```rust
// crossbeam-epoch/src/internal.rs:237-287 — try_advance, sanitizer cfgs elided
   237      pub(crate) fn try_advance(&self, guard: &Guard) -> Epoch {
   238          let global_epoch = self.epoch.load(Ordering::Relaxed);
   239          atomic::fence(Ordering::SeqCst);
   249          for local in self.locals.iter(guard) {
   250              match local {
   251                  Err(IterError::Stalled) => {
   255                      return global_epoch;
   256                  }
   257                  Ok(local) => {
   258                      let local_epoch = local.epoch.load(Ordering::Relaxed);
   262                      if local_epoch.is_pinned() && local_epoch.unpinned() != global_epoch {
   263                          return global_epoch;
   264                      }
   268                  }
   269              }
   270          }
   276          atomic::fence(Ordering::Acquire);
   285          let new_epoch = global_epoch.successor();
   286          self.epoch.store(new_epoch, Ordering::Release);
   287          new_epoch
   288      }
```

Two things here are easy to get wrong when you write your own, and both
are visible only in the real source:

- **The per-`Local` loads are `Relaxed` (`:258`), not `Acquire`.** The
  ordering is supplied once by the `SeqCst` fence at `:239` and once by the
  `Acquire` fence at `:276`, not per-load. Fences instead of per-access
  ordering is the standard trick when you are about to touch N locations
  and want to pay for ordering once; it is the same instinct as pinning
  once per operation instead of once per pointer.
- **A stalled iterator is a bail-out, not a retry** (`:251-256`). The
  `Local` list is itself lock-free, so iteration can be disturbed by a
  concurrent unregister; rather than spin, `try_advance` returns and leaves
  the job to whoever disturbed it. Advancing the epoch is never urgent —
  it is pure optimisation — so every failure path here is "give up
  cheaply".

The comment at `:281-284` justifies the unconditional `store` at `:286`: a
racing thread may have advanced it already, in which case this store writes
the same value, because the caller of `try_advance` is *itself* pinned in
`global_epoch` and so the epoch cannot have run two steps ahead.

**The failure mode is now concrete.** One thread that stays pinned — blocked
on I/O, a wedged scan, a debugger breakpoint — fails the test at `:262`
forever, E never advances, `is_expired` is never true, and garbage grows
without bound. **Hazard pointers**, the main alternative scheme, publish
each individual pointer a reader holds and free anything not currently
published; they bound garbage by construction, at a per-pointer cost
epochs refuse to pay. Neither is strictly better. Epochs are cheap and
unbounded; hazard pointers are bounded and expensive.

### Step 6 — the Rust twist: the borrow checker enforces the protocol

> **In:** the protocol from Steps 2–5, which a C programmer must follow by
> discipline.
> **Out:** how `Shared<'g, T>` turns Step 1's bug class into a compile
> error.

`Atomic<T>` (`atomic.rs`) is an atomic pointer whose `load` returns
`Shared<'g, T>` — a pointer whose lifetime `'g` is tied to the `Guard` that
authorised the load. The borrow checker then enforces "no pointer outlives
its pin" *at compile time*: drop the guard while a `Shared<'g, T>` derived
from it is still live and the program does not compile.

That is the Rust-shaped part C++ epoch libraries and hazard-pointer
libraries lack. In C++ the equivalent mistake — retaining a pointer past
the end of the critical section — compiles cleanly and corrupts memory
under load a week later. Here it is not detected; it is unrepresentable
(question 2).

The protocol still has one thing the type system cannot check: **duration**.
A guard held for ten seconds is perfectly typed and completely wrong, for
the reason Step 5 just gave. That is what `Guard::repin` (`:329`) and
`repin_after` (`:366`) are for — they unpin and re-pin, giving the
collector a window, and their signatures deliberately invalidate every
`Shared` you were holding, so the compiler forces you to re-load your
pointers afterwards. `Guard::flush` (`:295`) is the other escape hatch: push
the local bag to the global queue now rather than at 64.

### Step 7 — the costs, worked

> **In:** the whole scheme.
> **Out:** per-operation numbers on this machine, and the amortisation
> argument that makes them small.

Price the pieces with this topic's own measured constants (from
`false_sharing`: an uncontended atomic RMW on a thread's own 128 B-padded
line costs **2.28 ns**; moving a line between cores costs **38.3 ns**).

**Per pin, steady state.** One `Relaxed` store plus one `SeqCst` fence to
the thread's own `CachePadded` line — no other thread writes it, so no
transfer: on the order of **2–3 ns**. Nested pins: a `Cell` increment,
~0 ns.

**Per retire.** One append to a thread-local array, 63 times in 64. On the
64th, one `SeqCst` fence and one lock-free queue push.

**Per `try_advance`.** O(T) `Relaxed` loads, one per registered thread.
Each `Local.epoch` is `CachePadded` (`:317`) and was last written by its
owner, so each load is a cross-core transfer: T = 16 threads ⇒ 16 × 38.3 ns
≈ **610 ns**. That sounds enormous until you divide by
`PINNINGS_BETWEEN_COLLECT`:

```
  610 ns per try_advance ÷ 128 pins between collects = 4.8 ns per operation
  ...and try_advance is #[cold] (:236), so it isn't in the I-cache hot set
```

**4.8 ns amortised**, against a `scaling`-lane operation that costs
1/19.28 Mops/s ≈ 52 ns at 16 threads — under 10%, and it *shrinks* as
threads get busier because pins get more frequent while T stays fixed.

Now the comparison that decides the design. `try_advance` is O(threads) —
and O(threads) is exactly what hazard pointers pay **per free**, since
freeing a pointer requires checking it against every thread's published
hazard set. Epochs pay it per *advance attempt*, i.e. once per 128 pins,
and each advance can retire an unbounded number of objects. Same asymptotic
scan, radically different divisor. **Amortisation is the whole argument**,
and it is the recurring lesson of this curriculum — the same one behind
RocksDB's splice (skiplists guide, Step 5), postgres's batched wakeups
(LWLock guide, Step 6), and every buffer in topic 7.

The bill epochs hand you in exchange: unbounded garbage under a stalled
reader, and no way to bound it from inside the library. For M9 that is the
question — FalkorDB queries can run for seconds (question 4).

## Where each step lives in the code

Read in this order — API surface first, machinery second; ~1.5 h total:
`default.rs` → `guard.rs` → `internal.rs`. `internal.rs` rewards being read
bottom-up: `Local::pin` at `:403` is the whole scheme in 60 lines, and
everything above it is support.

| Step | What | Where |
|---|---|---|
| 2 | `epoch::pin()` — the public entry point | `crossbeam-epoch/src/default.rs:42` |
| 2 | `Guard` and its contract | `guard.rs:70` |
| 2 | `Local::pin` — counter, store, fence, collect | `internal.rs:403-462` |
| 2 | `guard_count` / `pin_count` are plain `Cell`s | `internal.rs:306`, `:314` |
| 2 | `Local.epoch` is `CachePadded` | `internal.rs:317`; `Global.epoch` `:173` |
| 2 | the x86 `lock cmpxchg` hack vs the aarch64 fence | `internal.rs:416-448` |
| 3 | `defer_destroy` / `defer` / `flush` | `guard.rs:271`, `:90`, `:189`, `:295` |
| 3 | `Local::defer` → thread-local bag | `internal.rs:382-389` |
| 3 | `MAX_OBJECTS = 64` | `internal.rs:66` |
| 3 | `push_bag` — fence, load epoch, seal | `internal.rs:191-198` |
| 4 | `SealedBag::is_expired` — the `>= 2` rule | `internal.rs:157-161` |
| 5 | `PINNINGS_BETWEEN_COLLECT = 128`, and its check | `internal.rs:335`, `:456` |
| 5 | `Global::collect`, `COLLECT_STEPS = 8` | `internal.rs:208`, `:178`, `:217-225` |
| 5 | `try_advance` — the scan | `internal.rs:237-288`; the bail `:262-264` |
| 5 | the `Local` list is intrusive and lock-free | `internal.rs:167`, `:292-295` |
| 6 | `Atomic<T>` / `Shared<'g, T>` | `crossbeam-epoch/src/atomic.rs` |
| 6 | `repin` / `repin_after` — the long-reader contract | `guard.rs:329`, `:366` |
| 7 | `CachePadded` is 128 B on x86-64 **and** aarch64 | `crossbeam-utils/src/cache_padded.rs:70-77`, `:87-94` |

### What to steal for M9

- pin once per *operation*, never per pointer — that is the whole cost
  advantage over hazard pointers
- keep the per-thread state on its own **128-byte** line; crossbeam does,
  and this topic's `false_sharing` lane measures 17.8× for getting it wrong
- make every maintenance path `#[cold]`, bounded (`COLLECT_STEPS`), and
  bail-out-happy — reclamation is never urgent
- decide the repin policy *before* you have a ten-second query, not after

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
Answer each before unfolding it.

- [ ] Quote the freeing rule as a predicate on two epochs, and justify its
  constant.
  <details><summary>Answer</summary>

  `global_epoch.wrapping_sub(self.epoch) >= 2` — `internal.rs:160`, the
  whole of `SealedBag::is_expired`.

  The constant is 2 because, as the comment at `:158-159` puts it, "a
  pinned participant can witness at most one epoch advancement". A thread
  pinned at `p` blocks `try_advance` at `:262` for as long as it stays
  pinned, so the global epoch cannot exceed `p + 1`. Given a bag sealed at
  `b` with `E − b ≥ 2`, every live reader satisfies `p ≥ E − 1 > b` — it
  pinned strictly after the bag was sealed, so it never held a pointer into
  it. With a threshold of 1 the argument fails: a reader pinned at `E−1`
  may have loaded pointers to nodes retired at `E−1` after it pinned.

  </details>

- [ ] Roughly what does a `pin()` cost when the thread is already pinned,
  and why?
  <details><summary>Answer</summary>

  Essentially nothing — a non-atomic increment. `Local::pin` reads
  `guard_count` (`internal.rs:406`), a plain `Cell<usize>` at `:306`, and
  the entire announcement block is behind `if guard_count == 0` (`:409`).
  Only the *outermost* pin does the store-plus-fence at `:446-447`.

  It can be a plain `Cell` because only the owning thread ever touches it —
  the same reasoning that lets `pin_count` (`:314`) and the bag (`:303`) be
  non-atomic. Everything a reader writes frequently is thread-private;
  everything shared is either read-mostly or `CachePadded`. That is the
  design, in one sentence.

  </details>

- [ ] `try_advance` is O(threads). Compute what that costs per operation at
  16 threads on this machine, and say why the answer is not alarming.
  <details><summary>Answer</summary>

  The scan does one load per registered `Local` (`internal.rs:249-270`).
  Each `Local.epoch` is `CachePadded` (`:317`) and was last written by its
  own thread, so each load pulls a line across cores — **38.3 ns** on this
  machine (`false_sharing`: 40.54 ns contended minus 2.28 ns uncontended).
  16 threads ⇒ ≈ **610 ns** per `try_advance`.

  It is not alarming because of the divisor. `try_advance` runs once per
  `PINNINGS_BETWEEN_COLLECT = 128` pins (`:335`, `:456`), so 610 / 128 ≈
  **4.8 ns per operation** — under 10% of the ~52 ns a `scaling`-lane
  operation costs at 16 threads — and it is `#[cold]` (`:236`), so it stays
  out of the hot instruction path. Hazard pointers pay the same O(T) scan
  **per free** rather than per 128 pins; that divisor is the entire
  argument for epochs.

  </details>

- [ ] Name the single thread behaviour that wedges the scheme, and the two
  API calls that exist to prevent it.
  <details><summary>Answer</summary>

  A thread that **stays pinned**: blocked on I/O, running a multi-second
  scan, or stopped in a debugger. The check at `internal.rs:262` sees it
  pinned in an older epoch and returns early forever, so the global epoch
  freezes, `is_expired` (`:160`) is never true, and the queue of sealed
  bags grows without bound. Nothing in the library bounds it — that is the
  price of not publishing per-pointer.

  `Guard::repin` (`guard.rs:329`) and `Guard::repin_after`
  (`guard.rs:366`) exist for exactly this: they unpin and re-pin, giving
  the collector a window. Their signatures invalidate every `Shared` you
  held, so the compiler makes you re-load your pointers — the protocol
  violation you would otherwise commit is a type error.

  </details>

- [ ] `try_advance` loads each thread's epoch with `Relaxed`. Where does
  the ordering come from, and why is it done that way?
  <details><summary>Answer</summary>

  From two fences, not from the loads: `atomic::fence(Ordering::SeqCst)` at
  `internal.rs:239`, before the scan, and `atomic::fence(Ordering::Acquire)`
  at `:276`, after it. The per-`Local` load at `:258` is `Relaxed`.

  It is done that way because ordering is paid per *fence*, not per access.
  Attaching `Acquire` to each of T loads would emit T ordering constraints
  where one suffices — the same economy as pinning once per operation
  rather than once per pointer (Step 2), and the same reason `push_bag`
  puts its fence at `:194` before a single `Relaxed` epoch load at `:196`.
  If you copy this pattern into your own collector, copy the fences too;
  dropping them is the classic bug that passes on x86 (where loads are
  acquire-ish anyway) and fails on the ARM Mac you are running on.

  </details>

- [ ] Retiring a node touches shared memory how often, and what happens on
  the exception?
  <details><summary>Answer</summary>

  **Once every 64 retires.** `Local::defer` (`internal.rs:382-389`) pushes
  into `self.bag`, an `UnsafeCell<Bag>` in the thread's own `Local`
  (`:303`) that holds `MAX_OBJECTS = 64` entries (`:66`). The `while let
  Err(d) = bag.try_push(deferred)` loop at `:385` only takes its body when
  the bag is full.

  On the 64th, `push_bag` (`:191-198`) swaps in a fresh bag, executes a
  `SeqCst` fence, loads the current global epoch, and pushes the sealed bag
  onto the global lock-free queue. The fence comes *before* the epoch load
  on purpose: without it the thread could stamp its garbage with an epoch
  older than the unlink it just performed, and under-stamped garbage is
  garbage freed too early.

  </details>

## References

**Code** (pinned at `crossbeam-rs/crossbeam@6b7458d`)

| File | Lines | What |
|---|---|---|
| `crossbeam-epoch/src/default.rs` | 42 | `pin()` — the entry point everything else serves |
| `crossbeam-epoch/src/guard.rs` | 70 | `Guard` — read the doc comment, it is the contract |
| | 90, 189, 271 | `defer`, `defer_unchecked`, `defer_destroy` |
| | 295, 329, 366 | `flush`, `repin`, `repin_after` — the long-reader escape hatches |
| `crossbeam-epoch/src/internal.rs` | 66 | `MAX_OBJECTS = 64` — the local bag's capacity |
| | 155–162 | `SealedBag::is_expired` — the entire freeing rule |
| | 165–174 | `Global` — the `Local` list, the bag queue, the padded epoch |
| | 178, 208–226 | `COLLECT_STEPS = 8` and the bounded `collect` loop |
| | 191–198 | `push_bag` — fence, then stamp with the epoch |
| | 237–288 | `try_advance` — the O(T) scan, its bail-outs, its fences |
| | 291–318 | `Local` — what is `Cell`, what is atomic, what is `CachePadded` |
| | 335, 456 | `PINNINGS_BETWEEN_COLLECT = 128`, and where it is checked |
| | 382–389 | `Local::defer` — the thread-local fast path |
| | 403–462 | `Local::pin` — read this first if you read nothing else |
| `crossbeam-epoch/src/atomic.rs` | — | `Atomic<T>`, `Owned<T>`, `Shared<'g, T>` |
| `crossbeam-utils/src/cache_padded.rs` | 70–77, 87–94 | why `CachePadded` is 128 B on aarch64 too |

Read order: `default.rs` → `guard.rs` → `internal.rs` (bottom-up from
`Local::pin`). About 1.5 h.

**Measurements** — see `notes.md` for full lane output, `FINDINGS.md` row 9
for the headline.

| Lane | Figure used above |
|---|---|
| `false_sharing` | uncontended padded atomic RMW = **2.28 ns**; one cross-core line transfer = **38.3 ns** |
| `false_sharing` | packed vs pad128 = **17.8×**; pad64 vs pad128 = **1.8×** |
| `scaling` | crossbeam `SkipSet` 4.21 → 19.28 Mops/s from 1 to 16 threads (≈52 ns/op at 16) |

**Cross-topic** — the skiplists guide for the structure this collector is
protecting; the LWLock guide, Step 3, for what a *shared* line costs when
you do not pad; topic 0 §2 for the memory hierarchy the 38.3 ns sits in.
