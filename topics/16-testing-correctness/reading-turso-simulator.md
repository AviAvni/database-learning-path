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

Every anchor below is turso at commit **`dd775bc`**, the revision
this repo pins (`resources/codebases.md`), quoted with the line
numbers the code occupies at that commit. Several of them contradict
what the simulator's own documentation implies; where they do, this
chapter says so and shows the line.

## The problem in one sentence

A crash-recovery bug that needs one specific interleaving of writes,
fsyncs, and a crash might show up once in a million runs — and when
it does, a conventional test can't replay it; DST makes every such
failure a single u64 you can re-run forever.

## The concepts, step by step

### Step 1 — determinism: make the program a pure function of a seed

> **In:** a program that reads the wall clock, spawns threads, and
> calls `pwrite`/`fsync` against a real kernel.
> **Out:** the same program, where every one of those reads is
> served by one seeded RNG — so the whole run is a pure function of
> a `u64`.

**Deterministic simulation testing (DST)** is the discipline of
removing every source of randomness the program doesn't control —
wall-clock time, thread scheduling, IO timing, OS errors — and
replacing each with values drawn from one seeded **pseudo-random
number generator** (an algorithm that turns one starting number, the
**seed**, into an endless reproducible stream of "random" numbers).
The **system under test** (SUT — the code being tested) touches the
outside world only through interfaces, and in test those interfaces
are backed by the RNG:

```
 real:  code → syscalls → kernel  (time, threads, fsync — nondeterministic)
 DST:   code → traits ──→ SimulatorClock (ChaCha8 from seed)
                     ├──→ SimulatorFile  (buffered; crash DROPS unsynced,
                     │                    may TEAR the last write)
                     └──→ SimNet         (topic 15's sim.rs already did this)
        ⇒ failure = a u64 seed. Re-run seed = same bug, every time.
```

The payoff is the whole game: run seed 0, 1, 2, … overnight; any
assertion failure prints its seed; re-running that seed reproduces
the exact same interleaving, faults and all. Debugging a
one-in-a-million bug becomes ordinary single-run debugging. The cost
is architectural: the SUT must own NO nondeterminism, which is why
turso routes all IO and time through traits (**dependency
injection**: the caller supplies the implementation, so the test can
supply a different one).

The boundary is never quite total, and turso's is a good example of
a partial one. `impl Clock for SimulatorIO`
(`testing/simulator/runner/io.rs:114-122`) routes
`current_time_wall_clock` through the simulated clock but returns
`MonotonicInstant::now()` — the *real* monotonic clock — from
`current_time_monotonic`. And `SimulatorClock::new` seeds its
starting instant from the real `Utc::now()` (`clock.rs:18`), so
absolute timestamps differ between runs of the same seed; only the
*deltas* are deterministic. Anything in the SUT that branches on an
absolute wall-clock value, or on monotonic elapsed time, escapes the
seed.

Why it matters: the seed is only worth as much as the boundary is
tight, and the boundary is a property you can read off the code —
count the escapes before you trust a reproducer.

### Step 2 — simulated time: every `now()` is a seeded random jump

> **In:** a `SimulatorClock` holding the current instant, a
> `ChaCha8Rng`, and a `[min_tick, max_tick)` range.
> **Out:** a `DateTime<Utc>` that is strictly greater than the last
> one returned, by a seeded random amount.

Once the clock is behind a trait, "time" is just data the simulator
makes up. turso's `SimulatorClock` advances the current time by a
random tick on *every read* — no wall clock exists anywhere after
construction:

```rust
// testing/simulator/runner/clock.rs — the whole clock, 7-13 and 25-34
     7  #[derive(Debug)]
     8  pub struct SimulatorClock {
     9      curr_time: RefCell<DateTime<Utc>>,
    10      rng: RefCell<ChaCha8Rng>,
    11      min_tick: u64,
    12      max_tick: u64,
    13  }
// ... 15-23: new() seeds curr_time from the REAL Utc::now() (line 18) ...
    25      pub fn now(&self) -> DateTime<Utc> {
    26          let mut time = self.curr_time.borrow_mut();
    27          let nanos = self
    28              .rng
    29              .borrow_mut()
    30              .random_range(self.min_tick..self.max_tick);
    31          let nanos = std::time::Duration::from_micros(nanos);
    32          *time += nanos;
    33          *time
    34      }
```

The file is 35 lines long; that is the entire clock. Three design
points hide in it.

First, **ChaCha8** — a specific, versioned RNG — not `rand`'s
default: the default algorithm is allowed to change between crate
releases, which would silently change what every archived seed
means.

Second, `now()` must ADVANCE rather than return a fixed value. Any
loop of the form "retry until deadline" polls the clock, and if time
never moves, the simulation livelocks. Note the consequence: reading
the clock is not free of side effects, and it *consumes a draw from
the RNG stream*. Add a `tracing` call that reads the clock and every
subsequent random decision in the run shifts — the seed still
reproduces, but it reproduces a different execution.

Third, line 31 is worth staring at. The variable is called `nanos`;
the constructor is `Duration::from_micros`. The unit is
**microseconds**. Combined with the profile defaults in
`testing/simulator/profiles/io.rs:45-53` — `min_tick: 1`,
`max_tick: 30` — that fixes the scale of simulated time:

```
 tick range          [1, 30) µs        profiles/io.rs:45-53
 mean tick           (1 + 29) / 2 = 15 µs
 1,000 now() calls   1,000 × 15 µs   = 15 ms of simulated time
 1,000,000 calls     1e6 × 15 µs     = 15 s of simulated time
```

So a run that touches the clock a million times has "aged" fifteen
seconds — while costing the CPU only the work of a million integer
draws. That ratio is the reason DST is cheap: simulated time is
bought at the price of arithmetic, not of sleeping.

Why it matters: time is the single most common leak in a
determinism boundary, and it is also the cheapest thing to make up.

### Step 3 — fault injection at the file layer, per operation

> **In:** a `SimulatorFile` wrapping a real file, plus a `fault`
> flag and a `latency_probability`.
> **Out:** each `pread` / `pwrite` / `sync` / `pwritev` / `truncate`
> either succeeds now, fails with an injected error, or is pushed
> onto a queue to complete later.

**Fault injection** means deliberately making an operation fail the
way hardware and kernels really fail — and the realistic granularity
is *per IO operation*, not "kill the process". Two independent
mechanisms sit on every simulated file.

The first is a fault flag. `pub(crate) fault: Cell<bool>`
(`runner/io.rs:14`) is the master switch;
`inject_fault` / `inject_fault_selective` (`runner/io.rs:64-80`) set
it, the selective variant matching on a **file stem** so faults can
be aimed at the WAL but not the database file, or the reverse. The
per-op counters that record what fired are declared on the file, not
the IO layer: `runner/file.rs:19-34` holds six of them
(`nr_pread_faults`, `nr_pwrite_faults`, `nr_sync_faults`, and the
matching call counters).

The second is latency. Every op consults
`generate_latency_duration`, and on a hit is deferred into a
`DelayedIo` queue so it completes later and out of order:

```rust
// testing/simulator/runner/file.rs — generate_latency_duration, 99-109
    99      #[instrument(skip_all, level = Level::TRACE)]
   100      fn generate_latency_duration(&self) -> Option<turso_core::WallClockInstant> {
   101          let mut rng = self.rng.borrow_mut();
   102          // Chance to introduce some latency
   103          rng.random_bool(self.latency_probability as f64 / 100.0)
   104              .then(|| {
   105                  let now = self.clock.now();
   106                  let sum = now + std::time::Duration::from_millis(rng.random_range(5..20));
   107                  sum.into()
   108              })
   109          }
```

Line 103 is the load-bearing one: `latency_probability` is declared
`pub latency_probability: u8` (`file.rs:40`) and divided by 100, so
it is a **percent**, not a per-mille or a float. The profile default
is `latency_probability: 1` (`profiles/io.rs:45-53`) — one percent.
Line 106 sets the delay itself: uniform in `[5, 20)` milliseconds.
Do the arithmetic before you tune anything:

```
 P(delay) per op        1%              profiles/io.rs:45-53 + file.rs:103
 delay when it fires    U[5, 20) ms     file.rs:106, mean 12.5 ms
 mean delay per op      0.01 × 12.5 ms  = 125 µs
 vs. a mean clock tick  15 µs           (Step 2)

 ⇒ one injected delay ≈ 12.5 ms / 15 µs ≈ 833 clock ticks of
   simulated time. A single 1%-probability delay reorders the
   IO queue by roughly a thousand ticks' worth of other work.
```

The delay path is duplicated per operation rather than factored:
pread at `file.rs:149-158`, pwrite at `175-184`, sync at `200-215`,
pwritev at `233-244`, truncate at `257-266`.

Now the correction. **Sync faults do not fire at this revision.** A
previous version of this chapter said turso can "fail a `sync`
(fsync)". It cannot:

```rust
// testing/simulator/runner/file.rs — sync(), 192-199
   192          self.nr_sync_calls.set(self.nr_sync_calls.get() + 1);
   193          if self.fault.get() {
   194              // TODO: Enable this when https://github.com/tursodatabase/turso/issues/2091 is fixed.
   195              tracing::debug!(
   196                  "ignoring sync fault because it causes false positives with current simulator design"
   197              );
   198              self.fault.set(false);
   199          }
```

The armed fault is swallowed *and cleared* (line 198), so it does
not even survive to the next operation. `nr_sync_faults` is declared
at `file.rs:34` and never incremented; the stats table hard-codes a
zero for it, with the comment `// No fault counter for sync`
(`file.rs:87-91`). The profile still defaults `sync: true` in its
fault-enable set (`profiles/io.rs:70-78`), which is exactly why this
is worth knowing: the configuration says the fault is on and the
code says it is off.

Why it matters: the fault your harness *reports* injecting and the
fault it *actually* injects are two different facts, and topic 16's
own baseline is the argument for checking. `NoSyncOnCommit` is the
easiest planted bug in `crash_matrix` to catch — 99.6% of seeds
find it — precisely because sync behaviour is where crash-recovery
bugs live. A harness that silently declines to perturb `sync` is
declining to look in the richest place.

### Step 4 — the generator: interaction plans with properties woven in

> **In:** a seed and a workload distribution.
> **Out:** an **interaction plan** — a sequence of SQL statements
> interleaved with property checks, each property carrying its own
> assertion.

A **generator** is the machine that produces inputs no human would
write by hand. turso's (`testing/simulator/generation/`) emits a
plan of statements interleaved with **properties**. A property here
is a **metamorphic oracle**: an oracle that doesn't know the right
answer, only a relationship two results must satisfy (topic README
§2).

The `Property` enum (`testing/simulator/model/property.rs:11-212`)
has sixteen variants. The ones worth naming, with the line each is
declared on:

| variant | line | what it asserts |
|---|---|---|
| `InsertValuesSelect` | 27 | inserted rows come back |
| `ReadYourUpdatesBack` | 49 | UPDATE success *and* failure |
| `TableHasExpectedContent` | 61 | model vs engine, one table |
| `DoubleCreateFailure` | 87 | the error path is pinned |
| `SelectLimit` | 100 | LIMIT n returns ≤ n |
| `DeleteSelect` | 120 | deleted rows are gone |
| `DropSelect` | 137 | dropped table is gone |
| `SelectSelectOptimizer` | 149 | NoREC — see below |
| `WhereTrueFalseNull` | 157 | TLP — see below |
| `UnionAllPreservesCardinality` | 167 | counts add up |
| `FsyncNoWait` | 179 | behaviour under fault |
| `FaultyQuery` | 182 | behaviour under fault |
| `SavepointRollback` | 189 | nested rollback |
| `SequenceMonotonicity` | 203 | sequences never go back |

Two of those need correcting, and the corrections come from the
doc comments the code itself carries.

`SelectSelectOptimizer` is **NoREC, not TLP.** Its doc
(`model/property.rs:142-148`) names the paper: "As highlighted by
Rigger et al. in Non-Optimizing Reference Engine
Construction(NoREC), SQLite tends to optimize `where` statements
while keeping the result column expressions unoptimized." It runs
`SELECT <predicate> FROM <t>` against `SELECT * FROM <t> WHERE
<predicate>` and — per the same doc — "is successful if the two
queries return the same number of rows". Cardinality only. That is
the NoREC oracle exactly (see
[reading-pqs-tlp-papers.md](reading-pqs-tlp-papers.md) Step 6).

`WhereTrueFalseNull` is the TLP one, and its doc says so
(`model/property.rs:153-160`): "canonically called Ternary Logic
Partitioning (TLP)".

`ReadYourUpdatesBack` is **not** a session guarantee. Its doc
(`model/property.rs:39-53`) spells out both arms: on UPDATE success
the after-rows carry the new values; on UPDATE failure
`select_before == select_after`. The second arm is a rollback
check — an atomicity property, not a read-your-writes one.

Why it matters: a property's *name* is a hypothesis about what it
tests; its doc comment and its assertion are the facts. Two of
sixteen names here mislead.

### Step 5 — the assertion is weaker than the identity it names

> **In:** two result sets — the original query's and the recombined
> partition's.
> **Out:** a pass/fail — but under a comparison that is *not*
> multiset equality.

TLP's published identity is `RS(Q) = RS(Q_p) ⊎ RS(Q_¬p) ⊎
RS(Q_p IS NULL)` where `⊎` is **multiset addition** (Rigger & Su,
OOPSLA 2020, Table 1, WHERE row). turso builds the three partitions
faithfully:

```rust
// testing/simulator/generation/property.rs — the three partitions, 1073-1083
  1073                  let old_predicate = select.body.select.where_clause.clone();
  1074
  1075                  let p_true = Predicate::and(vec![old_predicate.clone(), predicate.clone()]);
  1076                  let p_false = Predicate::and(vec![
  1077                      old_predicate.clone(),
  1078                      Predicate::not(predicate.clone()),
  1079                  ]);
  1080                  let p_null = Predicate::and(vec![
  1081                      old_predicate,
  1082                      Predicate::is(predicate.clone(), Predicate::null()),
  1083                  ]);
```

— and stitches them with `UNION ALL` (`generation/property.rs:1094-1115`).
But the check that follows is not `⊎`:

```rust
// testing/simulator/generation/property.rs — the assertion, 1138 and 1146-1147, 1162-1163
  1138                                  if select_rows.len() != select_tlp_rows.len() {
// ... 1139-1144: report a row-count mismatch ...
  1145                                  // Check if any row in select_rows is not in select_tlp_rows
  1146                                  for row in select_rows.iter() {
  1147                                      if !select_tlp_rows.iter().any(|r| r == row) {
// ... 1148-1160: report "in select but not in select_tlp" ...
  1161                                  // Check if any row in select_tlp_rows is not in select_rows
  1162                                  for row in select_tlp_rows.iter() {
  1163                                      if !select_rows.iter().any(|r| r == row) {
```

That is **equal cardinality plus mutual set containment** — which is
strictly weaker than multiset equality. Work an example:

```
 whole  = {a, a, b}        len 3, set {a, b}
 parts  = {a, b, b}        len 3, set {a, b}

 line 1138  3 == 3                            → pass
 line 1146  every row of whole appears in parts → pass
 line 1162  every row of parts appears in whole → pass
 verdict:   PASS — but a is duplicated on one side and b on the other.

 multiset equality would require count(a) = 2 on both sides. It does not.
```

A duplicate-multiplicity bug — a join that emits a row twice, a
`UNION ALL` that drops one copy of a duplicate — passes this
property. That is a real gap against the published oracle, and it is
not turso-specific: SQLancer's own comparator has the same shape
(`src/sqlancer/ComparatorHelper.java:91` size, then `:108-112`
`HashSet` equality — see
[reading-sqlancer.md](reading-sqlancer.md) Step 6).

Why it matters: "we implemented TLP" is a claim about the queries;
whether you implemented the *oracle* is a claim about the
comparison. Read the comparison.

### Step 6 — doublecheck: determinism itself is the cheapest oracle

> **In:** one plan and one seed.
> **Out:** two `SimulatorEnv`s stepped in lockstep, compared
> per-interaction and then file-to-file.

This oracle needs NO model of correct behaviour — only the promise
from Step 1. But it is not "run the plan twice and diff stdout". The
mechanism (`testing/simulator/runner/doublecheck.rs`) is:

- Two environments, both turso. `main.rs:484-491` builds the second
  with `env.clone_as(SimulationType::Default)` — this is *not* a
  differential test against SQLite.
- They are advanced **in lockstep, interaction by interaction, in
  one process** (`doublecheck.rs:104-176`), not run to completion
  and diffed at the end.
- Each interaction's result values are compared as they are produced
  (`compare_results`, `doublecheck.rs:178-201`; the mismatch report
  is at line 193).
- At the end, the two on-disk database **files** are compared
  byte-for-byte (`doublecheck.rs:56-73`).

Lockstep is the design decision that matters: it localises the
divergence to the interaction that caused it rather than to the
whole run. Any divergence means nondeterminism leaked into the SUT —
`HashMap` iteration order, uninitialised memory, a hidden wall-clock
read (Step 1 listed two that are still open by construction), a
stray thread. It costs one extra run and catches the class of bug
that silently invalidates every *other* seed-based result.

Why it matters: every claim in this topic's `notes.md` baseline is
"same harness, different seed". If the harness isn't deterministic,
the whole table means nothing — so the cheapest oracle is the one
that checks the assumption the others rest on.

### Step 7 — shrinking: greedy, linear, and honest about it

> **In:** a failing plan of hundreds of interactions and the error
> string it produced.
> **Out:** a smaller plan that produces *the same error string*.

A failing seed typically produces a plan where most interactions are
irrelevant. **Shrinking** minimises it. The previous version of this
chapter called turso's shrinker delta debugging; it is not, and the
code says so in its own comment.

Phase 1, `shrink_interaction_plan` (`shrink/plan.rs:24-100`), is
purely static: truncate everything after the failing interaction,
then drop properties that don't touch the tables the failing
interaction depends on. **No re-runs at all.** Its comment at line
25 reads "this is a very naive implementation".

Phase 2, `brute_shrink_interaction_plan` (`shrink/plan.rs:103-143`)
driving `iterative_shrink` (`146-173`), removes **one whole property
at a time, in reverse order**, re-runs, and keeps the removal only
if the shrunk plan reproduces the same failure. The equality that
defines "same failure" is a string compare on the error
(`test_shrunk_plan`, `shrink/plan.rs:175-201`; the test is
`e1 == e2` at line 198).

The cost model follows directly, and it is not ddmin's:

```
 n properties in the truncated plan
 ddmin (Zeller & Hildebrandt): partition, halve, ~O(n log n) re-runs,
                               shrinks toward a 1-minimal subset
 turso phase 2:                one linear reverse pass, exactly n re-runs,
                               each keeping or discarding one property

 n = 200 properties → 200 re-runs, one pass. No second pass, so a
 removal that only becomes possible after an earlier removal is
 never found.
```

Shrinking stateful op sequences is harder than shrinking pure inputs
because later ops depend on state earlier ops created — drop the
`CREATE TABLE` and every subsequent statement changes meaning, which
is exactly why phase 1 reasons about table dependencies before phase
2 starts deleting. The string-equality criterion at line 198 is the
other honest limitation: a shrink that changes the error *message*
while preserving the bug is rejected.

Why it matters: shrinking quality is the difference between a
reproducer a human will read and one they won't. Knowing it's a
single greedy pass tells you when to shrink again by hand.

### Step 8 — the bug base: a directory per seed, not a list of seeds

> **In:** a seed that failed, plus the CLI options that produced it.
> **Out:** a directory under `.bugbase` holding the plan, the shrunk
> plan, and the run history.

The previous version of this chapter said "the regression suite is
literally a list of u64s". It isn't. `runner/bugbase.rs` locates a
`.bugbase` directory — searching the limbo project dir, then the
home dir, then the cwd (`bugbase.rs:132-158`) — and writes **one
directory per seed** containing `seed.txt`, `plan.sql`,
`shrunk.sql`, and `runs.json`. Each `BugRun` record
(`bugbase.rs:41-54`) carries the turso **commit hash**, a timestamp,
the error, the CLI options, and a `shrunk` flag.

The commit hash is the interesting field: a seed alone does not
reproduce a bug, because the meaning of a seed changes whenever the
generator changes. Recording `(seed, commit, options)` is the
minimum tuple that reproduces. Our topic 15 sim tests hardcode seeds
42/7/11/13 — that is a bug base with one of the three fields, and
the `.bugbase` layout is what the other two look like.

Why it matters: "we saved the seed" is a weaker claim than it
sounds. The seed indexes into a random stream whose *shape* is part
of the program.

### Step 9 — outside the simulator: fuzzing and elle

> **In:** the same repo, two sibling test harnesses.
> **Out:** an oracle that isn't turso-vs-turso, and a history format
> that isn't turso's at all.

Two things live outside `testing/simulator/` and change what the
tree can find.

`fuzz/fuzz_targets/expression.rs` (299 lines) is a **differential**
target, not a "doesn't crash" one. `do_fuzz` (lines 248-297)
evaluates the generated expression in in-memory SQLite (257-264) and
in turso (266-287) and `assert_eq!`s the two (289-294); expressions
deeper than 100 are rejected from the corpus (252-255). The
`fuzz_target!` macro invocation is the last line of the file, 299.
Sibling targets: `cast_real.rs`, `scalar_func.rs`, `schema.rs`.

The corresponding restriction inside the simulator is worth knowing:
`Differential` mode disables fault injection outright
(`runner/env.rs:1341-1359` sets `profile.io.enable = false` with the
comment that faults can't be controlled on rusqlite), and also turns
off LIMIT and CREATE SEQUENCE generation. So in this tree, *faults
and a second implementation are mutually exclusive*.

`testing/concurrent-simulator/elle.rs` (317 lines) emits histories
in **elle's list-append EDN format** for `elle-cli` to check: the
module doc (lines 1-7) names G0/G1/G2/G-Single from Adya's
formalism, `ElleOp` (18-30) is Append/Read/Write/RwRead, `to_edn`
(36-67) produces `[:append "key" v]` and `[:r "key" [1 2 3]]`, and
`ElleEventType` (72-79) is Invoke/Ok/Fail/Info. There is a
`.github/workflows/elle.yml` to run it. That is the exact format
Figure 2 of the Elle paper prints — see
[reading-jepsen.md](reading-jepsen.md) Step 4.

And Antithesis is wired in for real: `Dockerfile.antithesis`,
`.github/workflows/antithesis.yml` (default **240-minute**
experiments, with an optional `diff_base` for targeted testing),
`scripts/antithesis/diff_to_targeted_coverage.py`, and workloads
under `testing/antithesis/` (`bank-test/`, `stress-composer/`, with
`anytime_validate.py` / `eventually_validate.py` /
`finally_validate.py`).

Why it matters: the simulator is one of four harnesses in this repo,
and they cover different bug classes on purpose. Reading only
`testing/simulator/` will make you think turso has no ground-truth
oracle; it has two, they just live elsewhere.

## Where each step lives in the code

The tree, top to bottom:

```
 testing/simulator/
   main.rs          entry: seed → config → plan → execute → check
   profiles/
     io.rs          latency/tick/fault defaults                   (steps 2-3)
   runner/
     clock.rs       SimulatorClock — time is an RNG stream        (step 2)
     io.rs          SimulatorIO — fault injection switchboard     (step 3)
     file.rs        SimulatorFile — per-op faults + seeded latency (step 3)
     env.rs         SimulatorEnv — profiles, Differential mode    (step 9)
     doublecheck.rs two envs stepped in lockstep, then file diff  (step 6)
     bugbase.rs     .bugbase — a directory per failing seed       (step 8)
   generation/      plan/property/query generators                (steps 4-5)
   model/           the in-memory oracle + Property enum          (step 4)
   shrink/          plan minimization                             (step 7)
 fuzz/fuzz_targets/ differential fuzzing vs rusqlite              (step 9)
 testing/concurrent-simulator/elle.rs  EDN histories for elle-cli (step 9)
```

| anchor | step | what it is |
|---|---|---|
| `runner/clock.rs:8-13` | 2 | `SimulatorClock { curr_time, rng: ChaCha8Rng, min_tick, max_tick }` |
| `runner/clock.rs:18` | 1 | `curr_time` seeded from the **real** `Utc::now()` |
| `runner/clock.rs:25-34` | 2 | `now()` advances by a seeded tick; line 31 is `from_micros` |
| `profiles/io.rs:45-53` | 2-3 | defaults: `latency_probability: 1`, `min_tick: 1`, `max_tick: 30` |
| `profiles/io.rs:70-78` | 3 | fault-enable defaults: read/write/sync all `true` |
| `runner/io.rs:14` | 3 | `pub(crate) fault: Cell<bool>` — the injection master switch |
| `runner/io.rs:64-80` | 3 | `inject_fault` / `inject_fault_selective` (per-file stem) |
| `runner/io.rs:114-122` | 1 | `impl Clock` — wall clock simulated, **monotonic clock real** |
| `runner/file.rs:19-34` | 3 | the six call/fault counters; `nr_sync_faults` at :34 |
| `runner/file.rs:40` | 3 | `pub latency_probability: u8` — a **percent** |
| `runner/file.rs:87-91` | 3 | `stats_table` prints a hard-coded `0` for sync faults |
| `runner/file.rs:99-109` | 3 | `generate_latency_duration` — `/100.0` at :103, `5..20` ms at :106 |
| `runner/file.rs:149-268` | 3 | the per-op delay blocks (pread 149, pwrite 175, sync 200, pwritev 233, truncate 257) |
| `runner/file.rs:192-199` | 3 | **sync faults are swallowed and cleared** at this revision |
| `runner/env.rs:1341-1359` | 9 | `Differential` mode disables fault injection |
| `model/property.rs:11-212` | 4 | the `Property` enum — 16 variants |
| `model/property.rs:39-53` | 4 | `ReadYourUpdatesBack` — success *and* rollback arms |
| `model/property.rs:142-148` | 4 | `SelectSelectOptimizer` — doc cites **NoREC** |
| `model/property.rs:153-160` | 4 | `WhereTrueFalseNull` — doc cites **TLP** |
| `generation/property.rs:1073-1083` | 5 | the three TLP partitions built as predicates |
| `generation/property.rs:1094-1115` | 5 | stitched with `UNION ALL` |
| `generation/property.rs:1138`, `1146-1177` | 5 | the assertion: size, then set containment both ways |
| `runner/doublecheck.rs:56-73` | 6 | final byte-for-byte database file comparison |
| `runner/doublecheck.rs:104-176` | 6 | the lockstep interaction loop |
| `runner/doublecheck.rs:178-201` | 6 | `compare_results`; mismatch reported at :193 |
| `main.rs:484-491` | 6 | `env.clone_as(SimulationType::Default)` — both sides are turso |
| `shrink/plan.rs:24-100` | 7 | phase 1: truncate + drop by table dependency, no re-runs |
| `shrink/plan.rs:103-173` | 7 | phase 2: one property at a time, reverse order, re-run each |
| `shrink/plan.rs:175-201` | 7 | `test_shrunk_plan` — "same failure" is `e1 == e2` at :198 |
| `runner/bugbase.rs:41-54` | 8 | `BugRun` — commit hash, timestamp, error, options, `shrunk` |
| `runner/bugbase.rs:132-158` | 8 | `.bugbase` directory discovery |
| `fuzz/fuzz_targets/expression.rs:248-297` | 9 | differential against in-memory SQLite |
| `fuzz/fuzz_targets/expression.rs:299` | 9 | `fuzz_target!(\|expr: Expr\| -> Corpus {...})` |
| `testing/concurrent-simulator/elle.rs:36-67` | 9 | `to_edn` — elle's list-append history format |

Reading order: follow the anchor map top to bottom — clock, then IO
and file (the fault switchboard), then the properties and the TLP
assertion, then doublecheck, shrink, and the bug base.

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
5. For M16: which three properties from `model/property.rs` port
   directly to Cypher? Sketch the graph equivalents.

## Done when

Answer each before unfolding it.

- [ ] You can explain what it takes to make a program a pure function of a seed, and name the three sources of nondeterminism that must be captured — plus the two turso still leaks.

  <details><summary>Answer</summary>

  Time, IO (results *and* timing), and scheduling. Each must be reached
  only through a trait whose test implementation draws from one seeded
  RNG (Step 1). turso does this for the wall clock (`clock.rs`) and for
  files (`file.rs`).

  The two leaks are both visible in the code. `impl Clock for
  SimulatorIO` (`runner/io.rs:114-122`) returns the real
  `MonotonicInstant::now()` from `current_time_monotonic` — only
  `current_time_wall_clock` is simulated. And `SimulatorClock::new`
  seeds `curr_time` from the real `Utc::now()` (`clock.rs:18`), so
  absolute timestamps differ run to run; only deltas reproduce.

  </details>

- [ ] You can say why ChaCha8 rather than the default RNG, and why reading the clock is not a side-effect-free operation.

  <details><summary>Answer</summary>

  `rand`'s default generator is explicitly allowed to change between
  releases. If it did, every archived seed in `.bugbase` would start
  meaning a different execution — the regression corpus would silently
  evaporate. ChaCha8 is named and versioned (`clock.rs:5, 10`), so a
  seed keeps its meaning.

  `now()` (`clock.rs:25-34`) mutates `curr_time` *and* consumes a draw
  from the RNG (line 30). So adding a clock read anywhere — even inside
  a log line — shifts every subsequent random decision in the run. The
  seed still reproduces, but it reproduces a different execution than
  it did yesterday.

  </details>

- [ ] You can describe fault injection at the file layer, quantify the latency injection, and say which fault does *not* fire at `dd775bc`.

  <details><summary>Answer</summary>

  Two mechanisms. A `fault: Cell<bool>` flag (`io.rs:14`) armed by
  `inject_fault` / `inject_fault_selective` (`io.rs:64-80`), the latter
  matching a file stem so the WAL can be faulted independently of the
  database file. And a latency path: `generate_latency_duration`
  (`file.rs:99-109`) fires with `latency_probability / 100.0` — the
  default of `1` (`profiles/io.rs:45-53`) is therefore **1%** — and on
  a hit defers the operation by `U[5, 20)` ms (line 106) into the
  `DelayedIo` queue.

  Mean delay per operation is `0.01 × 12.5 ms = 125 µs`, against a mean
  simulated clock tick of 15 µs — so one injected delay is worth about
  830 ticks of reordering.

  **`sync` faults do not fire.** `file.rs:192-199` checks the flag,
  logs "ignoring sync fault because it causes false positives with
  current simulator design", and clears it. `nr_sync_faults`
  (`file.rs:34`) is never incremented and `stats_table` hard-codes a
  zero (`file.rs:87-91`) — even though `profiles/io.rs:70-78` defaults
  `sync: true`.

  </details>

- [ ] You can explain the doublecheck oracle, and say precisely what it compares and when.

  <details><summary>Answer</summary>

  It runs two `SimulatorEnv`s — both turso, built by
  `env.clone_as(SimulationType::Default)` at `main.rs:484-491` — in
  **lockstep, interaction by interaction, inside one process**
  (`doublecheck.rs:104-176`). Result values are compared as each
  interaction completes (`compare_results`, `:178-201`, mismatch at
  `:193`), and at the end the two database *files* are compared
  byte-for-byte (`:56-73`).

  It needs no model of correctness — only the determinism promise from
  Step 1 — and it catches the class of bug (`HashMap` order, hidden
  clock reads, uninitialised memory, stray threads) that silently
  invalidates every *other* seed-based result. Lockstep is the point:
  it localises divergence to the interaction that caused it.

  </details>

- [ ] You can say why turso's TLP property is weaker than the published TLP oracle, and construct an input that slips through.

  <details><summary>Answer</summary>

  The paper's identity uses `⊎`, multiset addition (Rigger & Su,
  OOPSLA 2020, Table 1, WHERE row). turso builds the partitions
  correctly (`generation/property.rs:1073-1083`, `UNION ALL` at
  `1094-1115`) but then checks **equal length** (`:1138`) plus **set
  containment in both directions** (`:1146-1177`).

  `{a, a, b}` versus `{a, b, b}`: both have length 3, both have set
  `{a, b}`, every element of each appears in the other. It passes. A
  duplicate-multiplicity bug is invisible.

  SQLancer's comparator has the same shape — size at
  `ComparatorHelper.java:91`, then `HashSet` equality at `:108-112`.
  Two independent implementations of the same paper, the same gap.

  </details>

- [ ] You can say why shrinking stateful op sequences is harder than shrinking values, and describe turso's actual algorithm and its cost.

  <details><summary>Answer</summary>

  Later ops depend on state earlier ops created: delete the
  `CREATE TABLE` and every following statement changes meaning, so you
  cannot treat the plan as an unordered bag of independent elements.

  turso's shrinker is *not* delta debugging. Phase 1
  (`shrink/plan.rs:24-100`, comment at :25 calling itself "very
  naive") truncates after the failing interaction and statically drops
  properties that don't touch the depended-on tables, with **no
  re-runs**. Phase 2 (`:103-173`) removes one whole property at a time
  in reverse order, re-running each time, and keeps a removal only if
  the error string still matches exactly (`e1 == e2`, `:198`).

  Cost: exactly `n` re-runs for `n` properties, in a single pass —
  where ddmin would do ~O(n log n) and reach a 1-minimal subset. A
  removal that only becomes possible after an earlier removal is never
  found, and a shrink that changes the error *message* is rejected
  even if it preserves the bug.

  </details>

- [ ] You can describe what `.bugbase` actually stores, and say why a seed alone is not a reproducer.

  <details><summary>Answer</summary>

  One **directory per failing seed** (`runner/bugbase.rs:132-158`
  finds `.bugbase` under the project dir, then home, then cwd),
  holding `seed.txt`, `plan.sql`, `shrunk.sql`, and `runs.json`. Each
  `BugRun` (`:41-54`) records the turso **commit hash**, a timestamp,
  the error, the CLI options, and whether the plan was shrunk.

  A seed indexes into a random stream whose *shape* is defined by the
  generator. Change the generator — add a property, reorder a match
  arm, add a clock read — and the same seed produces a different plan.
  The minimum reproducing tuple is `(seed, commit, options)`, which is
  exactly what `BugRun` stores.

  </details>

- [ ] You wrote answers to all five questions in notes.md, including the three properties you will port.

  <details><summary>Answer</summary>

  No unfoldable answer — this one is the writing. The `Property` table
  in Step 4 is the menu; the ones that port to a graph engine without
  a SQL optimizer to lean on are the state-based ones
  (`TableHasExpectedContent`, `DeleteSelect`, `DropSelect`,
  `SavepointRollback`), while `WhereTrueFalseNull` needs a graph
  answer to "what is NULL here" — a missing property on a node, per
  [reading-sqlancer.md](reading-sqlancer.md) question 5.

  </details>

## References

**Code**
- [turso](https://github.com/tursodatabase/turso) @ `dd775bc` —
  `testing/simulator/` (clock/io/file fault injection, interaction
  plans, properties, doublecheck, shrink), `fuzz/fuzz_targets/` for
  differential fuzzing against rusqlite, and
  `testing/concurrent-simulator/elle.rs` for elle-format histories

| File | Lines | What |
|---|---|---|
| `testing/simulator/runner/clock.rs` | 8-13, 18, 25-34 | the entire simulated clock (35 lines) |
| `testing/simulator/profiles/io.rs` | 45-53, 70-78 | tick range, latency probability, fault-enable defaults |
| `testing/simulator/runner/io.rs` | 14, 64-80, 114-122 | fault switch, injection, the partial `Clock` impl |
| `testing/simulator/runner/file.rs` | 19-34, 40, 87-91, 99-109, 149-268, 192-199 | counters, latency, per-op delay, the disabled sync fault |
| `testing/simulator/model/property.rs` | 11-212 | the 16-variant `Property` enum with its doc comments |
| `testing/simulator/generation/property.rs` | 1073-1083, 1094-1115, 1138-1177 | TLP partition construction and its weaker-than-`⊎` assertion |
| `testing/simulator/runner/doublecheck.rs` | 56-73, 104-176, 178-201 | file diff, lockstep loop, per-interaction compare |
| `testing/simulator/shrink/plan.rs` | 24-100, 103-173, 175-201 | two-phase greedy shrinker |
| `testing/simulator/runner/bugbase.rs` | 41-54, 132-158 | `BugRun` records and `.bugbase` layout |
| `testing/simulator/runner/env.rs` | 1341-1359 | `Differential` mode disables faults |
| `fuzz/fuzz_targets/expression.rs` | 248-297, 299 | differential fuzz target vs in-memory SQLite |
| `testing/concurrent-simulator/elle.rs` | 1-7, 18-30, 36-67, 72-79 | elle list-append EDN histories |

**Papers**
- Rigger & Su — "Finding Bugs in Database Systems via Query
  Partitioning" (OOPSLA 2020) — Table 1 is the identity turso's
  `WhereTrueFalseNull` implements; see
  [reading-pqs-tlp-papers.md](reading-pqs-tlp-papers.md)
- Kingsbury & Alvaro — "Elle: Inferring Isolation Anomalies from
  Experimental Observations" (VLDB 2020) — Figure 2 is the format
  `elle.rs` emits; see [reading-jepsen.md](reading-jepsen.md)
