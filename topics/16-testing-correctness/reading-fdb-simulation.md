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

Two sourcing notes, because this topic has more folklore than any
other in the book. Every architectural claim below is cited to §4
("Simulation Testing") of *FoundationDB: A Distributed Unbundled
Transactional Key Value Store* (SIGMOD 2021) — which contains, note
carefully, **no numbers at all**; it is entirely qualitative. Every
code anchor is `apple/foundationdb` at commit **`4c775a9`**, the
revision this repo pins. Where the talks and blog posts claim
figures the paper does not, this chapter says so rather than
repeating them.

## The problem in one sentence

A distributed database's worst bugs need a partition, a machine
kill, and a recovery to overlap within milliseconds — an event a
real test cluster might produce once a month and never again — so
FDB rebuilt the system to make that event schedulable, seeded, and
replayable.

## The concepts, step by step

### Step 1 — why distributed systems defeat example-based testing

> **In:** N nodes exchanging messages, each of which may be
> delayed, dropped, or reordered.
> **Out:** a space of *orderings*, not a space of inputs — and the
> dangerous orderings are the rare ones.

A distributed system's behavior depends not just on inputs but on
*orderings*: which message arrived first, which node paused, whether
a disk write completed before the crash. Unit tests check one
ordering; production eventually explores all of them. The gap is
where the bugs live.

Put a number on "explodes". Take a single round in which each of `n`
nodes sends one message, and ask only how many delivery orders exist:

```
 n = 3 nodes, 1 message each     3! = 6            enumerable
 n = 5                           5! = 120          enumerable
 n = 5, three rounds             (5!)^3 ≈ 1.7×10^6 borderline
 n = 5, three rounds, each message may also be dropped
                                 × 2^15 = 32,768
                                 ≈ 5.7 × 10^10     not enumerable

 and a real recovery involves hundreds of messages, not fifteen.
```

Worse, when a rare ordering does fail, it's gone: real clocks, real
threads, and real networks never replay. You get a stack trace and
no way back.

Why it matters: the problem is not "we need more tests". It is that
the axis you must cover is not an axis your test framework can
address, and no amount of examples fixes that.

### Step 2 — the bet: the database and its test harness are ONE artifact

> **In:** permission to constrain how the production code is
> written.
> **Out:** a whole cluster that fits in one thread, whose entire
> execution is a function of one seed.

FoundationDB decided not to bolt testing on afterward but to design
the system so the entire cluster runs single-threaded inside one
process, scheduled by a seeded event loop (an RNG-driven scheduler;
a **seed** is the one number that reproduces the whole random
stream). The paper's §4 states the constraint and the abstraction
boundary in two sentences:

> "All database code is deterministic; … one database node is
> deployed per core." … "the simulator … abstracts away all sources
> of nondeterminism — network, disk, time, and PRNG."

Four sources, named exactly. That list is the checklist for anything
you build yourself:

```
 ┌─ one OS process, one thread ────────────────────────┐
 │  simulated cluster: N "machines" as actor sets      │
 │  SimClock      — logical time, jumps to next event  │
 │  SimNetwork    — seeded delays, drops, PARTITIONS   │
 │  SimDisk       — seeded corruption, torn writes,    │
 │                  "disk that lies" (bit rot)         │
 │  + buggify()   — code-embedded chaos, Step 5        │
 └──────────────────────────────────────────────────────┘
```

One thread means no OS scheduler in the picture — every interleaving
of "concurrent" events is chosen by the simulator's RNG, so a u64
seed reproduces a whole-cluster failure, including the partition
timings. (Our topic 15 `sim.rs` is this in the small.) The
production build swaps the same interfaces for real ones: §4 says
"The production implementation is a simple shim to the relevant
system calls" — the *simulated* implementation is the elaborate one.

Why it matters: this is the only step that costs anything. Steps 3
through 6 are what you get for free once you have paid it.

### Step 3 — the mechanism: a seeded event loop over a time-ordered heap

> **In:** a seed and a set of pending events.
> **Out:** one exact execution — and, for IO-bound work, one that
> completes faster than the wall-clock interval it simulates.

Strip the architecture to its core and it is a priority queue of
future events plus one predicate. The "cluster" advances by popping
the next event; logical time *teleports* to that event's timestamp —
nothing ever sleeps:

```text
// ILLUSTRATION — the shape of a seeded event loop, not quoted from
// FoundationDB (whose real version is Flow's Net2 runner). The real
// per-site chaos predicate is flow/include/flow/Buggify.h:92-96,
// quoted verbatim in Step 5.
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
```

Because time jumps instead of passing, an IO-bound workload runs
*faster than real time*. Work the ratio with this topic's own
measured harness, which is the same idea at small scale:

```
 crash_matrix (this topic, notes.md baseline):
   5,000 seeds × 40 ops, five bug variants
   wall clock                        ≈ 0.02 s per 5,000-seed sweep
   → ≈ 200,000 simulated crash-recoveries per second

 a real crash-recovery on real hardware: ≈ 1 s (process restart + WAL replay)
 speedup ≈ 200,000 ×  — i.e. one second of simulator ≈ 2.3 days of cluster

 the ratio is not magic: it is the fraction of "elapsed" time that was
 a sleep. Replace sleeps with a heap pop and that time costs nothing.
```

Now the discipline. The famous claim is that FDB simulated *millions
of cluster-years* before release. **That number is not in the
paper** — §4 is entirely qualitative — so this chapter does not
assert it. What the paper *does* claim, in §6.2 and with a named
deployment, is checkable:

> CloudKit deployed FoundationDB for "more than 0.5M disk years
> without a single data corruption event."

Disk years of *production*, not simulated cluster-years. That is the
number to quote.

Why it matters: the speedup is the entire economic argument for DST,
and it is computable from your own harness rather than borrowed from
a conference talk.

### Step 4 — Flow: making the language deterministic

> **In:** C++, which has threads, blocking syscalls, and no way to
> stop you using them.
> **Out:** a dialect in which the scheduler is the only thing that
> can make progress.

The event loop only works if no code path can escape it — no
pthreads, no blocking syscalls in the data path. Flow is FDB's C++
dialect built for exactly this. §4's description:

> "a novel syntactic extension to C++ adding async/await-like
> concurrency primitives"

**Actors** (independent state machines that communicate only by
messages) and **futures** (values that arrive later) compile down to
deterministic state machines, and every `wait()` yields control back
to the simulator's scheduler instead of blocking a thread. The same
discipline raft-rs reaches by being sans-io
([reading-raft-rs.md](../15-replication-consensus/reading-raft-rs.md)): logic
that never touches the outside world directly can be driven by
anything — including a seeded heap.

The cost is total: FDB rewrote itself in a private language to buy
determinism. And the paper is honest that the boundary of the
rewrite is the boundary of the testing (§4, Limitations):

> simulation "cannot test the performance of the real system"; it
> "cannot test third-party libraries or code that is not written in
> Flow"; and "several bugs have resulted from the true operating
> system contract being weaker than it was believed to be."

That last clause is the one to remember: a simulated disk implements
the fsync contract *you believe in*. If the kernel's is weaker, your
simulator is wrong in exactly the direction that hurts.

Hold that price — Step 7's table is about who else pays how much.

Why it matters: "make it deterministic" is not a code style. It is a
language-level property, and every project in this topic pays for it
at a different layer.

### Step 5 — buggify: the SUT cooperates with its tester

> **In:** a rare branch in production code that a black-box tester
> could never reach on demand.
> **Out:** a one-token annotation that makes that branch common — in
> simulation only, and reproducibly per seed.

Fault injection from outside (kill a process, return EIO from a
syscall) only reaches the failures the environment can express.
"Buggification", the paper's own word (§4), goes further: annotations
*inside* the FDB codebase that, in simulation only, make rare paths
common. The system under test cooperates with the tester by exposing
its own rare branches as injectable events, at the semantic level
where the interesting states live.

First correction to the folklore: **there is no `BUGGIFY` macro at
`4c775a9`.** It is now a function, and reading it repays the minute
it takes:

```c
// flow/include/flow/Buggify.h — the per-site activation macro and buggify(), 51-96 (elided)
    51  #define __GENERATE_BUGGIFY_VARIABLES(TYPE, Type, type)                     \
    52  	inline double P_##TYPE##_BUGGIFIED_SECTION_ACTIVATED{ 0.25 };           \
    53  	inline double P_##TYPE##_BUGGIFIED_SECTION_FIRES{ 0.25 };               \
    54  	inline double P_##TYPE##_ENABLED{ false };                              \
    55  	inline std::unordered_map<BuggifySection, bool, BuggifySectionHash> Type##_SBVars;  \
// ... 56-67: is/enable/disable/clear accessors over P_##TYPE##_ENABLED ...
    68  	inline bool get##Type##SBVar(const char* file, const int line) {        \
    69  		const BuggifySection section{ file, line };                         \
    70  		const auto sectionItr = Type##_SBVars.find(section);                \
    71  		if (sectionItr != Type##_SBVars.end()) [[likely]] {                 \
    72  			return sectionItr->second;                                      \
    73  		}                                                                   \
    75  		const double rand = deterministicRandom()->random01();              \
    76  		const bool activated = rand < P_##TYPE##_BUGGIFIED_SECTION_ACTIVATED;  \
    77  		Type##_SBVars.emplace(section, activated);                          \
    78  		g_traceBatch.addBuggify(activated, line, file);                     \
// ... 79-84: dump the trace, return activated ...
    92  inline bool buggify(double probability = P_GENERAL_BUGGIFIED_SECTION_FIRES,
    93                      const std::source_location location = std::source_location::current()) {
    94  	return isGeneralBuggifyEnabled() && getGeneralSBVar(location.file_name(), static_cast<int>(location.line())) &&
    95  	       deterministicRandom()->random01() < probability;
    96  }
```

Three conditions on line 94–95, and they are not the same condition:

1. `isGeneralBuggifyEnabled()` — the global switch (`:54`, default
   `false`, so buggify is inert outside simulation).
2. `getGeneralSBVar(file, line)` — a **per-site, memoized** coin.
   Line 70–73 looks the site up in a map keyed by `(file, line)`
   (`BuggifySection`, `:38-43`); only on first encounter does line
   75–76 flip a coin at `P_GENERAL_BUGGIFIED_SECTION_ACTIVATED`
   (0.25) and *remember it for the whole run*.
3. `deterministicRandom()->random01() < probability` — a fresh coin
   per *call*, defaulting to `P_GENERAL_BUGGIFIED_SECTION_FIRES`
   (0.25).

So compute the odds an unconditional buggify site fires on a given
execution:

```
 P(site activated for this run)   = 0.25      (memoized once, line 76)
 P(a given call fires | activated) = 0.25      (line 95)

 P(a given call fires)             = 0.25 × 0.25 = 0.0625 = 1 in 16

 but activation is per RUN, not per call, so over many calls in ONE run:
   3 runs in 4:  the site NEVER fires, however often it is reached
   1 run in 4:   the site fires on ~1 call in 4

 EXPENSIVE_VALIDATION (:98) is different: P_EXPENSIVE_VALIDATION = 0.05
 (:36) with NO memoization — a fresh 1-in-20 coin every single call.
```

That two-level structure is the whole idea, and it is Step 6's
**swarm testing** in miniature: a run in which a site is *never*
buggified explores the normal path deeply; a run in which it *is*
explores the rare path repeatedly. Flipping the coin per call would
give every run the same shallow mixture.

Second correction: how many sites are there? "About 800" is the
folklore figure and it is not in the paper. What is checkable is the
tree. Grepping `buggify(` across `fdbserver/` at `4c775a9` returns
**369 call sites**, of which **246 are in a single file**,
`fdbserver/core/ServerKnobs.cpp`. That file is not a fault injector
at all — it is the tuning-parameter randomizer:

```c
// fdbserver/core/ServerKnobs.cpp — a representative knob, 164
   164  	init( MAX_COMMIT_BATCH_INTERVAL,  2.0 ); if( randomize && buggify() ) MAX_COMMIT_BATCH_INTERVAL = 0.5; // ...
```

Read that line twice. Two thirds of FDB's buggify sites exist to
make *tuning constants* wrong on purpose, which is §4's point that
"randomization of tuning parameters also ensures that specific
performance tuning values do not accidentally become necessary for
correctness". The remaining ~123 sites across `storageserver`,
`TLogServer`, `VersionedBTree`, `DiskQueue`, `CoordinatedState` and
the workloads are the semantic fault injectors people mean when they
say BUGGIFY.

Question: why is injecting at the semantic level
(`commit_unknown_result`) more powerful than at the syscall level
(EIO)?

Why it matters: the memoization at line 77 is the difference between
a chaos monkey and a search strategy, and it is four lines of code.

### Step 6 — oracles as workloads: assert invariants, not outputs

> **In:** a randomized cluster-hour with faults injected throughout.
> **Out:** a verdict — from invariants, because nobody knows the
> "right answer".

With chaos injected, who decides a run failed? Not expected outputs.
FDB ships **workloads** that assert *invariants* (properties that
must hold in every legal execution). §4 defines the class precisely:

> "the test oracle … verifies invariants that can only be maintained
> through proper atomicity and isolation", plus checks that the
> cluster recovers within a set time.

Concretely: a read at version v sees all commits ≤ v; the cluster
recovers to availability after any tolerated fault set; swizzled
clogging (partition, then heal in random order) never loses acked
data; machine kills mid-recovery never fork history. Dumb sanity
workloads plus invariants beat clever expected-value tests because
they stay valid under any interleaving — this is the generator +
oracle framing of the topic README, at cluster scale.

The fault menu §4 names is worth copying verbatim into your own
design doc: machine, rack and datacenter **fail-stop failures and
reboots**; network faults, **partitions**, and latency; disk
**corruption of unsynchronized writes on reboot**; and randomized
event times. Then the sentence everyone skips:

> "Fault injection distributions are carefully tuned to avoid
> driving the system into a small state-space caused by an excessive
> fault rate."

Too much chaos is *worse* than too little — a cluster that is always
partitioned only ever exercises the "we are partitioned" path. Your
fault probability is a tuning parameter with an interior optimum,
not a dial to turn to 11. This topic's own `crash_matrix` is the
same lesson from the other side: at a 10% crash rate over 40 ops the
`TornWriteAccepted` bug is caught in only 2,442 of 5,000 seeds
(48.8%) while `NoSyncOnCommit` is caught in 4,980 (99.6%) — same
harness, same fault rate, an order-of-magnitude difference in how
often you find out.

The randomization is *coordinated*, which the paper calls **swarm
testing** (citing Groce et al.):

> "each cluster is randomly configured with different cluster sizes,
> configurations, workloads, fault injection parameters, tuning
> parameters, and enables and disables a different random subset of
> buggification points."

That last clause is Step 5's `P_GENERAL_BUGGIFIED_SECTION_ACTIVATED`
in prose: each run gets a *different subset* of chaos, not the
average of all of it.

Coverage is tracked at the same granularity, with a macro whose
literal form the paper gives:

```
 TEST( buffer.is_full() );      // buffer is full
```

— which counts, in the paper's words, "the number of distinct
simulation runs" that reached the condition. Not lines. Not
branches. *Runs that reached the interesting state*, which is the
only coverage metric that means anything once every run is a
different configuration.

Why it matters: this step is the transferable one. You will not
write Flow; you will absolutely write invariant workloads and a
fault-rate tuning curve.

### Step 7 — Antithesis: buy determinism at the hypervisor instead

> **In:** a system you are not allowed to rewrite.
> **Out:** the same reproducibility, purchased at a lower layer for
> a different price.

Same founders, next act: if you can't rewrite your system in Flow,
put the WHOLE VM under a deterministic hypervisor — every syscall,
interrupt, and thread interleaving is recorded and replayable, so
*unmodified* binaries get FDB-grade reproducibility. On top,
coverage-guided exploration ("multiverse debugging" — fork the
simulation at interesting branch points and explore the divergent
universes) decides which random branches to push deeper.

turso runs there, and you can check it rather than take it on faith:
`Dockerfile.antithesis` at the repo root and
`.github/workflows/antithesis.yml` (a scheduled run with a
240-minute default duration and an optional `diff_base` for
targeted coverage), driving the workloads in
`testing/antithesis/bank-test/` and
`testing/antithesis/stress-composer/`, with
`scripts/antithesis/diff_to_targeted_coverage.py` turning a diff into
a coverage target.

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
thread races Flow defines away, and — per Step 4's Limitations —
third-party libraries Flow cannot see); higher boundary = cheaper to
adopt but more nondeterminism left uncorralled.

One measured data point on why the boundary matters, from the
paper's §6.2: FDB originally used Zookeeper for coordination, and
"fault injection found two independent bugs (circa 2010)" in it —
after which Zookeeper was deleted and replaced with a de novo Paxos
implementation *written in Flow*. The lesson is not that Zookeeper
was bad; it is that the moment a component sits outside your
determinism boundary, you either move it inside or stop testing it.

Why it matters: this is the decision you actually face on your own
codebase, and the table's second column is what your team will
argue about.

## How to read the sources (with the concepts in hand)

1. **The SIGMOD 2021 paper, §4 ("Simulation Testing")** — three
   pages, and the authoritative source for every claim in Steps 2,
   4, 5 and 6. Read it noticing what is *not* there: no bug counts,
   no cluster-years, no coverage percentages. Anyone quoting a
   number and citing "the FoundationDB paper" is quoting something
   else.
2. **FDB "Simulation and Testing" / "Testimony" docs** — the same
   philosophy with more colour and less rigour; use them for
   intuition, not for figures.
3. **`flow/include/flow/Buggify.h`** (133 lines) — read it all.
   Step 5 walks the two-level coin; also note `EXPENSIVE_VALIDATION`
   (`:98`) and the separate `CLIENT_BUGGIFY` axis (`:100-102`).
4. **`flow/include/flow/CodeProbe.h`** and
   **`flow/SimBugInjector.cpp`** — the modern successors to the
   paper's `TEST()` macro and the hand-rolled injectors.
5. **`fdbserver/core/ServerKnobs.cpp`** — skim any 40 lines. This
   is where two thirds of the buggify sites live, and seeing that
   they are knob randomizers rather than fault injectors permanently
   fixes the mental model.
6. **`flow/README.md`** — skim for the `wait()`-yields-to-scheduler
   discipline (Step 4) rather than the C++ details.
7. **Antithesis blog** — read one or two posts for the
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

## Done when

Answer each before unfolding it.

- [ ] You can explain why example-based tests lose to distributed systems, in terms of interleaving count.

  <details><summary>Answer</summary>

  Because the axis that matters is *ordering*, not input. Five nodes
  sending one message each gives `5! = 120` delivery orders; three
  such rounds gives `(5!)^3 ≈ 1.7 × 10^6`; allow each of those
  fifteen messages to also be dropped and it is `× 2^15`, about
  `5.7 × 10^10`. A real recovery involves hundreds of messages.

  An example-based test pins one point in that space — whichever one
  your machine happened to produce. And the failing point is not
  recoverable afterwards: real clocks, threads and networks do not
  replay, so even a production failure gives you a stack trace and no
  way back to it.

  </details>

- [ ] You can state the bet: the database and its test harness are one artifact, and say what that forbids in the production code.

  <details><summary>Answer</summary>

  The bet: make the *production* code deterministic so the whole
  cluster can run inside one thread under a seeded scheduler. §4:
  "All database code is deterministic; … one database node is
  deployed per core."

  What it forbids: threads in the data path, blocking syscalls, and
  any direct use of the four nondeterminism sources §4 names —
  **network, disk, time, and PRNG**. Every one of those goes through
  an interface whose production implementation is, per §4, "a simple
  shim to the relevant system calls" and whose simulated
  implementation is the interesting one.

  It also forbids third-party code in the data path — which is why
  §4's Limitations concede simulation "cannot test third-party
  libraries or code that is not written in Flow", and why Zookeeper
  was eventually replaced by a Paxos implementation in Flow (§6.2).

  </details>

- [ ] You can describe the seeded event loop over a time-ordered heap, and say why simulation runs *faster* than real time.

  <details><summary>Answer</summary>

  A min-heap of `(fire_time, event)`. Pop the earliest, set logical
  time *to* it, process it, push whatever follow-up events it
  generates. Nothing ever sleeps and nothing ever blocks, so the only
  cost is CPU spent processing events.

  It runs faster than real time exactly to the extent that the
  workload was waiting rather than computing. A simulated 30-second
  recovery that is 99.9% waiting costs 30 ms of CPU.

  This topic's own harness measures the ratio: `crash_matrix` sweeps
  5,000 seeds × 40 ops in about 0.02 s, roughly 200,000 simulated
  crash-recoveries per second, against a wall-clock crash-recovery of
  order one second. Note the corollary: for a *CPU-bound* workload
  the speedup is 1× or worse, because there was no waiting to delete.

  </details>

- [ ] You can explain what buggify is and give the argument for why compiling it out of production is not cheating.

  <details><summary>Answer</summary>

  It is an in-source annotation that, in simulation only, takes a
  rare branch. At `4c775a9` it is a function, not a macro:
  `buggify(probability, source_location)` at
  `flow/include/flow/Buggify.h:92-96`, gated on three conditions —
  the global enable (`:54`, default `false`), a memoized per-`(file,
  line)` activation coin at 0.25 (`:68-84`), and a per-call firing
  coin at 0.25 (`:95`).

  Why it is not cheating: buggify never adds behaviour, it only
  *selects among behaviours the production code already contains*.
  `MAX_COMMIT_BATCH_INTERVAL = 0.5` (`ServerKnobs.cpp:164`) is a
  legal value of a legal knob; `commit_unknown_result` is a status
  the client must already handle. The branch taken under buggify is
  a branch production can take — it is simply one that needs a
  once-a-year coincidence to reach.

  The honest caveat is the paper's own (§4, Limitations): "several
  bugs have resulted from the true operating system contract being
  weaker than it was believed to be." Buggify explores *your model*
  of the rare paths. If the model is wrong, so is the exploration.

  </details>

- [ ] You can compute how often an unconditional buggify site fires, and explain why the odds are structured in two levels rather than one.

  <details><summary>Answer</summary>

  `0.25 × 0.25 = 0.0625`, one call in sixteen — but that flat number
  hides the structure. `P_GENERAL_BUGGIFIED_SECTION_ACTIVATED` (0.25,
  `Buggify.h:52`) is drawn **once per site per run** and memoized in
  `General_SBVars` keyed by `(file, line)` (`:68-77`).
  `P_GENERAL_BUGGIFIED_SECTION_FIRES` (0.25, `:53`) is drawn per
  call (`:95`).

  So in three runs out of four the site never fires no matter how
  often it is reached; in the fourth it fires on about one call in
  four. Two levels, not one, because that is **swarm testing**: §4
  says each run "enables and disables a different random subset of
  buggification points". A single per-call coin would give every run
  the same thin mixture of chaos and explore nothing deeply.

  Contrast `EXPENSIVE_VALIDATION` (`:98`), which uses
  `P_EXPENSIVE_VALIDATION = 0.05` (`:36`) with **no** memoization —
  a flat 1-in-20 per call, because it is a check, not a behaviour
  change, and there is nothing to explore deeply.

  </details>

- [ ] You can name three bug classes simulation provably cannot catch.

  <details><summary>Answer</summary>

  §4's Limitations gives three directly:

  1. **Performance bugs** — "cannot test the performance of the real
     system", because logical time is not real time and the whole
     point of Step 3 is that waiting costs nothing.
  2. **Anything outside the determinism boundary** — "cannot test
     third-party libraries or code that is not written in Flow".
     Zookeeper (§6.2) is the worked example.
  3. **Wrong assumptions about the environment** — "several bugs have
     resulted from the true operating system contract being weaker
     than it was believed to be." The simulated disk implements the
     fsync semantics you coded, not the ones your kernel has.

  Add a fourth that follows from Step 2: a bug in the simulator
  itself is invisible, because the simulator is the oracle's notion
  of reality. This is why turso runs `crash_matrix`-style harnesses
  *and* Antithesis *and* a real-hardware suite.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including which of our IO traits already sit in the right place for M16.

  <details><summary>Answer</summary>

  No unfoldable answer — this one is the writing. Use §4's four-item
  list as the audit checklist: **network, disk, time, PRNG**. For
  question 5, the ones people forget are the last two: `HashMap`
  iteration order (Rust randomizes the seed per process), and any
  `rand::thread_rng()` reachable from a plan or a hash table. The
  thread pool from M9 is the expensive one, because corralling it
  means the M9 work has to become sans-io or single-threaded under a
  flag — which is Step 4's "rewrite cost" column arriving on your own
  codebase.

  </details>

## References

**Papers & docs**
- Zhou et al. — "FoundationDB: A Distributed Unbundled Transactional
  Key Value Store" (SIGMOD 2021) — **§4 is the authoritative source**
  for the determinism constraint, the four nondeterminism sources,
  Flow, buggification, swarm testing, the `TEST()` coverage macro,
  and the Limitations; §6.2 for CloudKit's 0.5M disk years and the
  Zookeeper replacement; §1 for the `f+1` replication choice
- FoundationDB — "Simulation and Testing" + "Testimony" docs
  ([apple.github.io/foundationdb](https://apple.github.io/foundationdb/testimony.html))
  — intuition, not figures
- Antithesis blog ([antithesis.com/blog](https://antithesis.com/blog))
  — by the FDB founders; the deterministic-hypervisor generalization
  and "multiverse debugging"

**Code** — [foundationdb](https://github.com/apple/foundationdb) @
`4c775a9`

| File | Lines | What |
|---|---|---|
| `flow/include/flow/Buggify.h` | 36 | `P_EXPENSIVE_VALIDATION{0.05}` — no memoization |
| `flow/include/flow/Buggify.h` | 38-49 | `BuggifySection{file, line}` and its hash — the memo key |
| `flow/include/flow/Buggify.h` | 51-84 | the per-axis variable generator: activation 0.25, fires 0.25, memoized `get*SBVar` |
| `flow/include/flow/Buggify.h` | 92-96 | `buggify()` — the three-condition predicate |
| `flow/include/flow/Buggify.h` | 98, 100-102 | `EXPENSIVE_VALIDATION` and the separate `CLIENT_BUGGIFY` axis |
| `fdbserver/core/ServerKnobs.cpp` | 164 | a representative knob randomizer — 246 of the tree's 369 `buggify(` sites live in this file |
| `flow/include/flow/CodeProbe.h` | — | the modern successor to the paper's `TEST()` coverage macro |
| `flow/SimBugInjector.cpp` | — | simulator-side injection |
| `flow/include/flow/DeterministicRandom.h` | — | the seeded PRNG every coin above draws from |
| `flow/README.md` | — | Flow: actors + futures compiled to deterministic state machines |

**Code** — [turso](https://github.com/tursodatabase/turso) @ `dd775bc`

| File | What |
|---|---|
| `Dockerfile.antithesis` | the image Antithesis runs |
| `.github/workflows/antithesis.yml` | scheduled run, 240-minute default, optional `diff_base` |
| `testing/antithesis/bank-test/`, `stress-composer/` | the workloads, i.e. Step 6's oracles |
| `scripts/antithesis/diff_to_targeted_coverage.py` | turns a diff into a coverage target |
