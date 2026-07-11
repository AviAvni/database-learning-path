# FoundationDB & Antithesis: the whole cluster in one thread

FoundationDB made the most radical testing bet in databases: design
the entire distributed system so it can run — every node, disk, and
network — inside one deterministic thread, then spend the saved
debugging time injecting compressed chaos. This chapter walks that
design philosophy, the Flow language that makes it possible, and
Antithesis, where the same founders push the determinism boundary
down to a hypervisor so unmodified systems get it for free. It's the
"in the large" version of what our `dst.rs` stub does in miniature.

## The FDB bet

FoundationDB (2010s) decided the database and its test harness are
ONE artifact: the entire distributed system — every node, disk,
network — runs single-threaded inside one process, scheduled by a
seeded event loop.

```
 ┌─ one OS process, one thread ────────────────────────┐
 │  simulated cluster: N "machines" as actor sets      │
 │  SimClock      — logical time, jumps to next event  │
 │  SimNetwork    — seeded delays, drops, PARTITIONS   │
 │  SimDisk       — seeded corruption, torn writes,    │
 │                  "disk that lies" (bit rot)         │
 │  + BUGGIFY(p)  — code-embedded chaos macros         │
 └──────────────────────────────────────────────────────┘
```

Flow (their C++ dialect) exists to make this possible: actors +
futures compile to deterministic state machines; `wait()` yields to
the simulator's scheduler. No pthreads in the data path — the same
discipline raft-rs reaches by being sans-io (reading-raft-rs.md).

## The three pillars

1. **Determinism**: one seed reproduces a whole-cluster failure,
   including the partition timings. (Our topic 15 sim.rs in the
   large.)
2. **BUGGIFY**: ~800 macros in the FDB codebase that, in simulation
   only, make rare paths common — "pretend the buffer is full",
   "return commit_unknown_result". The SUT *cooperates* with the
   tester. Question: why is injecting at the semantic level
   (commit_unknown_result) more powerful than at the syscall level
   (EIO)?
3. **Test oracles as workloads**: swizzled clogging, machine kills
   mid-recovery, dumb sanity workloads — each asserts invariants
   (e.g., a read at version v sees all commits ≤ v) rather than
   specific outputs.

The whole architecture reduces to a seeded event loop plus one macro:

```rust
// the "cluster" advances by popping the next event — no threads, no sleeps
fn run(seed: u64) {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut events = BinaryHeap::new();          // min-heap on fire_time
    while let Some((t, ev)) = events.pop() {
        clock.jump_to(t);                        // logical time TELEPORTS
        for follow_up in step(ev, &mut rng) {    // deliver, drop, delay, corrupt…
            events.push(follow_up);
        }
    }
}

fn buggify(rng: &mut impl Rng, p: f64) -> bool {
    cfg!(simulation) && rng.random_bool(p)       // rare paths made common;
}                                                // compiled out in production
```

The famous claim: FDB found so few bugs in production because the
simulator ran *millions of cluster-years* of compressed chaos —
CPU-bound, so faster than real time.

## Antithesis: the generalization

Same founders, next act: if you can't rewrite your system in Flow,
put the WHOLE VM under a deterministic hypervisor — every syscall,
interrupt, and thread interleaving is replayable. Coverage-guided
exploration ("multiverse debugging") decides which random branches
to explore deeper. turso runs its Dockerfile.antithesis image there.

```
 approach            determinism boundary      rewrite cost
 ──────────────────────────────────────────────────────────
 FDB / Flow          language runtime          total (Flow)
 turso simulator     IO/clock traits           moderate (DI)
 topic-15 sim.rs     message passing           small (sans-io)
 Antithesis          hypervisor                ZERO
```

## Questions for notes.md

1. FDB tests "the disk lies" (corruption, torn writes) — which of
   Raft's assumptions does this violate, and how does VSR/
   TigerBeetle thinking (reading-vsr.md) address it?
2. BUGGIFY is compiled out in production. What's the argument that
   test-only branches DON'T invalidate what you tested?
3. Simulation can't catch: (a) a compiler bug, (b) a kernel fsync
   lie, (c) a race in the simulator itself, (d) real-clock
   dependencies. For each: which layer of the table above catches
   it, if any?
4. Why does deterministic simulation get FASTER than real time for
   IO-bound workloads (logical clock jumps to next event)?
5. For M16: our engine already isolates IO behind traits (M5 WAL,
   M6 buffer pool). List the remaining nondeterminism sources to
   corral (threadpool from M9! HashMap iteration! rand in plans!).

## References

**Papers & docs**
- FoundationDB — "Simulation and Testing" + "Testimony" docs
  ([apple.github.io/foundationdb](https://apple.github.io/foundationdb/testimony.html))
  — the design-philosophy source; no clone needed
- Antithesis blog ([antithesis.com/blog](https://antithesis.com/blog))
  — by the FDB founders; the deterministic-hypervisor generalization
  and "multiverse debugging"

**Code**
- [foundationdb](https://github.com/apple/foundationdb) —
  `flow/README.md` — the Flow language: actors + futures compiled to
  deterministic state machines; skim for the `wait()`-yields-to-
  scheduler discipline rather than the C++ details
