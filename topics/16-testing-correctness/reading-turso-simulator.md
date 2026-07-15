# turso's simulator: every failure is a u64 seed

The most readable production DST codebase in Rust: seeded clock,
fault-injecting IO, metamorphic properties, and a shrinker, all in
one `testing/simulator/` tree. Before you open it, this chapter
builds the six ideas the code assumes — determinism, simulated time,
per-operation faults, metamorphic properties, doublecheck, and
shrinking — one at a time, then hands you the file-and-line anchor
map. Read it as the reference implementation for our `dst.rs` stub
and for M16 — every piece here has a miniature counterpart in the
experiments.

## The problem in one sentence

A crash-recovery bug that needs one specific interleaving of writes,
fsyncs, and a crash might show up once in a million runs — and when
it does, a conventional test can't replay it; DST makes every such
failure a single u64 you can re-run forever.

## The concepts, step by step

### Step 1 — determinism: make the program a pure function of a seed

Deterministic simulation testing (DST) is the discipline of removing
every source of randomness the program doesn't control — wall-clock
time, thread scheduling, IO timing, OS errors — and replacing each
with values drawn from one seeded pseudo-random number generator
(RNG: an algorithm that turns one starting number, the **seed**,
into an endless reproducible stream of "random" numbers). The system
under test (SUT — the code being tested) touches the outside world
only through interfaces, and in test those interfaces are backed by
the RNG:

```
 real:  code → syscalls → kernel  (time, threads, fsync — nondeterministic)
 DST:   code → traits ──→ SimClock  (ChaCha8 from seed)
                     ├──→ SimFile   (buffered; crash DROPS unsynced,
                     │               may TEAR the last write)
                     └──→ SimNet    (topic 15's sim.rs already did this)
        ⇒ failure = a u64 seed. Re-run seed = same bug, every time.
```

The payoff is the whole game: run seed 0, 1, 2, … overnight; any
assertion failure prints its seed; re-running that seed reproduces
the exact same interleaving, faults and all. Debugging a
one-in-a-million bug becomes ordinary single-run debugging. The cost
is architectural: the SUT must own NO nondeterminism, which is why
turso routes all IO and time through traits (dependency injection).

### Step 2 — simulated time: every `now()` is a seeded random jump

Once the clock is behind a trait, "time" is just data the simulator
makes up. turso's `SimulatorClock` advances the current time by a
random tick on *every read* — no wall clock exists anywhere:

```rust
// time is data: every now() consumes seeded randomness and ADVANCES
struct SimClock {
    curr: Duration,
    rng: ChaCha8Rng,                 // portable, versioned — never the default RNG
    min_tick: Duration,
    max_tick: Duration,
}

impl SimClock {
    fn now(&mut self) -> Instant {
        self.curr += self.rng.random_range(self.min_tick..self.max_tick);
        Instant::from(self.curr)     // monotone progress: timeout loops terminate
    }
}
```

Two design points hide in those ten lines. First, ChaCha8 — a
specific, versioned RNG — not `rand`'s default: the default
algorithm can change between crate releases, silently changing what
every archived seed means. Second, `now()` must ADVANCE rather than
return a fixed value: any loop of the form "retry until deadline"
polls the clock, and if time never moves, the simulation livelocks.
Question: why must `now()` ADVANCE time rather than return a fixed
value? (What loops forever if time never moves? Think timeout code.)

### Step 3 — fault injection at the file layer, per operation

Fault injection means deliberately making an operation fail the way
hardware and kernels really fail — and the realistic granularity is
*per IO operation*, not "kill the process". In turso every simulated
file can, under seeded control: fail a single `pread` or `pwrite`,
fail a `sync` (fsync — the syscall that forces buffered data to
disk), or delay any operation into a `DelayedIo` queue so it
completes later and out of order. A master switch
(`fault: Cell<bool>`, io.rs:14) arms injection; a selective variant
targets faults at one file stem only (the WAL but not the database
file, or vice versa).

This is exactly the fault model our `sim_fs.rs` copies
(buffered-until-sync + tear-on-crash), and it covers the crash
matrix from topic 5 — automatically, exhaustively, on demand:
torn writes, short reads, fsync failures the kernel will never give
you when you want them. Question: which topic 5 crash-matrix cell
does each of {pwrite fault, sync fault, delayed write + crash}
correspond to?

### Step 4 — the generator: interaction plans with properties woven in

A generator is the machine that produces inputs no human would write
by hand. turso's generator (`generation/`) emits an **interaction
plan**: a workload-distributed sequence of SQL statements
interleaved with **property** checks. A property here is a
metamorphic oracle (an oracle that doesn't know the right answer,
only a relationship two results must satisfy — topic README §2):

- `SelectSelectOptimizer` — two spellings of the same query must
  agree (TLP-shaped).
- `WhereTrueFalseNull` — the three-valued partition identity.
- `UnionAllPreservesCardinality` — row counts must add up.
- `ReadYourUpdatesBack` — a session guarantee (DDIA ch. 5 — same
  anomaly, single node).
- `FsyncNoWait` / `FaultyQuery` (property.rs:270) — fault-flavored
  properties that assert behavior *under* injection.
- `DoubleCreateFailure` — pins error-path behavior.

Note the generation trick: unrelated random queries are interleaved
WITHOUT breaking property invariants — coverage and oracles coexist
in one plan. That's the shape M16's Cypher properties need.

### Step 5 — doublecheck: determinism itself is the cheapest oracle

Run the identical plan twice from the same seed; the two outputs
must match byte-for-byte (`runner/doublecheck.rs`). This oracle
needs NO model of correct behavior — only the promise from Step 1.
Any divergence means nondeterminism leaked into the SUT: HashMap
iteration order, uninitialized memory, a hidden wall-clock read, a
stray thread. It costs one extra run and catches the class of bug
that silently invalidates every *other* seed-based result.
Question: what class of bug does doublecheck catch that the model
oracle misses? (Hint: iteration order, uninitialized memory, hidden
wall-clock reads.)

### Step 6 — shrinking and the bug base: from failure to reproducer to regression suite

A failing seed typically produces a plan with hundreds of
interactions, most irrelevant. **Shrinking** (the `shrink/` module)
minimizes it: repeatedly delete chunks of the plan and re-run,
keeping any smaller plan that still fails — delta debugging — until
what remains is a minimal reproducer. Shrinking stateful op
sequences is harder than shrinking pure inputs because later ops
depend on state earlier ops created (drop the `CREATE TABLE` and
every subsequent statement changes meaning).

Found bugs then persist in `runner/bugbase.rs` as seeds — the
regression suite is literally a list of u64s. Compare: our topic 15
sim tests hardcode seeds 42/7/11/13. That's a bug base, four entries
long.

## Where each step lives in the code

The tree, top to bottom:

```
 testing/simulator/
   main.rs          entry: seed → config → plan → execute → check
   runner/
     clock.rs       SimulatorClock — time is an RNG stream        (step 2)
     io.rs          SimulatorIO — fault injection switchboard     (step 3)
     file.rs        SimulatorFile — per-op faults + seeded latency (step 3)
     execution.rs   drive the plan, catch assertion failures      (step 4)
     doublecheck.rs run the same plan twice, diff outputs         (step 5)
     bugbase.rs     known-bug corpus (regression seeds)           (step 6)
   generation/      plan/property/query generators                (step 4)
   model/           the in-memory oracle + interaction model      (step 4)
   shrink/          plan minimization                             (step 6)
```

| anchor | step | what it is |
|---|---|---|
| runner/clock.rs:8-13 | 2 | `SimulatorClock { curr_time, rng: ChaCha8Rng, min_tick, max_tick }` |
| runner/clock.rs:25-34 | 2 | `now()` ADVANCES time by a seeded random tick — time is data |
| runner/io.rs:14 | 3 | `fault: Cell<bool>` — the injection master switch |
| runner/io.rs:64-77 | 3 | `inject_fault` / `inject_fault_selective` (per-file stem!) |
| runner/io.rs:135-138 | 3 | per-op fault counters: pread/pwrite/sync faults |
| runner/file.rs:40 | 3 | `latency_probability` — seeded IO delay |
| runner/file.rs:100-110 | 3 | `generate_latency_duration` — random_bool from the file's rng |
| runner/file.rs:149-233 | 3 | every op (read/write/sync) can be delayed into a `DelayedIo` queue |
| generation/property.rs:270 | 4 | `FsyncNoWait` / `FaultyQuery` — fault-flavored properties |
| generation/property.rs:276-282 | 4 | the metamorphic set: SelectSelectOptimizer, WhereTrueFalseNull, UnionAllPreservesCardinality, ReadYourUpdatesBack |
| fuzz/fuzz_targets/expression.rs:299 | — | `fuzz_target!(\|expr: Expr\|)` — STRUCTURED fuzzing via arbitrary (topic README §3, lives outside the simulator tree) |

Reading order: follow the anchor map top to bottom — clock, then IO
and file (the fault switchboard), then the properties, then
doublecheck and shrink.

## Questions for notes.md

1. ChaCha8 everywhere, not the default RNG — why does DST need a
   *portable, versioned* RNG? What breaks on rand upgrades?
2. `inject_fault_selective` targets file stems (WAL vs db file) —
   which bug class needs faults on ONE file only?
3. Where does turso's simulator sit vs Antithesis (whole-VM
   determinism)? What can each test that the other can't?
4. The shrink/ module: why is shrinking HARDER for stateful op
   sequences than for pure inputs (proptest's integrated shrinking
   vs delta debugging)?
5. For M16: which three properties from generation/property.rs port
   directly to Cypher? Sketch the graph equivalents.

## References

**Code**
- [turso](https://github.com/tursodatabase/turso) —
  `testing/simulator/` (clock/io/file fault injection, interaction
  plans, properties, doublecheck, shrink) plus
  `fuzz/fuzz_targets/expression.rs` for structured fuzzing via
  `arbitrary` — clone it; the anchor map above is your reading order
