# Compaction is four axes, not two strategies

"Leveled vs tiered" is a false binary: a compaction policy is an independent
choice on four design axes — trigger, layout, granularity, movement — and
every system you've read in this topic sits somewhere in that grid. This is
the taxonomy chapter; read it LAST of the four papers, because it organizes
the other three.

## The four axes (§3 — the contribution)

```
 a compaction policy = choice on each axis:

 1. TRIGGER      when?    level saturation / #runs / staleness / space amp
 2. DATA LAYOUT  what shape?   leveling / tiering / 1-leveling / L-leveling / hybrid
 3. GRANULARITY  how much at once?   whole level / one file (RocksDB) / few files
 4. DATA MOVEMENT who moves?   full merge / trivial move (relink non-overlapping)
```

Your mini-LSM: trigger = level size, layout = leveled or tiered, granularity =
whole level, movement = full merge (+ trivial move if you stole lsm-tree's
`Choice::Move`). Locate every system you've read on these axes — RocksDB
leveled is (saturation, leveling, one-file, merge+trivial-move).

## Reading order

1. §3 — the taxonomy. Make the table for: your mini-LSM, lsm-tree crate,
   RocksDB leveled, RocksDB universal, FIFO.
2. §4 — the benchmark methodology: they implement the design space inside one
   engine to compare fairly. This is the Fair Benchmarking paper's lesson
   (topic 0) applied — same engine, one variable.
3. **§5 findings** — the ones worth keeping:
   - file-granularity compaction (RocksDB style) smooths write stalls vs
     whole-level (spikes) — granularity is a *tail latency* knob, not a
     throughput knob;
   - trigger choice dominates point-lookup latency more than layout at low
     write rates;
   - no policy wins everywhere (the RUM conjecture, empirically, again).
4. Skim the workload sensitivity plots — note which finding you'll test.

## Questions to answer in notes.md

1. Your write_amp experiment compacts whole levels. Predict, then measure if
   time allows: what does per-insert p99.9 look like vs a per-file granularity
   variant? (This is topic 2's rehash-spike lesson at LSM scale.)
2. Which axis does Dostoevsky's lazy leveling move on? (Layout only — trigger/
   granularity/movement orthogonal.) Which does Monkey move on? (None — it's
   a filter-memory axis the taxonomy doesn't cover; where would you add it?)
3. For M4's graph-snapshot SSTs: bulk-loading a snapshot is one giant sorted
   run. Which axis choices make ingest cheap? (Trivial move into the bottom
   level — no merge at all.)

## Done when

Your notes contain the 5-system × 4-axis table and one prediction you could
test with the mini-LSM.

## References

**Papers**
- Sarkar, Papon, Staratzis, Athanassoulis — "Constructing and Analyzing
  the LSM Compaction Design Space" (VLDB 2021) — §3 taxonomy and §5
  findings are the keepers; §4's one-engine methodology is the Fair
  Benchmarking lesson applied
