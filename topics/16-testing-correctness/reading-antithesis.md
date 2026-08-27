# Antithesis: assertions as a search signal, and a simulation that branches

Antithesis appears in this topic's opening paragraph and then never
again, which is unsatisfying, because it is the one system here whose
core is not readable: the deterministic hypervisor is a commercial
product and there is no source to open. This chapter is about what you
*can* read — the open-source SDK your program links against — and what
that interface proves about the platform behind it.

The interface turns out to be the interesting part. It is not the
FoundationDB one. A seeded simulator (topic 16's
[FDB](reading-fdb-simulation.md) and [turso](reading-turso-simulator.md)
chapters) asks your program to be deterministic so that a failing run
can be *replayed*. The Antithesis SDK asks for something stronger and
stranger — that your program never remember a random value, and never
seed a PRNG from one — and the reason is that its simulation does not
replay a line, it **branches a tree**. Once you see why the SDK is
written the way it is, the design of the platform is legible from the
outside.

The second idea worth stealing costs nothing and needs no vendor: an
assertion vocabulary in which `Sometimes` is a first-class kind. A
`Sometimes` assertion is not a safety property, it is a **coverage**
property — it fails when your test campaign never reached an
interesting state — and it is the direct answer to the failure mode
every fault-injection harness eventually has, where the faults quietly
stopped firing and the suite stayed green.

Anchors are `antithesishq/antithesis-sdk-rust` at the commit
`resources/codebases.md` pins, quoted with the line numbers they occupy
there. Paths are relative to the repository root. Claims about the
platform itself are attributed to the SDK's own documentation, and
flagged where they cannot be checked from this side.

## The problem in one sentence

A fault-injecting test harness has two failure modes — it can miss a bug
your system has, and it can *stop exercising* the system while continuing
to pass — and the second one is invisible unless "this interesting thing
must actually happen" is something you can assert, and expensive to fix
unless the search knows which direction is interesting.

## The concepts, step by step

### Step 1 — replaying a line versus branching a tree

> **In:** deterministic simulation as
> [reading-fdb-simulation.md](reading-fdb-simulation.md) built it — one
> seed, one history, replayable. **Out:** the different model the SDK's
> rules imply, and the evidence for it in the source.

FoundationDB-style DST removes nondeterminism so that a run is a
function of a seed. The value delivered is *replay*: a failure is a
`u64` you can hand to a colleague.

The Antithesis SDK asks for two things that replay does not require, and
states the reason for both:

```rust
// lib/src/random.rs, lines 3-15 — the contract on get_random.
// The word to notice is on line 6: branch.
     3  /// Returns a u64 value chosen by Antithesis.
     4  ///
     5  /// You should use this value immediately rather than using it
     6  /// later. If you delay, then it is possible for the simulation
     7  /// to branch in between receiving the random data and using it.
     8  /// These branches will have the same random value, which
     9  /// defeats the purpose of branching.
    10  ///
    11  /// Similarly, do not use the value to seed a pseudo-random
    12  /// number generator. The PRNG will produce a deterministic
    13  /// sequence of pseudo-random values based on the seed, so if the
    14  /// simulation branches, the PRNG will use the same sequence of
    15  /// values in all branches.
```

Read that as evidence rather than as advice. Both rules only make sense
if the platform can **snapshot the entire system state and resume it
more than once**, taking different random values down each copy. Under
plain seeded replay, caching a random value or seeding a PRNG from it
would be harmless — the seed determines everything anyway.

```
   seeded DST (FDB, turso)          branching search (what the SDK implies)

   seed ──▶ ●──▶●──▶●──▶●  ✗              ●──▶●──┬─▶●──▶●  ✗
            one history, replayable            │  └─▶●──▶●  ✓
            by re-running the seed             └─▶●──▶●     ✓
                                        a snapshot resumed several times;
                                        each resumption gets different
                                        randomness FROM THE PLATFORM
```

This is why "use it immediately" is a hard rule. If your code draws a
value, stores it, and uses it ten milliseconds later, any branch point
inside those ten milliseconds produces two futures holding the *same*
value — the fork explored nothing. The randomness must be requested at
the moment the decision is made, so that the decision is what forks.

The SDK's own words for the second half, at
`lib/src/lib.rs:34-35`: doing either of these things "makes it much
harder for the Antithesis platform to control the history of your
program's execution, and also makes it harder for Antithesis to learn
which inputs provided at which times are most fruitful."

**"Learn"** is the other load-bearing word, and Step 5 is about it.

### Step 2 — the assertion vocabulary

> **In:** Step 1's platform. **Out:** the five macros the SDK exports
> and the two independent bits each one sets, which is a cleaner
> vocabulary than `assert!`.

```
   macro                          condition must hold      must be reached
   ────────────────────────────   ──────────────────────   ───────────────
   assert_always!                 every time it runs       yes
   assert_always_or_unreachable!  every time it runs       no
   assert_sometimes!              at least once            yes
   assert_reachable!              —                        yes
   assert_unreachable!            —                        never
```

Two independent bits: *what must be true of the condition*, and *whether
the site has to be executed at all*. Ordinary `assert!` fixes the first
to "always" and the second to "don't care", which is why a test suite
can go green while never running the code.

In the source the second bit is literally a flag called `must_hit`:

```rust
// lib/src/assert/macros.rs, lines 115-125 — assert_always. The macro is a thin
// wrapper; line 123 is the whole difference from assert_always_or_unreachable.
   115  macro_rules! assert_always {
   116      ($condition:expr, $message:literal$(, $details:expr)?) => {
   117          $crate::assert_helper!(
   118              condition = $condition,
   119              $message,
   120              $(details = $details)?,
   121              $crate::assert::AssertType::Always,
   122              "Always",
   123              must_hit = true
   124          )
   125      };
```

`assert_always_or_unreachable!` (`macros.rs:151`) is the same macro with
`must_hit = false`, and the doc line above it says the property "will
pass even if the assertion is never encountered".

Underneath there are only three assertion types —
`AssertType::{Always, Sometimes, Reachability}` (`lib/src/assert/mod.rs:93-96`)
— crossed with `must_hit` and, for reachability, the polarity. The
`message` string is not a comment: the SDK's docs say "Antithesis
generates one test property per unique `message`", so it is the
property's *identity* across the whole campaign (`lib/src/lib.rs:6-8`).

### Step 3 — `Sometimes` is a coverage property, and it is the useful one

> **In:** Step 2's table. **Out:** the assertion kind that has no
> equivalent in ordinary testing, and the failure mode it catches — with
> the version you can apply to this topic's own bench today.

`assert_sometimes!(cond, "…")` passes if `cond` was true **at least
once** anywhere in the test campaign. Nothing in `assert!`-shaped
testing does this, because it is not a property of a run; it is a
property of the *search*.

What it catches: the harness that stopped working.

```
   what you wrote                        what you meant
   ──────────────                        ──────────────
   assert_always!(no_data_loss, …)       durability holds
   assert_sometimes!(crashed_mid_fsync,  … AND we actually tried the case
                     "torn write hit")      where it could fail
```

Without the second line, a fault injector whose probability drifted to
zero — a config typo, a refactor that stopped threading the fault
handle, a timeout that now fires before the interesting window — leaves
a permanently green suite that tests nothing. Anyone who has run a
crash-injection harness for a year has had this happen.

This topic already has the measurement that makes the point. The
`crash_matrix` lane reports:

```
   bug                      caught         rate
   None                          0         0.0%     ← the anti-vacuity check
   TornWriteAccepted          2442        48.8%
   NoSyncOnCommit             4980        99.6%
```

The `None` row is a hand-rolled `Sometimes` assertion: it asserts that
the oracle does *not* fire on a correct implementation. And
`TornWriteAccepted` at 48.8% is the flip side — the harness is reaching
the interesting state only half the time, which is exactly the quantity
a `Sometimes` assertion turns from invisible into reportable. Exercise:
add `sometimes_seen: HashSet<&str>` to `crash_matrix`, record which
fault classes actually fired per seed, and print the ones that never
did. That is the whole idea, and it costs twenty lines.

### Step 4 — the catalog: assertions the platform knows about but has never seen

> **In:** Step 3's `must_hit`. **Out:** the mechanism that makes "never
> reached" reportable at all, which is a neat piece of Rust.

A `Sometimes` assertion that is never executed cannot report itself —
there is no code running to do it. So the SDK registers every assertion
site *at startup*, before any of them runs:

```rust
// lib/src/assert/mod.rs, lines 20-24 and 32-35 — the catalog and its
// registration. Line 22's attribute is what makes it work.
    20  /// Catalog of all antithesis assertions provided
    21  #[doc(hidden)]
    22  #[distributed_slice]
    23  #[cfg(feature = "full")]
    24  pub static ANTITHESIS_CATALOG: [AssertionCatalogInfo];
    // ... 26-30: the same for the guidance catalog ...
    32  #[cfg(feature = "full")]
    33  pub(crate) static INIT_CATALOG: Lazy<()> = Lazy::new(|| {
    34      for info in ANTITHESIS_CATALOG.iter() {
    35          let f_name: &str = info.function.as_ref();
```

A **distributed slice** (the `linkme` crate) is a static array assembled
by the *linker*: each macro expansion contributes an element from
wherever it appears in the crate graph, and the whole array exists
before `main` runs. Each element carries the assertion's identity and
source location (`mod.rs:112-124`): type, display type, message, class,
function, file, line, column, `must_hit`, id.

So the platform is told "here are the 340 properties this binary can
report, with their source locations" at startup, and each execution then
reports which of them were *hit* and with what result. The emitted JSON
carries both bits — the doc comment at `mod.rs:341-343` shows three
records for one assertion, differing in `condition` and `hit`.

That is the piece to steal even if you never use Antithesis: **a
property that was never evaluated must still be enumerable**, or your
coverage report is a report about the code that ran.

### Step 5 — guidance: telling the search which way is interesting

> **In:** the "learn which inputs are most fruitful" claim of Step 1.
> **Out:** the channel through which a program tells the search it is
> getting warmer, and its equivalent in the previous chapter.

Beside the assertion catalog there is a guidance catalog
(`mod.rs:26-30`), and beside the boolean assertions there is a family of
comparison macros (`macros.rs:414-580`):

```
   assert_always_greater_than!            assert_sometimes_greater_than!
   assert_always_greater_than_or_equal_to!   … _or_equal_to!
   assert_always_less_than!               assert_sometimes_less_than!
   assert_always_less_than_or_equal_to!      … _or_equal_to!
   assert_always_some!  /  assert_sometimes_all!    (over a set of named clauses)
```

These do not just assert; they emit **guidance** — a record whose
`GuidanceType` is `Numeric`, `Boolean` or `Json`
(`lib/src/assert/guidance.rs:197-201`), carrying a `maximize` flag
(`:209`). Where a plain
`assert_always!(queue_len < 1000)` tells the platform only pass or fail,
`assert_always_less_than!(queue_len, 1000, …)` tells it the *margin*, so
a run that reached 998 is known to be more interesting than one that
reached 12, and the search can push in that direction.

You have just read the same idea in
[reading-hypothesis.md](reading-hypothesis.md) Step 7: Hypothesis's
`target` phase, where a test calls `target(value)` and the engine
mutates toward larger values, with a Pareto front for multiple
objectives. Same mechanism — a scalar fitness signal from inside the
system under test — at two very different scales: one process versus a
fleet, seconds versus days.

The database-shaped instances of this are worth naming: replication lag,
queue depth, open transaction count, WAL size, time since last
successful fsync, clock skew between nodes. Each is a number your system
already computes, and each is a direction a search would otherwise have
to find by luck.

### Step 6 — the transport, and why the same binary runs anywhere

> **In:** Steps 2–5, all of which emit records. **Out:** where those
> records go, and the three-way fallback that keeps instrumented code
> runnable on your laptop.

```rust
// lib/src/internal/voidstar_handler.rs, lines 7-18 — the entire platform interface
     7  const LIB_NAME: &str = "/usr/lib/libvoidstar.so";
     // ... 8-9 ...
    10  pub struct VoidstarHandler {
    11      // Not used directly but exists to ensure the library is loaded
    12      // and all the following function pointers points to valid memory.
    13      _lib: Library,
    14      // SAFETY: The memory pointed by `s` must be valid up to `l` bytes.
    15      fuzz_json_data: unsafe fn(s: *const c_char, l: size_t),
    16      fuzz_get_random: fn() -> u64,
    17      fuzz_flush: fn(),
    18  }
```

Three C symbols, dynamically loaded from a fixed path: push a JSON
record, get a random `u64`, flush. That is the whole boundary between
your instrumented program and the platform. Everything in Steps 2–5 —
assertions, catalog, guidance, lifecycle — is JSON down `fuzz_json_data`,
and Step 1's branching is `fuzz_get_random`.

And the fallback, which is the part that makes instrumenting worthwhile
even if you never buy anything:

```rust
// lib/src/internal/mod.rs, lines 57-66 — three environments, one binary
    57  #[cfg(feature = "full")]
    58  fn get_handler() -> Box<dyn LibHandler + Sync + Send> {
    59      match VoidstarHandler::try_load() {
    60          Ok(handler) => Box::new(handler),
    61          Err(_) => match LocalHandler::new() {
    62              Some(h) => Box::new(h),
    63              None => Box::new(NoOpHandler::new()),
    64          },
    65      }
    66  }
```

Inside the platform, `libvoidstar.so` loads and records go to it.
Outside it, `LocalHandler` writes the same JSON to the file named by
`ANTITHESIS_SDK_LOCAL_OUTPUT` (`mod.rs:54`) — so you get a local
assertion log with the same schema. With neither, `NoOpHandler`, and the
assertions cost approximately nothing.

`random::get_random` falls back to the Rust standard library outside the
platform (`lib.rs:38-39`), and `AntithesisRng` plugs the same source into
the `rand` ecosystem for whichever `rand` version you already depend on
(`lib.rs:41-51`). So an instrumented program is an ordinary program
everywhere else, which is the only way instrumentation of this kind ever
survives in a codebase.

### Step 7 — lifecycle: telling the search when the interesting part starts

> **In:** the branching search of Step 1. **Out:** the two calls that
> shape *where* it spends its budget.

```rust
// lib/src/lifecycle.rs — two functions, at :37 and :63
   37  pub fn setup_complete(details: &Value) {
   63  pub fn send_event(name: &str, details: &Value) {
```

`setup_complete` says the system is initialised and the workload is
about to begin: booting a five-node cluster is not the part worth
exploring a thousand times, and a branching search that does not know
where setup ends will waste its budget on it. `send_event` marks a named
milestone with a JSON payload during the run.

The parallel in this repo is exact: topic 0's benchmarking chapter
insists on warmup being excluded from measurement, and this is warmup
being excluded from *search*. Both are the same instruction — do not
spend your budget on the part you already understand.

### Step 8 — what this chapter can and cannot tell you

> **In:** Steps 1–7, all sourced from the SDK. **Out:** an explicit line
> between what the source proves and what remains a vendor claim, so you
> can cite this chapter safely.

What the open-source SDK establishes:

- The platform supplies randomness on demand and the program is
  forbidden from caching it, in language that only makes sense if
  execution **branches** (Step 1).
- Assertions are enumerated at link time and reported by identity, so
  properties that were never reached are still known (Step 4).
- The program can emit a scalar fitness signal to steer the search
  (Step 5).
- The whole interface is three C symbols (Step 6).

What it does not, and this chapter therefore does not assert: how the
determinism is achieved, what the snapshotting costs, how branches are
scheduled or prioritised, how much of a state space a campaign covers,
or any performance number whatsoever. Those are the closed part. This
repo's rule is that a number you cannot check does not go in a guide,
and none of Antithesis's published figures can be checked from here.

Which leaves a fair summary: **the ideas are free and the
implementation is not.** `Sometimes` assertions, an enumerable catalog,
guidance signals and a lifecycle marker can all be built into a harness
you already own — Step 3's exercise is the smallest version — and the
deterministic hypervisor underneath cannot.

## How to read the source (with the concepts in hand)

The `antithesis-sdk-rust` crate is about 3,200 lines of Rust including
tests; an hour is enough.

1. `lib/src/lib.rs` module docs first — Steps 1, 5 and 6 in the author's
   own words.
2. `lib/src/assert/macros.rs`. Read `assert_always!` (`:115`) and
   `assert_always_or_unreachable!` (`:151`) side by side, then
   `assert_sometimes!` (`:188`), `assert_reachable!` (`:227`) and
   `assert_unreachable!` (`:267`). The `assert_helper!` at `:35`/`:89`
   is where the two `cfg` worlds split.
3. `lib/src/assert/mod.rs:20-124` — the catalog, `AssertType`, and
   `AssertionCatalogInfo`. Then the doc comment at `:341-343`, which
   shows the emitted JSON for one assertion in three states.
4. `lib/src/assert/guidance.rs:195-217` — `GuidanceType`, `maximize`,
   and the record shape.
5. `lib/src/internal/mod.rs:57-89` — the handler fallback and the
   `LibHandler` trait, then `voidstar_handler.rs` entire (60 lines).
6. `simple/src/main.rs` and `simple/src/rand.rs` — the worked example
   program, which is short and shows the intended call sites.

Then, to make it concrete, instrument this topic's own `dst_run` harness
with `assert_sometimes!`-equivalents and see which of your fault classes
have never fired.

## Questions (answer in notes.md)

1. Step 1 argues that "never seed a PRNG from `get_random`" implies
   branching. Construct the concrete two-branch scenario in which a
   seeded PRNG makes the fork useless, using a KV store's crash-point
   choice as the decision.
2. Both `assert_always!` and `assert_always_or_unreachable!` demand the
   condition hold whenever evaluated. Give a database example where you
   genuinely want `must_hit = false`, and one where accepting it would
   hide a real regression.
3. Take the `crash_matrix` lane's five bug rows. Write the `Sometimes`
   assertions that would have caught a harness in which the crash
   injector silently stopped firing, and say what each one's failure
   message should contain to be actionable at 3am.
4. Guidance gives the search a scalar to maximise. Pick three for a
   replicated KV store, and for each say what a *maximising* search
   would do that a uniform random one would not — including the case
   where maximising it is actively unhelpful.
5. The catalog is assembled by the linker (`linkme`). What breaks if an
   assertion lives in a crate that is compiled but never linked into the
   final binary, and how would you detect that in CI?
6. Compare the failure artefact of the three systems in this topic: a
   `u64` seed (FDB/turso), a shrunk choice sequence (Hypothesis), and a
   platform-side branch history (Antithesis). Which can you put in a
   commit message, which can you replay in CI, and which needs the
   vendor?

## Done when

Answer each before unfolding it.

- [ ] You can say what the SDK's rules about randomness prove about the
      platform.
  <details><summary>Answer</summary>

  `random.rs:3-15` forbids both storing a value for later use and
  seeding a PRNG from it, and gives the same reason for both: the
  simulation may **branch**, and both branches would then hold identical
  values, "which defeats the purpose of branching". Neither rule would
  be necessary under plain seeded replay, where the seed determines
  everything anyway. So the platform snapshots system state and resumes
  it more than once with different randomness — a tree, not a line.
  Randomness must be requested at the moment of the decision so that the
  decision is what forks.
  </details>

- [ ] You can name the two independent bits an Antithesis assertion sets.
  <details><summary>Answer</summary>

  What must be true of the condition (`Always`, `Sometimes`, or nothing
  for pure reachability — `AssertType` at `assert/mod.rs:93-96`), and
  whether the site must be executed at all (`must_hit`, set to `true` at
  `macros.rs:123` for `assert_always!` and `false` for
  `assert_always_or_unreachable!` at `:151`). Ordinary `assert!` pins
  the first to "always" and leaves the second unstated, which is how a
  suite goes green while never executing the code.
  </details>

- [ ] You can explain why `Sometimes` is a coverage property and give
      the failure it catches.
  <details><summary>Answer</summary>

  It passes if the condition held at least once across the campaign, so
  it is a claim about the *search*, not about any run. It catches the
  harness that stopped working — a fault injector whose probability
  drifted to zero, a fault handle a refactor stopped threading through —
  which otherwise leaves a permanently green suite testing nothing. This
  topic's `crash_matrix` already has a hand-rolled version: the `None`
  row at 0.0%, which asserts the oracle does not fire on a correct
  implementation, and the 48.8% `TornWriteAccepted` rate, which is
  exactly the "did we reach the interesting state" quantity.
  </details>

- [ ] You can say how a never-executed assertion gets reported.
  <details><summary>Answer</summary>

  Every assertion site contributes an `AssertionCatalogInfo` — type,
  message, class, function, file, line, column, `must_hit`, id
  (`assert/mod.rs:112-124`) — to a `#[distributed_slice]` assembled by
  the linker (`:20-24`), which is walked at startup (`:32-35`). The
  platform therefore knows every property the binary *can* report before
  any of them runs, and each execution reports which were `hit` and with
  what `condition`. Without that, a `Sometimes` assertion that never
  executes has no code available to report itself.
  </details>

- [ ] You can connect guidance to something you have already read.
  <details><summary>Answer</summary>

  Guidance is a scalar fitness signal from inside the system under test:
  the comparison macros (`macros.rs:414-580`) emit a guidance record —
  `Numeric`, `Boolean` or `Json` — with a `maximize` flag
  (`guidance.rs:197-209`), so the search learns that a run reaching 998
  is more interesting than one reaching 12. It is Hypothesis's `target`
  phase (`reading-hypothesis.md` Step 7) — same idea, one process versus
  a fleet. For a database: replication lag, queue depth, open
  transaction count, WAL size, clock skew.
  </details>

- [ ] You can state the boundary between what the source shows and what
      it does not.
  <details><summary>Answer</summary>

  The SDK proves the *interface*: on-demand randomness with a
  no-caching contract, a link-time assertion catalog, guidance signals,
  a lifecycle marker, and a three-symbol boundary
  (`fuzz_json_data`, `fuzz_get_random`, `fuzz_flush` —
  `voidstar_handler.rs:15-17`). It shows nothing about how determinism
  is achieved, what a snapshot costs, how branches are scheduled, or how
  much of a state space a campaign covers — and none of the published
  figures can be checked from this side, so this chapter quotes none of
  them. The ideas are reusable; the hypervisor is not.
  </details>

## References

- The `antithesis-sdk-rust` SDK (`antithesishq/antithesis-sdk-rust`) at the
  pinned commit (see the pin table at the end of
  [resources/codebases.md](../../resources/codebases.md)). Files read
  here: `lib/src/lib.rs`, `lib/src/random.rs`, `lib/src/assert/macros.rs`,
  `lib/src/assert/mod.rs`, `lib/src/assert/guidance.rs`,
  `lib/src/lifecycle.rs`, `lib/src/internal/mod.rs`,
  `lib/src/internal/voidstar_handler.rs`.
- Antithesis's own documentation (`antithesis.com/docs/`), cited by the
  SDK's doc comments for the definitions of *test property*,
  *workload*, and *triage report*. Treat it as vendor documentation:
  useful for the vocabulary, not a source for numbers.
- Jingyu Zhou et al., **"FoundationDB: A Distributed Unbundled
  Transactional Key Value Store"**, SIGMOD 2021 — the simulation
  testing this platform's founders came from; read
  [reading-fdb-simulation.md](reading-fdb-simulation.md) first.
- In this topic: [reading-hypothesis.md](reading-hypothesis.md) (Step
  5's guidance is its `target` phase; Step 3's coverage property is what
  its swarm testing is trying to reach),
  [reading-turso-simulator.md](reading-turso-simulator.md) (the
  open-source system closest to this design).
- Alex Groce et al., **"Swarm Testing"**, ISSTA 2012 — the cheapest way
  to make `Sometimes` assertions start passing.
