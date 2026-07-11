# The LSM-tree: an IO scheduling policy, not a data structure

Where the origin of the LSM half of the topic's dichotomy gets read on its
own terms. Warning up front: **1996 LSM ≠ 2026 LSM.** The paper's C0/C1
components are B-trees merged by "rolling merge"; modern LSMs (LevelDB
lineage) use immutable sorted files + whole-file compaction. Read it for the
*cost model* — that part is timeless — and translate the mechanism as you go.

## Why it was written

The motivating workload is TPC-A account history: massive insert rate, few reads.
Indexing it with a B-tree means one random page IO per insert. The paper's thesis:
**batch inserts in memory, migrate to disk sequentially, and the per-insert IO cost
drops by orders of magnitude.**

## Read in this order

1. **§1 (intro + The Five Minute Rule)** — the economic argument: pages hot enough
   are worth keeping in RAM; LSM works because *recent* data is hot by construction.
2. **§2 (two-component LSM)** — the core picture. Translate as you read:

```
paper (1996)                         modern (LevelDB lineage)
─────────────                        ────────────────────────
C0 in-memory AVL/2-3 tree      →     memtable (skiplist)
C1 on-disk B-tree              →     a level of immutable SSTs
rolling merge cursor           →     compaction job
filling disk pages ~100% full  →     SST blocks, sequentially written
```

   The whole 1996 idea fits in one loop — defer, batch, write sequentially,
   and pay for it at read time:

```rust
fn insert(&mut self, k: Key, v: Val) {
    self.wal.append(&k, &v);          // durability: a sequential append
    self.c0.insert(k, v);             // C0: sorted tree in RAM (≈ memtable)
    if self.c0.bytes() > THRESHOLD {
        // rolling merge: drain C0 into C1 in key order — pages written
        // sequentially, ~100% full; ONE batch amortizes thousands of inserts
        merge_into(&mut self.c0, &mut self.c1);
    }
}

fn get(&self, k: &Key) -> Option<Val> {
    self.c0.get(k).or_else(|| self.c1.get(k))   // the read-amp tax: check
}                                               // EVERY component, newest first
```

3. **§3 (cost model)** — the payoff. The key result, in modern words: with batching,
   each insert's amortized IO cost is `~(entry_size / page_size) × WA` sequential
   bytes instead of one random page read+write. The `COST_π` algebra formalizes
   "sequential bandwidth is ~100x cheaper than random IOPS" — the topic 0 ladder in
   1996 dollars.
4. **§4–5 (multi-component + concurrency/recovery)** — skim. Multi-component C0…Ck
   with size ratio `r` between adjacent components is exactly modern leveled
   compaction's fanout-10 geometry; the optimal-`r` derivation prefigures
   Monkey/Dostoevsky (topic 4).
5. **§6 (comparison)** — skim; the competitors (MD/1 hashing, TSB-tree) are dead, the
   framing (amortized cost per insert) survived.

## Questions to answer in notes.md

1. The paper claims LSM trades *what* for its insert speedup? (It's read amp — find
   where the paper admits point reads must check every component.)
2. Rolling merge keeps C1 a valid B-tree at all times. What do modern LSMs give up by
   using immutable files instead, and what do they gain? (Hint: crash recovery
   complexity vs write pattern.)
3. Derive: at size ratio r between components, an entry is rewritten how many times
   before reaching the last component? Relate to leveled WA ≈ r × levels.

## The one-line takeaway

LSM is not a data structure, it's an *IO scheduling policy*: convert random writes
into sequential ones by deferring and batching — and pay for it at read time.

## References

**Papers**
- O'Neil, Cheng, Gawlick, O'Neil — "The Log-Structured Merge-Tree
  (LSM-Tree)" (Acta Informatica 1996) —
  [PDF](https://www.cs.umb.edu/~poneil/lsmtree.pdf) — read §1–3 in
  order for the cost model; skim §4–6 and translate the mechanism to
  modern terms as you go
