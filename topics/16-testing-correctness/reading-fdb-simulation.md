# FoundationDB & Antithesis: the whole cluster in one thread

FoundationDB made the most radical testing bet in databases: design
the entire distributed system so it can run — every node, disk, and
network — inside one deterministic thread, then spend the saved
debugging time injecting compressed chaos. Before the docs, this
chapter builds the idea step by step: why distributed systems defeat
ordinary testing, how a seeded event loop simulates a whole cluster,
what the Flow language buys, why BUGGIFY injects faults at the
semantic level, and how Antithesis pushes the same determinism down
to a hypervisor so unmodified systems get it for free. It's the
"in the large" version of what our `dst.rs` stub does in miniature.

## The problem in one sentence

A distributed database's worst bugs need a partition, a machine
kill, and a recovery to overlap within milliseconds — an event a
real test cluster might produce once a month and never again — so
FDB rebuilt the system to make that event schedulable, seeded, and
replayable millions of times per night.

## The concepts, step by step

### Step 1 — why distributed systems defeat example-based testing

A distributed system's behavior depends not just on inputs but on
*orderings*: which message arrived first, which node paused, whether
a disk write completed before the crash. With N nodes exchanging
messages, the number of possible interleavings explodes
combinatorially, and the dangerous ones — partition during leader
election, crash mid-recovery — are vanishingly rare on healthy
hardware. Unit tests check one ordering; production eventually
explores all of them. The gap is where the bugs live. Worse, when a
rare ordering does fail, it's gone: real clocks, real threads, and
real networks never replay.

### Step 2 — the bet: the database and its test harness are ONE artifact

FoundationDB (2010s) decided not to bolt testing on afterward but to
design the system so the entire cluster — every node, disk, network
— runs single-threaded inside one process, scheduled by a seeded
event loop (an RNG-driven scheduler; a seed is the one number that
reproduces the whole random stream):

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

One thread means no OS scheduler in the picture — every interleaving
of "concurrent" events is chosen by the simulator's RNG, so a u64
seed reproduces a whole-cluster failure, including the partition
timings. (Our topic 15 sim.rs is this in the small.)

### Step 3 — the mechanism: a seeded event loop over a time-ordered heap

Strip the architecture to its core and it is a priority queue of
future events plus one macro. The "cluster" advances by popping the
next event; logical time *teleports* to that event's timestamp —
nothing ever sleeps:

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

Because time jumps instead of passing, an IO-bound workload runs
*faster than real time* — a simulated 30-second recovery costs
however long the CPU takes to process its events, often
milliseconds. That's how the famous claim works: the simulator ran
*millions of cluster-years* of compressed chaos before release,
which is why FDB found so few bugs in production.

### Step 4 — Flow: making the language deterministic

The event loop only works if no code path can escape it — no
pthreads, no blocking syscalls in the data path. Flow is FDB's C++
dialect built for exactly this: **actors** (independent state
machines that communicate only by messages) and **futures** (values
that arrive later) compile down to deterministic state machines, and
every `wait()` yields control back to the simulator's scheduler
instead of blocking a thread. The same discipline raft-rs reaches by
being sans-io (reading-raft-rs.md): logic that never touches the
outside world directly can be driven by anything — including a
seeded heap.

The cost is total: FDB rewrote itself in a private language to buy
determinism. Hold that price — Step 7's table is about who else pays
how much.

### Step 5 — BUGGIFY: the SUT cooperates with its tester

Fault injection from outside (kill a process, return EIO from a
syscall) only reaches the failures the environment can express.
BUGGIFY goes further: ~800 macros *inside* the FDB codebase that, in
simulation only, make rare paths common — "pretend the buffer is
full", "return commit_unknown_result", "trigger recovery now". The
system under test cooperates with the tester by exposing its own
rare branches as injectable events, at the semantic level where the
interesting states live. Question: why is injecting at the semantic
level (commit_unknown_result) more powerful than at the syscall
level (EIO)?

### Step 6 — oracles as workloads: assert invariants, not outputs

With chaos injected, who decides a run failed? Not expected outputs
— nobody knows the "right answer" of a randomized cluster-year. FDB
ships **workloads** that assert *invariants* (properties that must
hold in every legal execution): a read at version v sees all commits
≤ v; the cluster recovers to availability after any tolerated fault
set; swizzled clogging (partition, then heal in random order) never
loses acked data; machine kills mid-recovery never fork history.
Dumb sanity workloads plus invariants beat clever expected-value
tests because they stay valid under any interleaving — this is the
generator + oracle framing of the topic README, at cluster scale.

### Step 7 — Antithesis: buy determinism at the hypervisor instead

Same founders, next act: if you can't rewrite your system in Flow,
put the WHOLE VM under a deterministic hypervisor — every syscall,
interrupt, and thread interleaving is recorded and replayable, so
*unmodified* binaries get FDB-grade reproducibility. On top,
coverage-guided exploration ("multiverse debugging" — fork the
simulation at interesting branch points and explore the divergent
universes) decides which random branches to push deeper. turso runs
its Dockerfile.antithesis image there.

The whole design space is one table — determinism boundary vs
rewrite cost:

```
 approach            determinism boundary      rewrite cost
 ──────────────────────────────────────────────────────────
 FDB / Flow          language runtime          total (Flow)
 turso simulator     IO/clock traits           moderate (DI)
 topic-15 sim.rs     message passing           small (sans-io)
 Antithesis          hypervisor                ZERO
```

Lower boundary = more of the world captured (Antithesis catches
thread races Flow defines away); higher boundary = cheaper to adopt
but more nondeterminism left uncorralled.

## How to read the sources (with the concepts in hand)

1. **FDB "Simulation and Testing" + "Testimony" docs** — the design
   philosophy in the authors' words. Read with Steps 2–3 in hand:
   every section is either the event loop, BUGGIFY (Step 5), or a
   workload oracle (Step 6). No clone needed.
2. **`flow/README.md`** in the FDB repo — skim for the
   `wait()`-yields-to-scheduler discipline (Step 4) rather than the
   C++ details; the point is what a language must give up to be
   simulatable.
3. **Antithesis blog** — read one or two posts for the
   deterministic-hypervisor claim and multiverse debugging (Step 7);
   map every capability they advertise onto the table above.

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
