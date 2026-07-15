# The LSM-tree: an IO scheduling policy, not a data structure

Where the origin of the LSM half of the topic's dichotomy gets read on its
own terms. Before the paper, this chapter builds the idea from zero — the
write problem, the buffer-and-flush trick, the merge that keeps reads sane,
and the three amplifications that name the price — then hands you a
section-by-section route. Warning up front: **1996 LSM ≠ 2026 LSM.** The
paper's C0/C1 components are B-trees merged by "rolling merge"; modern LSMs
(LevelDB lineage) use immutable sorted files + whole-file compaction. Read
it for the *cost model* — that part is timeless — and translate the
mechanism as you go.

## The problem in one sentence

The motivating workload is TPC-A account history — a firehose of inserts,
almost never read — and indexing it with a B-tree costs one random disk IO
per insert: at ~5 ms per seek that's **~200 inserts/second per disk**, no
matter how fast the CPU is.

## The concepts, step by step

### Step 1 — the write problem: random in-place writes

An in-place index like a B-tree updates data where it lives: an insert
reads the target leaf page from disk, modifies it, and writes it back to
the same spot. Because keys arrive in essentially random order, each insert
lands on a random page — and on a 1996 disk a random page access is a
mechanical seek, ~5–10 ms:

```
 B-tree insert path, keys arriving in random order:

   insert(k₁) → seek to page 8,312  → read 4 KB → write 4 KB   ~10 ms
   insert(k₂) → seek to page 41,907 → read 4 KB → write 4 KB   ~10 ms
   ...
   ⇒ ~100–200 inserts/s per spindle — while the SAME disk streams
     sequential writes at MB/s ⇒ thousands of entries/s if only
     we could write them in file order
```

That ~100× gap between random IOPS and sequential bandwidth is the topic 0
ladder again — and note the waste: a 4 KB page is rewritten to change one
~100-byte entry.

### Step 2 — the idea: buffer in memory, flush sorted runs sequentially

Instead of updating disk in place, collect inserts in a sorted in-memory
tree — the paper's **C0 component** — and, when it fills, write its
contents out to disk in one big sequential pass. Durability comes from a
**write-ahead log** (an append-only file each insert is written to first —
itself a sequential write, so it doesn't reintroduce the problem). One
flush amortizes thousands of inserts over a single sequential IO burst. The
whole 1996 idea fits in one loop — defer, batch, write sequentially, and
pay for it at read time:

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

The `get` half is the fine print — Steps 3 and 4.

### Step 3 — the merge (compaction): what keeps reads bounded

Flushing sorted runs forever would litter the disk with hundreds of files,
and a lookup would have to check every one of them — so the engine
continually **merges** freshly flushed data into a larger on-disk sorted
component, the paper's **C1** (modern name for the ongoing process:
**compaction**). Merging two sorted inputs is a single interleaved pass:
sequential reads in, sequential writes out, output pages packed ~100% full.

Concretely: without merging, a year of 64 MB memtable flushes is thousands
of separate sorted runs — thousands of places a point read must look. With
merging, a read checks C0 then C1: **two** places. The price is that every
entry gets *rewritten* during merges, over and over — which needs a name.

### Step 4 — naming the price: read, write, and space amplification

The trade now has standard names. **Write amplification** (bytes actually
written to disk per byte of user data): every merge rewrites entries that
were already on disk, so one logical insert may be physically written many
times over its lifetime. **Read amplification** (number of components or
pages consulted per lookup, versus the one that actually holds the answer):
a read must check every component, newest first — the paper admits this
openly. **Space amplification** (bytes on disk per byte of live data):
overwritten and deleted entries linger in older components until a merge
finally drops them.

An LSM buys its ~100× insert speedup by moving cost *into* read and space
amplification; a B-tree makes the opposite trade. That three-way tension is
the RUM conjecture chapter, verbatim.

### Step 5 — 1996's rolling merge vs modern leveled/tiered

The paper's merge is a *rolling cursor*: C1 stays one single valid B-tree
at all times, and the merge continuously cycles through it in key order,
rewriting pages in place-ish fashion. Modern LSMs dropped that: they write
**immutable sorted files** (SSTs) and compact by merging whole files into
new files, deleting the inputs — simpler crash recovery (files are never
modified, only created and deleted) in exchange for lumpier IO. Translate
as you read:

```
paper (1996)                         modern (LevelDB lineage)
─────────────                        ────────────────────────
C0 in-memory AVL/2-3 tree      →     memtable (skiplist)
C1 on-disk B-tree              →     a level of immutable SSTs
rolling merge cursor           →     compaction job
filling disk pages ~100% full  →     SST blocks, sequentially written
```

The paper's §4 generalizes to multi-component C0…Ck with a size ratio `r`
between adjacent components — exactly modern leveled compaction's fanout-10
geometry, and its optimal-`r` derivation prefigures Monkey/Dostoevsky
(topic 4).

### Step 6 — the punchline: an IO scheduling policy, not a data structure

Strip the mechanism away and nothing about the *data* changed — same
entries, same sort order, same queries; the only thing the LSM changed is
**when and in what order bytes reach the disk**. The paper's §3 `COST_π`
algebra makes this precise: with batching, each insert's amortized IO cost
is `~(entry_size / page_size) × WA` *sequential* bytes instead of one
random page read+write — the algebra formalizes "sequential bandwidth is
~100× cheaper than random IOPS", the topic 0 ladder in 1996 dollars. That
is why "LSM vs B-tree" survives every hardware generation: it's a policy
choice about IO scheduling, and the constants change but the policy
question doesn't.

## How to read the paper (with the concepts in hand)

Read in this order:

1. **§1 (intro + The Five Minute Rule)** — the economic argument: pages hot
   enough are worth keeping in RAM; LSM works because *recent* data is hot
   by construction (Step 2's C0 is exactly the hot set).
2. **§2 (two-component LSM)** — Steps 2–3 in the authors' words: C0, C1,
   and the rolling merge. Keep Step 5's translation table open and convert
   every term to its modern equivalent as you read.
3. **§3 (cost model)** — the payoff, Step 6. Work the `COST_π` algebra
   until "amortized sequential bytes per insert" feels obvious; this is the
   timeless part.
4. **§4–5 (multi-component + concurrency/recovery)** — skim. Multi-component
   C0…Ck with size ratio `r` is modern leveled compaction (Step 5); the
   optimal-`r` derivation prefigures Monkey/Dostoevsky (topic 4). The
   concurrency/recovery machinery is what immutable SSTs made obsolete.
5. **§6 (comparison)** — skim; the competitors (MD/1 hashing, TSB-tree) are
   dead, the framing (amortized cost per insert) survived.

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
