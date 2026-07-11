# Reading guide — "Designing Access Methods: The RUM Conjecture" (Athanassoulis et al., EDBT 2016)

Short vision paper (~6 pages, 1 h). It doesn't build anything — it names the trade-off
every storage structure lives inside. Read it *after* the two engine papers so the
triangle has concrete corners.

## The claim

For any access method, define overheads relative to the bare minimum work:

- **RO** (read): bytes read ÷ bytes strictly needed to answer.
- **UO** (update): bytes written ÷ bytes logically changed.
- **MO** (memory/space): bytes stored ÷ bytes of live data.

**Conjecture: you can optimize any two; the third has a hard lower bound that grows
as the other two approach 1.** Not a proven theorem — a design compass (hence
"conjecture"; the paper is explicit about this).

## Read in this order

1. **§1–2** — definitions above; make sure you can compute RO/UO/MO for a plain
   sorted array (RO≈1, UO≈n/2 shifts, MO≈1) and a log (UO≈1, RO≈n, MO grows).
2. **§3 (the map)** — the paper places real structures on the triangle. Reproduce it:

```
                       RO = 1 (read-optimal)
                            ▲
                  B+tree ●  │  ● hash index
                            │
             LSM leveled ●  │  ● sorted array (static)
                            │
        LSM tiered ●        │        ● bitmap/bloom (approximate)
                            │
   log ●────────────────────┴────────────────────● compressed archive
 UO = 1 (update-optimal)                   MO = 1 (space-optimal)
```

3. **§4 (moving on the map)** — the punchline for this curriculum: knobs are
   *positions*, not settings. Bloom bits/key trades MO for RO. Compaction eagerness
   trades UO for RO. Page fill factor trades MO for UO. Monkey (topic 4) turns this
   into an actual optimization problem.
4. **§5 (research directions)** — skim; grade its 2016 predictions with 2026
   hindsight (adaptive/learned indexes, versioned data — how did they age?).

## Questions to answer in notes.md

1. Place your engine_shootout results on the triangle: which measured number is RO,
   UO, MO for fjall and redb?
2. Where does FalkorDB's matrix adjacency sit? (Dense-ish matrix: MO poor for sparse
   graphs — that's why delta matrices + roaring exist, topics 20/26.)
3. What's the RUM position of a WAL by itself? Why does *every* engine carry one
   anyway? (Durability isn't in the triangle — it's an orthogonal axis the paper
   deliberately excludes.)

## The one-line takeaway

There is no best index, only a workload-shaped position on a three-way frontier —
"which engine is better" is an ill-posed question until the workload is named.
