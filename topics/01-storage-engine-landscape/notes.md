# Topic 1 — Notes

Numbers from this machine (Apple Silicon, macOS). Record *why*, not just what.

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
