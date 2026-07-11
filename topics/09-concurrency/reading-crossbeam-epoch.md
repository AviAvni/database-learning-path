# Epoch reclamation: the GC that makes lock-free reads free

Lock-free deletion's boss fight is reclamation — when is it safe to
`free()` a node some reader might still hold? crossbeam-epoch answers with
three garbage bags and a global epoch counter, and it's the crate your
`concurrent_set.rs` builds on — read it first so `pin()` isn't magic.

## 1. The API surface (what you'll actually call)

- `epoch::pin()` (default.rs:42) → `Guard` (guard.rs:70). While a guard
  lives, no garbage from the current epoch is freed. Cost: ~one SeqCst
  fence + thread-local bump. Pin once per OPERATION, not per pointer.
- `Guard::defer_destroy(ptr)` (guard.rs:271) / `defer` (:90 — arbitrary
  closures, unchecked variant :189) — "free this when safe".
- `Atomic<T>` / `Shared<'g, T>`: an atomic pointer whose loads are
  lifetime-tied to a guard — the borrow checker enforces "no pointer
  outlives its pin". This is the Rust-shaped part hazard pointers lack.

## 2. The machinery (internal.rs)

- `Local` (:293) — per-thread: its pinned epoch + garbage bag. Threads
  register into a global intrusive list.
- `defer` (:382): garbage goes into the LOCAL bag first (no contention),
  sealed into the global queue tagged with the current epoch when full.
- The advance trigger: every `PINNINGS_BETWEEN_COLLECT = 128` pins
  (:335, check at :454–456), the pinning thread calls `collect` (:208)
  → `try_advance` (:237).
- `try_advance`: scan ALL registered threads; if anyone is pinned in an
  OLDER epoch, bail. Otherwise bump the global epoch. Freeing is then
  "pop bags ≥ 2 epochs old".

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

```
 global epoch: E
 thread A: pinned @ E      ─┐
 thread B: pinned @ E       ├─ all @ E ⇒ advance to E+1
 thread C: unpinned        ─┘
 bags: [E-2: freeable] [E-1: wait] [E: filling]
 one thread stuck pinned @ E-1 ⇒ epoch NEVER advances ⇒ unbounded garbage
 (the epoch weakness; hazard pointers bound garbage instead)
```

## 3. Idioms for your concurrent_set.rs

- Amortize-and-batch AGAIN: local bag → sealed batch → global queue →
  collect every 128 pins. Compare valkey's SPSC batches (topic 7) and
  redis incremental rehash (topic 2).
- `try_advance` is O(threads) — that's the cost hazard pointers pay per
  FREE; epochs pay it per ADVANCE attempt. Amortization decides winners.
- Read `Guard`'s docs on repinning (`repin`/`repin_after`) — long-running
  readers (a full graph scan!) must repin or they wedge the collector.
  This is M9's "reader holds a snapshot for 10 s" problem in miniature.

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
