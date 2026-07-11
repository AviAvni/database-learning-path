# Reading guide — turso's deterministic simulator

Clone: `~/repos/turso` (`testing/simulator/`, plus `fuzz/`). The most
readable production DST codebase in Rust — read it as the reference
implementation for our `dst.rs` stub and for M16.

## Layout

```
 testing/simulator/
   main.rs          entry: seed → config → plan → execute → check
   runner/
     clock.rs       SimulatorClock — time is an RNG stream
     io.rs          SimulatorIO — fault injection switchboard
     file.rs        SimulatorFile — per-op faults + seeded latency
     execution.rs   drive the plan, catch assertion failures
     doublecheck.rs run the same plan twice, diff outputs
     bugbase.rs     known-bug corpus (regression seeds)
   generation/      plan/property/query generators
   model/           the in-memory oracle + interaction model
   shrink/          plan minimization
```

## Anchor map

| anchor | what it is |
|---|---|
| runner/clock.rs:8-13 | `SimulatorClock { curr_time, rng: ChaCha8Rng, min_tick, max_tick }` |
| runner/clock.rs:25-34 | `now()` ADVANCES time by a seeded random tick — time is data |
| runner/io.rs:14 | `fault: Cell<bool>` — the injection master switch |
| runner/io.rs:64-77 | `inject_fault` / `inject_fault_selective` (per-file stem!) |
| runner/io.rs:135-138 | per-op fault counters: pread/pwrite/sync faults |
| runner/file.rs:40 | `latency_probability` — seeded IO delay |
| runner/file.rs:100-110 | `generate_latency_duration` — random_bool from the file's rng |
| runner/file.rs:149-233 | every op (read/write/sync) can be delayed into a `DelayedIo` queue |
| generation/property.rs:270 | `FsyncNoWait` / `FaultyQuery` — fault-flavored properties |
| generation/property.rs:276-282 | the metamorphic set: SelectSelectOptimizer, WhereTrueFalseNull, UnionAllPreservesCardinality, ReadYourUpdatesBack |
| fuzz/fuzz_targets/expression.rs:299 | `fuzz_target!(\|expr: Expr\|)` — STRUCTURED fuzzing via arbitrary |

## 1. Time is an RNG stream (`clock.rs:25`)

Every `now()` call advances the clock by `random_range(min_tick..
max_tick)` — no wall clock anywhere. Question: why must `now()`
ADVANCE time rather than return a fixed value? (What loops forever
if time never moves? Think timeout code.)

## 2. Fault injection lives in the FILE (`file.rs`)

Not "kill the process" — per-operation faults: a pwrite can fail, a
sync can fail, any op can be delayed and reordered via the
`DelayedIo` queue. This is the fault model our `sim_fs.rs` copies
(buffered-until-sync + tear-on-crash). Question: which topic 5
crash-matrix cell does each of {pwrite fault, sync fault, delayed
write + crash} correspond to?

## 3. Properties = metamorphic oracles (`generation/property.rs`)

`SelectSelectOptimizer` is TLP-shaped: two spellings of the same
query must agree. `ReadYourUpdatesBack` is a session guarantee
(DDIA ch. 5 — same anomaly, single node). `DoubleCreateFailure`
pins error-path behavior. Note the generation trick: unrelated
random queries are interleaved WITHOUT breaking property invariants
— coverage and oracles coexist.

## 4. Doublecheck (`runner/doublecheck.rs`)

Run the identical plan twice; outputs must match byte-for-byte.
This is the cheapest oracle of all: it needs NO model — it only
needs determinism. Question: what class of bug does doublecheck
catch that the model oracle misses? (Hint: iteration order,
uninitialized memory, hidden wall-clock reads.)

## 5. The bug base (`runner/bugbase.rs`)

Found bugs persist as seeds — the regression suite is a list of
u64s. Compare: our topic 15 sim tests hardcode seeds 42/7/11/13.

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
