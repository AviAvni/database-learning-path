# The LSM-tree: an IO scheduling policy, not a data structure

Where the origin of the LSM half of the topic's dichotomy gets read on its own
terms. Before the paper, this chapter builds the idea from zero — the write
problem, the buffer-and-flush trick, the merge that keeps reads sane, and the
three amplifications that name the price — then hands you a section-by-section
route. Warning up front: **1996 LSM ≠ 2026 LSM.** The paper's C₀/C₁ components
are B-trees joined by a *rolling merge*; modern LSMs (the LevelDB lineage) use
immutable sorted files plus whole-file compaction. Read it for the **cost
model** — that part is timeless, and §3 states it in five equations — and
translate the mechanism as you go.

Every number below is either from the paper (cited to its section, example,
definition or equation) or from this repo's own measurement
([FINDINGS.md](../../FINDINGS.md) row 1). Nothing is a remembered figure.

## The problem in one sentence

The motivating workload is TPC-A account history — a firehose of inserts,
almost never read — and indexing it with a B-tree costs one random disk read
plus one random disk write per insert (§3.2, equation 3.1), which on 1995
hardware means **50 extra disk arms to sustain 1000 inserts/second, doubling the
cost of the whole system** (§1, Example 1.2).

## The concepts, step by step

### Step 1 — the write problem: random in-place writes

> **In:** an index that must stay sorted, and a stream of inserts whose keys
> arrive in random order.
> **Out:** the number the whole paper is attacking — inserts per second per
> disk arm, and why it is set by mechanics rather than by CPU.

**In-place** means an index updates a record where that record already lives:
the insert reads the target **leaf page** (the bottom-level, page-sized node
that actually stores entries) from disk, modifies it in a memory buffer, and
writes it back to the same disk address. Because the keys arrive in essentially
random order, each insert lands on a different, unpredictable page, and on a
1996 disk reaching an unpredictable page means moving the arm — a **seek**.

The paper does not hand-wave this; it prices it. From §1, Example 1.2, with the
paper's own parameters:

```text
Example 1.2 parameters (paper §1):
  insert rate                       1,000 index entries / second
  accumulation window               20 days × 8 hours
  index entry size  Se              16 bytes  (4 B Acct-ID + 8 B Timestamp
                                               + 4 B History-row RID, §5)
  page size         Sp              4,096 bytes

  entries    = 1000/s × 8 h × 3600 s/h × 20 d  =    576,000,000 entries
  leaf bytes = 576,000,000 × 16 B              =  9,216,000,000 B = 9.2 GB
  leaf pages = 9.216e9 / 4096                  =  2,250,000 ≈ 2.3 M pages
```

Those three figures — 576,000,000 entries, 9.2 GBytes, "about 2.3 million pages
needed on the index leaf level, even if there is no wasted space" — are the
paper's, restated in §5 verbatim. Now the cost:

```text
  per insert, B-tree:  1 random page read  + 1 random page write = 2 I/Os
  at 1000 inserts/s :  2,000 random I/Os per second
  1995 disk arm     :  ~40 usable I/Os per second   (§1; peak is 60-70, but
                        40 is "the nominal usable rate to avoid long queues", §3.1)
  arms required     :  2000 / 40 = 50 additional disk arms
```

and §1's conclusion: that "essentially doubles the disk cost for the TPC
application", because the Account table already needed 50 arms for its own 2,000
I/Os per second (Example 1.1).

So the honest 1995 figure is **~20 index inserts per second per disk arm**
(40 I/Os ÷ 2 I/Os per insert), not the "few hundred" that gets quoted from
memory. The same arm streams a 64-page multi-page block in 95 ms — 9.5 ms seek,
5.5 ms rotational delay, 80 ms transfer — which §3.1 works out to **about 1.5
ms/page, so COST_π/COST_P ≈ 1/10**. That ten-to-one gap between "a page reached
by seeking" and "a page reached as part of a big block" is the topic 0 ladder in
1995 dollars, and it is factor one of two in the paper's headline result.

Note the second waste, which the paper's 100%-full C₁ pages later attack: a
4,096-byte page is read and rewritten to change one 16-byte entry — a
**256:1 byte-level write amplification** before any merging happens at all.

### Step 2 — the idea: buffer in memory, flush sorted runs sequentially

> **In:** Step 1's diagnosis — the cost is *where* and *when* bytes hit disk,
> not how many bytes there are.
> **Out:** the two-component structure (C₀, C₁) and the write-ahead log, and
> what each one is for.

Instead of updating disk in place, collect inserts in a sorted **in-memory**
tree — the paper's **C₀ component** — and let them migrate out later to the
disk-resident **C₁ component**. §2 is explicit that C₀ need not be a B-tree at
all: "the nodes could be any size: there is no need to insist on disk page size
nodes since the C₀ tree never sits on disk", and it names a (2-3) tree or an
AVL-tree as candidates. Its modern descendant is the **memtable** (usually a
skiplist).

Durability comes from the **write-ahead log** (WAL): an append-only file every
insert is written to before it is acknowledged. §2 puts it first — "a log record
to recover this insert is first written to the sequential log file in the usual
way" — and the crucial property is that the log is *sequential*, so it does not
reintroduce Step 1's random-seek problem. §4.2 goes further and points out the
LSM does not even need its own index log: the ordinary transactional insert
records already contain every field plus the row's RID, so index entries can be
reconstructed from them.

The whole 1996 idea fits in one loop — defer, batch, write sequentially, and pay
for it at read time:

```rust
// ILLUSTRATION — pseudocode for the paper's §2 two-component algorithm.
// Not from a repo; the real thing in this repo is
// topics/01-storage-engine-landscape/experiments/src/main.rs:1 (fjall lane).
1  fn insert(&mut self, k: Key, v: Val) {
2      self.wal.append(&k, &v);      // §2: sequential log record goes first
3      self.c0.insert(k, v);         // C0: sorted tree in RAM, "no I/O cost" (§2)
4      if self.c0.bytes() > THRESHOLD {
5          // §2 rolling merge: drain a contiguous key range of C0 into C1.
6          // C1 leaves are packed 100% full and written to a NEW disk position.
7          rolling_merge(&mut self.c0, &mut self.c1);
8      }
9  }
10
11 fn get(&self, k: &Key) -> Option<Val> {
12     // §2: "any search for an index entry will look first in C0 and then in C1"
13     self.c0.get(k).or_else(|| self.c1.get(k))
14 }
```

Line 3 is where the speedup lives — §2: "the operation of inserting an index
entry into the memory resident C₀ tree has no I/O cost." Line 13 is the bill,
and it is Steps 3 and 4.

### Step 3 — the rolling merge: what keeps reads bounded

> **In:** a C₀ that fills up, and a C₁ that must absorb it without seeking.
> **Out:** the paper's emptying-block / filling-block mechanism, the two
> properties it buys (100%-full pages, new disk positions), and the batching
> parameter M that those properties make possible.

Flushing sorted runs and leaving them alone would litter the disk with thousands
of files, and a lookup would have to check every one — so the engine
continuously **merges** newly arrived data into the larger component. §2 calls
this the **rolling merge**, and describes it as a cursor that cycles through C₁
in key order forever: "subsequent merge steps bring together increasing index
value segments of the C₀ and C₁ components until the maximum values are reached
and the rolling merge starts again from the smallest values."

Mechanically (§2), a merge step uses two buffers:

- the **emptying block** — a multi-page block of *old* C₁ leaves read in from
  disk. §2 envisions multi-page blocks of **256 KBytes**;
- the **filling block** — a multi-page block of *newly merged* C₁ leaves being
  built, written out when full.

Two properties of that loop matter more than the mechanism:

1. **C₁ nodes are 100% full.** §2: the C₁ tree "has a comparable directory
   structure to a B-tree, but is optimized for sequential disk access, with
   nodes 100% full". Compare Comer's B-tree, whose expected utilisation is
   ln 2 ≈ 69% — that difference alone is a 1/0.69 = 1.45× space saving.
2. **Merged blocks go to *new* disk positions.** §2: "newly merged blocks are
   written to new disk positions, so that the old blocks will not be overwritten
   and will be available for recovery in case of a crash." This is copy-on-write
   at the block level, and §2 credits the inspiration to Rosenblum and
   Ousterhout's Log-Structured File System. It is *not* an in-place rewrite —
   a detail that matters when you meet immutable SSTs in Step 5, because that
   part of modern LSM design was already here in 1996.

Property 1 plus the delay is what creates the paper's second batching factor.
**Definition 3.2.1** names it: **M**, "the average number of entries in the C₀
tree inserted into each single page leaf node of the C₁ tree during the rolling
merge", and equation (3.2) computes it:

```text
(3.2)   M = (Sp / Se) · ( S0 / (S0 + S1) )

  Sp = page size in bytes           S0 = size of the C0 leaf level
  Se = index entry size in bytes    S1 = size of the C1 leaf level

paper's own worked case, §3 opening:
  Se = 16 B, Sp = 4 KB   ⇒  Sp/Se ≈ 250 entries per fully packed node
  S0 = S1/25             ⇒  M ≈ 250 / 25   ≈ 10 entries merged per C1 leaf

paper's §3.2 worked case (Definition 3.2.1):
  Sp/Se = 200, S1 = 40·S0 ⇒  M = 200 · 1/41 = 4.88  ≈ 5
```

Both are the paper's numbers, one paragraph apart, and the difference between
them is the whole design knob: M is set by how big you are willing to make the
memory component.

### Step 4 — naming the price: read, write, and space amplification

> **In:** an engine that batches writes and merges repeatedly.
> **Out:** three ratios, each defined so you could compute it from a directory
> listing, and this repo's measured value for one of them.

The trade has standard modern names. Each is a ratio, so each needs a numerator
and a denominator stated:

- **Write amplification** = bytes physically written to the device ÷ bytes of
  user data inserted. Every merge rewrites entries that were already on disk, so
  one logical insert is physically written many times over its lifetime.
- **Read amplification** = pages (or components) consulted per lookup ÷ the one
  page that actually holds the answer. §2 states the LSM's version plainly:
  "any search for an index entry will look first in C₀ and then in C₁."
- **Space amplification** = bytes occupied on the device ÷ bytes of live user
  data. Overwritten and deleted entries linger in older components until a merge
  drops them.

Two of the three have a paper number and a repo number.

*Space amp, paper.* §3.1's Example 3.1 prices the B-tree side: the 20-day index
"requires about 9.2 GBytes of leaf-level entries. Given that a growing tree is
only about 70% full, the entire tree will require 13.8 GBytes" — space amp
13.8/9.2 = **1.5×** for a B-tree under this insert pattern. The LSM side is
Example 3.2's "0.7 GBytes on disk because of closely packed entries", against a
1 GByte B-tree — space amp **0.7×**, below one.

*Space amp, measured here.* [FINDINGS.md](../../FINDINGS.md) row 1 is this
topic's own version of exactly that comparison, on the same 1.08 M records of
100 bytes:

| engine | family | logical | on disk | space amp |
|---|---|---|---|---|
| fjall | LSM | 108.0 MB | 48.4 MB | **0.45×** |
| redb | B-tree (CoW) | 108.0 MB | 6833.9 MB | **63.28×** |

A **140× spread**, and the LSM's figure is below 1.0 — for the same reason the
paper's is: closely packed runs (plus, in fjall's case, LZ4 on the value bytes),
paid for with read cost. redb's 63× is not a defect either; `notes.md` explains
it as a copy-on-write B-tree meeting its adversarial case — random key order,
1,080 separate durable batch commits, no compaction afterwards, so every commit
copies each page on the root-to-leaf path and cannot free the old ones yet. Use
those two numbers, not remembered ones, whenever this topic needs a figure for
"LSM vs B-tree space".

An LSM buys its insert speedup by moving cost *into* read and space
amplification; a B-tree makes the opposite trade. That three-way tension is the
RUM conjecture chapter, verbatim.

### Step 5 — from rolling merge to leveled compaction, and the write-amp formula

> **In:** the 1996 mechanism, and the modern vocabulary you already have.
> **Out:** a term-by-term translation table, and §3.4's Theorem 3.1 worked on
> a concrete leveled geometry.

The paper's merge is a *cursor* over a single, always-valid C₁ B-tree. Modern
LSMs dropped the cursor but kept the write-to-a-new-place discipline: they write
**immutable sorted files** (SSTs) and compact by merging whole files into new
files, then deleting the inputs. Translate as you read:

```text
paper (1996)                         modern (LevelDB lineage)
─────────────                        ────────────────────────
C0 in-memory (2-3)/AVL tree    →     memtable (skiplist)
C1 ... CK on-disk B-trees      →     levels L1 ... LK of immutable SSTs
rolling merge cursor           →     compaction job
emptying / filling blocks      →     compaction input / output files
multi-page block, 256 KB (§2)  →     SST data block + readahead
C1 nodes packed 100% full (§2) →     SST blocks, sequentially written
size ratio r  (§3.4)           →     size ratio / fanout T (usually 10)
number of disk components K    →     number of levels L
```

§3.4 is the part worth doing on paper, because it *is* modern leveled
compaction's write-amplification formula, derived thirty years early.
Symbols (§3.4):

- `S_i` = bytes of leaf-level entries in component `C_i`; `S = Σ S_i`
- `r_i = S_i / S_{i-1}` = the size ratio between adjacent components
- `R` = steady insert rate into `C₀`, in bytes per second
- `K` = number of *disk*-resident components (so `K+1` components in all)
- `S_p` = page size in bytes; `H` = total page I/O rate needed for all merges

**Theorem 3.1**: with `S_K`, `S₀` and `R` fixed, `H` is minimised when all the
`r_i` are equal to one constant `r` — i.e. size the components in a *geometric
progression*. Then

```text
(3.5)   S = S0 · (1 + r + r² + ... + r^K)
(3.6)   H = (2R / Sp) · ( K·(1 + r) − 1/2 )
```

Equation (3.6) comes straight out of the proof's per-level accounting, and that
accounting *is* the write-amp derivation. For one merge of `C_{i-1}` into `C_i`,
in pages per second:

```text
  read  from C_{i-1}      R/Sp          (the entries migrating out)
  read  from C_i        r·R/Sp          (the cursor passes r× as many C_i pages)
  write to  C_i     (r+1)·R/Sp          (both inputs land in the enlarged C_i)
  ───────────────────────────────
  per level          (2r+2)·R/Sp
  over K levels    K·(2r+2)·R/Sp  =  (2R/Sp)·K·(1+r)
  minus the C0 read, which is free (C0 is in memory):  −(1/2)·(2R/Sp)
  ⇒ (3.6)
```

So, reading the write line only:

> **write amplification = K · (r + 1)**, and total I/O amplification (reads plus
> writes) = 2·(K·(1+r) − ½).

Work it on the geometry the modern default describes — size ratio `T = r = 10`,
four disk levels `K = 4`:

```text
  write amp   = K·(r+1)          = 4 × 11          =  44×
  the usual textbook shorthand   = T × L = 10 × 4  =  40×   (drops the "+1")
  total I/O amp = 2·(K·(1+r) − ½) = 2·(44 − 0.5)   =  87×

  sizing, from (3.5), with S0 = 10 MB:
    S1..S4 = 100 MB, 1 GB, 10 GB, 100 GB
    S      = 10 MB × (1+10+100+1000+10000) = 10 MB × 11,111 = 111 GB
  and inverting: r = (S_K/S0)^(1/K) = (100 GB / 10 MB)^(1/4) = 10000^(1/4) = 10
```

44× write amp for a 111 GB index is the price of the insert speedup, and it is
why topic 4 spends its time on the `T`/`K` choice rather than on the merge code.
Note the shape of the tradeoff in (3.6): raising `r` makes each merge more
expensive but reduces `K` (since `K = log_r(S_K/S₀)`), so `H` is a genuine
minimisation problem, not a monotone knob. That derivation is what Monkey and
Dostoevsky later reopened (topic 4).

The paper is also blunt about when the LSM *loses*. §3.3: if `M < K₁ ·
COST_π/COST_P` — which happens when C₀ is tiny relative to C₁, or entries are so
large that few fit per page — "this could even cancel the batching effect of
multi-page disk reads, so we would do better to use a normal B-tree for
inserts". There is no LSM-always-wins claim anywhere in the paper.

### Step 6 — the punchline: an IO scheduling policy, not a data structure

> **In:** everything above.
> **Out:** equation (3.4) as the formal statement of the title claim, and
> Definition 5.1 as the reason the claim survives hardware generations.

Strip the mechanism away and nothing about the *data* changed — same entries,
same collation order, same queries. The only thing the LSM changed is **when,
and in what order, bytes reach the disk.** §3.2 makes that precise. Against the
B-tree baseline of equation (3.1), `COST_B-ins = COST_P · (D_e + 1)` — where
`D_e` is the *effective depth*, "the average number of pages not found in buffer
during a random key-value search", typically **2** for Example 1.2's index — the
LSM's amortised insert cost is equation (3.3), `COST_LSM-ins = 2·COST_π / M`,
and the ratio is:

```text
(3.4)  COST_LSM-ins / COST_B-ins  =  K1 · (COST_π / COST_P) · (1 / M)

       K1 = 2/(De + 1) ≈ 2/3 ≈ 0.67     (§3.2, for De ≈ 2)

worked with the paper's own §3.2 values:
       COST_π/COST_P = 1/10   (§3.1: 1.5 ms/page in a 64-page block
                               vs a full random access)
       M             = 5      (Sp/Se = 200, S1 = 40·S0)
       ratio = 0.67 × 0.1 × 0.2 = 0.0134 ≈ 1/75
```

which is §3.2's "nearly two orders of magnitude". Read the two factors: neither
is a property of the *data structure*. `COST_π/COST_P` is a property of how the
I/O is *issued* (one big block versus many small seeks); `1/M` is a property of
how long you *wait* before issuing it. Both are scheduling decisions.

§5 gives the same claim its cleanest form. **Definition 5.1** calls an access
method a **Continuum Structure** if it "provides for immediate placement of a
newly inserted index entry in its ultimate collation order, based on key-value,
with all other entries already present" — and then observes that B-trees,
extendible hashing, and Bounded Disorder files are all Continuum Structures, so
all of them pay Step 1's random-page cost, and none of them can escape it by
being cleverer about layout. The LSM's one novelty is that it is *not* one: §1
calls it "a cascaded series of deferred placements".

That is why "LSM vs B-tree" survives every hardware generation. The constants
move — COST_π/COST_P was 1/10 on a 1995 SCSI-2 disk and is a different number on
NVMe — but the policy question, *how long do I defer placement and how big a
batch do I place*, does not.

## How to read the paper (with the concepts in hand)

The paper's own plan is at the end of §1. Its real section map — worth having
open, because the numbering is easy to misremember:

| § | Title | What it actually contains |
|---|---|---|
| 1 | Introduction | The Five Minute Rule; Examples 1.1 and 1.2 (TPC-A) |
| 2 | The Two Component LSM-Tree Algorithm | C₀/C₁, rolling merge, §2.1 growth |
| 3 | Cost-Performance and the Multi-Component LSM-Tree | 3.1 disk model, 3.2 equations 3.1–3.4, 3.3 multi-component, 3.4 Theorems 3.1/3.2 |
| 4 | Concurrency and Recovery in the LSM-tree | 4.1 concurrency, 4.2 checkpoint/recovery |
| 5 | Cost-Performance Comparisons with Other Access Methods | Definition 5.1; TSB-tree, MD/OD R-tree, Bounded Disorder |
| 6 | Conclusions and Suggested Extensions | Figure 6.1, the cold/warm/hot cost graph |

Read in this order:

1. **§1, including The Five Minute Rule** — the economic argument. Read the rule
   carefully: by 1995 the paper restates it as **60 seconds**, not five minutes
   ("the reason it is smaller now in 1995 than when defined in 1987"), and §3.1
   re-derives τ ≈ 62.5 seconds from its own cost table. LSM works because
   *recent* data is hot by construction — Step 2's C₀ is exactly the hot set.
2. **§2 (two-component LSM)** — Steps 2–3 in the authors' words. Keep Step 5's
   translation table open and convert every term as you read.
3. **§3.1–3.2 (the disk model and the four equations)** — the payoff, Step 6.
   §3.1's cost table (COST_m = $100/MB, COST_d = $1/MB, COST_P = $25 per IO/s,
   COST_π = $2.5 per IO/s, 1995 workstation) is what makes equation (3.4)
   numeric. Work it until "amortised block I/Os per insert" feels obvious.
4. **§3.3–3.4 (multi-component, Theorem 3.1)** — do not skim this; it is Step
   5's write-amp arithmetic and the direct ancestor of leveled compaction.
   Equation (3.6) is the one to be able to re-derive.
5. **§4 (concurrency and recovery)** — skim. Most of this machinery is what
   immutable SSTs made obsolete; §4.2's checkpoint scheme is the interesting
   survivor.
6. **§5 (comparisons)** — read Definition 5.1 and skip the rest. The competitors
   (TSB-tree, MD/OD R-tree, Bounded Disorder files) are dead; the framing —
   "is this structure a Continuum Structure?" — is the durable idea.

## Questions to answer in notes.md

1. §1 Example 1.2 gets 2.3 million leaf pages from 1,000 inserts/second. Redo
   the arithmetic with 2026 numbers — same entry size, an NVMe device at
   500,000 IOPS instead of 40 — and say whether Example 1.2's conclusion
   ("essentially doubles the disk cost") still follows. Which of the paper's
   two batching factors survives, and which collapses?
2. Equation (3.2) gives `M = (Sp/Se)·(S0/(S0+S1))`. The paper works two cases
   with different answers (M ≈ 10 and M = 5). Reconcile them: which parameter
   differs, and what does that tell you about which knob an engine actually
   controls at run time?
3. §2 says merged blocks are written to *new* disk positions. Modern LSMs use
   immutable SSTs. Name one thing modern engines gained by going further than
   the paper (whole files immutable, not just blocks relocated) and one thing
   they gave up. Point at the §4.2 machinery that the change made unnecessary.
4. Use Theorem 3.1 to derive the write amplification for a tiered geometry
   instead of leveled: if each level holds `r` *separate* runs that are merged
   only when the level is full, what happens to the `(r+1)` term? Which
   amplification moves in the other direction, and by how much?
5. §3.3 states the condition under which a plain B-tree beats an LSM for
   inserts. Write it out, substitute this repo's numbers where you can, and
   name one real workload in which it holds.

## The one-line takeaway

LSM is not a data structure, it's an *IO scheduling policy*: defer placement and
batch it, so writes leave the machine in block-sized, sequential units — and pay
for it at read time, in a currency equation (3.4) prices exactly.

## Done when

Answer each before unfolding it.

- [ ] You can state, in terms of what the disk arm is asked to do, why random in-place writes are the problem the paper is solving — with the paper's own per-insert I/O count.

<details>
<summary>Answer</summary>

An in-place index must put each new entry in its final collation position
immediately. With random keys that position is on an unpredictable one of the
index's 2.3 million leaf pages (§1, Example 1.2), so each insert costs one
random read plus, in the steady state, one random write of a dirty page —
equation (3.1), `COST_B-ins = COST_P·(D_e + 1)` with `D_e ≈ 2`. At 1,000
inserts/second that is 2,000 random I/Os per second, and at the 1995 nominal
rate of 40 usable I/Os per disk arm per second, 50 extra arms — which §1 says
doubles the disk cost of the whole TPC application. The CPU is never the
limit; the arm is.

</details>

- [ ] You can define read, write and space amplification precisely enough to compute each one, and quote this repo's measured space-amp figures rather than a remembered ratio.

<details>
<summary>Answer</summary>

Each is a ratio with a stated denominator: write amp = bytes physically written
to the device ÷ bytes of user data inserted; read amp = pages (or components)
consulted per lookup ÷ the one that holds the answer; space amp = bytes occupied
on the device ÷ bytes of live user data.

The measured figures for this topic are in
[FINDINGS.md](../../FINDINGS.md) row 1: on the same 108.0 MB of records, fjall
(LSM) occupies 48.4 MB — space amp **0.45×** — and redb (CoW B-tree) occupies
6,833.9 MB — space amp **63.28×**. A 140× spread. Below 1.0 is not a paradox:
the LSM packs runs densely and compresses values, spending read cost to buy
space, which is the paper's own Example 3.2 result (0.7 GBytes for a 1 GByte
B-tree) with a 2026 compressor attached.

</details>

- [ ] You can write equation (3.4) from memory, name every symbol in it, and put a number on each factor.

<details>
<summary>Answer</summary>

`COST_LSM-ins / COST_B-ins = K₁ · (COST_π/COST_P) · (1/M)`.

- `COST_P` — disk-arm cost to provide 1 page/second of *random* I/O; `COST_π` —
  the same for a page read as part of a multi-page block. §3.1 measures the
  ratio at ≈ **1/10** (a 64-page block costs 9.5 ms seek + 5.5 ms rotation +
  80 ms transfer = 95 ms, about 1.5 ms/page).
- `M` — Definition 3.2.1, the average number of C₀ entries merged into each
  single-page C₁ leaf. Equation (3.2): `M = (Sp/Se)·(S0/(S0+S1))`. §3.2's case:
  `Sp/Se = 200`, `S1 = 40·S0` ⇒ **M = 5**.
- `K₁ = 2/(D_e + 1)`, with `D_e` the effective B-tree depth ≈ 2 ⇒ **0.67**.

Product: 0.67 × 0.1 × 0.2 = 0.0134 ≈ 1/75, which §3.2 rounds to "nearly two
orders of magnitude".

</details>

- [ ] You can derive the multi-component write amplification from Theorem 3.1 and evaluate it for T = 10 over 4 levels.

<details>
<summary>Answer</summary>

Theorem 3.1's per-level accounting: merging `C_{i-1}` into `C_i` reads `R/Sp`
pages/second from `C_{i-1}`, reads `r·R/Sp` from `C_i` (the cursor crosses `r`
times as many `C_i` pages), and writes `(r+1)·R/Sp` to the enlarged `C_i`. The
write line alone gives **write amp = K·(r+1)**; summing all three lines over `K`
levels and subtracting the free in-memory `C₀` read gives equation (3.6),
`H = (2R/Sp)·(K·(1+r) − 1/2)`.

For `r = T = 10`, `K = 4`: write amp = 4 × 11 = **44×** (the usual shorthand
`T × L` = 40× drops the `+1`), and total read+write I/O amplification =
2 × (44 − 0.5) = **87×**. Equation (3.5) sizes it: with `S₀ = 10 MB` the
components are 100 MB, 1 GB, 10 GB, 100 GB and `S = 10 MB × 11,111 = 111 GB`;
inverting, `r = (100 GB / 10 MB)^(1/4) = 10`.

</details>

- [ ] You can defend the title claim against the obvious objection that C₀ and C₁ are clearly data structures.

<details>
<summary>Answer</summary>

Both factors in equation (3.4) are scheduling properties, not structural ones:
`COST_π/COST_P` is about *how* the I/O is issued (one 256 KB block versus many
seeks) and `1/M` is about *how long you wait* before issuing it. Neither changes
the entries, their collation order, or the queries. §5's Definition 5.1 is the
sharp version: a **Continuum Structure** places each new entry in its ultimate
collation order immediately, and B-trees, extendible hashing and Bounded
Disorder files all are one — so all pay the same random-placement cost
regardless of how their nodes are shaped. The LSM's single novelty is being
*not* one; §1 calls it "a cascaded series of deferred placements". C₀ and C₁ are
indeed data structures, but they are the *implementation* of a deferral policy,
and §3.3 proves the point from the other side by giving the condition
(`M < K₁·COST_π/COST_P`) under which the same structures lose to a plain B-tree.

</details>

- [ ] You wrote answers to all five questions in notes.md, and can connect the paper's rolling merge to the leveled/tiered choice topic 4 asks you to implement.

<details>
<summary>Answer</summary>

The link is Theorem 3.1. The paper proves that, for a fixed largest component,
the I/O rate is minimised when the size ratios `r_i` are all equal — a geometric
progression, which is exactly leveled compaction's fanout. Topic 4's leveled
implementation is Theorem 3.1's optimum; its tiered implementation is what you
get when you relax the "one sorted run per component" assumption and let a level
hold several runs, trading write amplification down for read and space
amplification up. §3.4's closing note that `K+1` is "the only remaining free
variable" is topic 4's level-count knob, and the optimal-`r` derivation is what
Monkey and Dostoevsky reopened with a per-level Bloom-filter budget.

</details>

## References

**Papers**
- O'Neil, Cheng, Gawlick, O'Neil — "The Log-Structured Merge-Tree (LSM-Tree)"
  (Acta Informatica 33(4), 1996) —
  [PDF](https://www.cs.umb.edu/~poneil/lsmtree.pdf) — §1 for Examples 1.1/1.2
  and the Five Minute Rule; §2 for C₀/C₁ and the rolling merge; §3.1–3.2 for the
  disk model and equations (3.1)–(3.4); §3.4 for Theorem 3.1 and equations
  (3.5)–(3.6); §5 for Definition 5.1 (Continuum Structure)
- Gray, Putzolu — "The Five Minute Rule for Trading Memory for Disk Accesses"
  (SIGMOD 1987) — reference [13] of the LSM paper, the source of the rule §1
  restates at 60 seconds
- Rosenblum, Ousterhout — "The Design and Implementation of a Log-Structured
  File System" (SOSP 1991) — §2 credits it for the write-to-new-locations idea

**This repo**
- [FINDINGS.md](../../FINDINGS.md) row 1 — the measured space-amp comparison
  (fjall 0.45×, redb 63.28×) this guide cites instead of a borrowed figure;
  re-derive with `./verify.sh 01`
- [notes.md](notes.md) — the baseline table and the explanation of why redb's
  63× is the adversarial case rather than a defect
- [reading-rum-conjecture.md](reading-rum-conjecture.md) — the three-way
  formulation of Step 4's amplifications
