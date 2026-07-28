# Topic 1 — Notes

Numbers from this machine (Apple Silicon, macOS). Record *why*, not just what.

## Baseline (provided lane, Apple M3 Pro, measured 2026-07-28)

`cargo run --release` — 1.08 M records of 100 B, random key order, batches of
1000, `sync()` at the end. Logical bytes vs bytes actually on disk:

| engine | family | logical | on disk | space amp |
|---|---|---|---|---|
| fjall | LSM | 108.0 MB | 48.4 MB | **0.45×** |
| redb | B-tree (CoW) | 108.0 MB | 6833.9 MB | **63.28×** |

**A 140× spread on the same data, and the LSM's number is below 1.0.** Both
halves are worth sitting with:

- fjall lands at 0.45× because an LSM writes *compressed sorted runs*: the
  value bytes are LZ4'd on the way into the SST, so "amplification" below 1 is
  not a paradox, it is the third axis of the RUM triangle being spent — read
  cost — to buy space.
- redb's 63× is **not** a defect, it is this workload hitting a copy-on-write
  B-tree at its worst point. Random-order inserts touch a new leaf almost every
  time; each of the 1080 batch commits copies every page on the path to the
  root and cannot reuse the old ones until a later commit frees them. Random
  keys plus per-batch durability plus no compaction is the adversarial case,
  and it is exactly the case the RUM conjecture says you cannot escape — you
  can only choose which axis pays.

The honest caveat, and the reason this is a *starting* number rather than a
verdict: this measures ONE point in the design space (random keys, small
batches, no compaction pass afterwards). Change the key order to sequential, or
compact at the end, and redb's figure collapses. The exercise lanes are where
you find out how far.

## Predictions (write BEFORE running the shootout)

Per README §7 — predict the winner and the mechanism, then let the data grade you:

| Workload | Predicted winner | Predicted mechanism | Verdict |
|----------|------------------|---------------------|---------|
| fillrandom | | | |
| fillseq | | | |
| readrandom (zipf) | | | |
| readrandom (uniform) | | | |
| scan | | | |
| space amp | | | |

## Shootout results

(engine versions: fjall 2.x, redb 2.6 — pin exact versions from Cargo.lock here;
durability parity: fjall `PersistMode::Buffer` vs redb `Durability::None`.)

- First smoke run (`cargo run --release 20000`): both engines report ~15x
  "space amplification" — at 20K × 108B (2.2MB logical) the number is fixed overhead
  (fjall's preallocated journal, redb's initial region sizing), not amplification.
  Lesson from topic 0: measure at a size where the effect dominates the floor.
  Re-run at n=1M+ for the real number.

## Papers

### O'Neil '96 — LSM-Tree
(questions from reading-lsm-paper.md)

### Comer '79 — The Ubiquitous B-Tree
(questions from reading-comer-btree.md)

### RUM Conjecture (EDBT '16)
(questions from reading-rum-conjecture.md — place shootout results on the triangle)

### Architecture of a DBMS (2007)
(questions from reading-architecture-of-a-dbms.md)

## Code reading

### fjall
### turso btree/pager
### tidesdb
### RocksDB layout

## M1 — storage backend abstraction

Design rationale lives in `capstone/notes/m1-backend-design.md`; comparison with the
reference `graph/src/storage/backend.rs` goes there too (only AFTER the design).
