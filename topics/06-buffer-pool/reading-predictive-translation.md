# Predictive translation: hide the lookup instead of deleting it

LeanStore and vmcache (the previous chapter, [`reading-leanstore-paper.md`](reading-leanstore-paper.md))
answer the translation problem by *deleting* the hash table: a swizzled pointer
in the parent, or the MMU's page table. Both work, and both leave a bill —
swizzling forbids a page having two referents, vmcache needs a kernel module
and one metadata record per page *on the SSD*, not per page in RAM. This
chapter reads the 2026 counter-proposal, which keeps the hash table and asks a
different question: not "how do I avoid the lookup?" but "what can the CPU do
*while* the lookup is in flight?". It builds the idea in order — what a
dependent chain of loads costs, what a classic hash-table pool does on the hot
path, the trick of reading a *guessed* frame in parallel with the lookup, what
keeps the guess right, what makes the lookup cheap enough to be worth
overlapping — then what the paper measured, and a micro-lane in this repo that
tests the overlap argument on its own.

Section and figure numbers are Zinsmeister, Nguyen, Leis, Neumann,
*"Predictive Translation: High-Performance Buffer Management Without the
Trade-Offs"*, SIGMOD 2026 (16 pages, [PDF](https://www-db.cs.tum.edu/~zinsmeister/papers/predictive-translation.pdf),
DOI 10.1145/3786678). The system they build is called **PrediCache**; their code
is not one of the repositories this book pins, so there are no source anchors
into it — the only code quoted here is this repo's own
`topics/06-buffer-pool/experiments/src/bin/translation_micro.rs`. Every number
carries the section, figure or table it came from, or says it was computed here.

## The problem in one sentence

A hash-table buffer pool reaches a page through a chain of *dependent* loads
(bucket, then control block, then frame), so a hit costs several cache misses
back to back where a swizzled pointer costs one — and the paper's claim is that
if the frame address can be *guessed* from the page id, the CPU can issue the
page read at the guess while the hash lookup is still resolving, so the lookup's
latency hides behind the read it would otherwise have preceded, and a hit costs
about one miss again without a swizzled pointer, a kernel module, or a metadata
record per page on disk.

## The concepts, step by step

### Step 1 — a buffer pool's hot path is a chain of dependent loads

> **In:** the vocabulary of the previous chapter (page id, frame, latch, hash
> translation) and nothing else.
> **Out:** the one hardware fact the whole paper leans on — **dependent loads
> serialize, independent loads overlap** — and a way to count it.

A **load** here means one memory read. A **cache miss** is a load the CPU's
caches cannot answer, so it waits on DRAM, about 100 ns on a modern machine
(topic 0's ladder). A **superscalar** CPU is one that issues several
instructions per cycle and keeps many in flight at once, *out of order*: it does
not wait for a slow instruction if a later one does not need its result.

The paper's §2.3 shows the point in two snippets. In the first, the second load
needs the first load's result; in the second, the two loads are unrelated:

```text
// ILLUSTRATION — the two snippets of §2.3, rewritten; not code from the paper's system.
// dependent                        // independent
 1  p2    = *p1;                    1  data1 = *p1;
 2  data  = *p2;                    2  data2 = *p2;
```

If both loads miss the cache, the dependent version cannot start line 2 until
line 1 returns, so it takes about 2 × L, where L is one miss latency; the
independent version issues both at once and takes about L. "This dependency can
nearly double the execution time" (§2.3). Worked with a round number: at
L = 100 ns, two dependent misses cost 200 ns and two independent ones about
100 ns. This is also why a *chain* of n dependent misses cannot be sped up by
any cleverness except removing links from the chain or starting a link early.

Everything below is about the chain at the front of a page access.

### Step 2 — what a classic hash-table pool does on a hit

> **In:** Step 1's chain-length arithmetic.
> **Out:** a count of dependent loads for the traditional design (the paper's
> baseline), and the two things the paper blames: the *bucket latch* and the
> *indirection depth*.

Take the buffer manager of **Shore-MT** (a research storage manager the paper
uses as the representative traditional design, §2.4; it says PostgreSQL's and
InnoDB's are very similar). A page request does this (§2.4, Figure 2, §4.1):

1. Hash the page id to a **bucket**. Shore-MT uses cuckoo hashing, so there are
   two candidate buckets, probed one after the other, each under a **bucket
   latch** — a short lock on that bucket's chain.
2. Inside the bucket, walk to the **translation slot** for the page id.
3. Follow the slot to the **frame header** (the page's control block: state,
   page latch, the frame's address).
4. Follow the header's pointer to the **frame** and read the page.

Steps 1→2→3→4 are four loads, each needing the previous one's result. The paper
counts "the hot path… involves (1) numerous latches… and (2) multiple dependent
memory accesses" (§2.4, Summary) and, in its own baseline implementation
("traditional", §6), three pointer dereferences — "hash table bucket, buffer
frame header, and buffer frame". It also adds a second cost that has nothing to
do with dependency: the bucket latch is a *write* even for a read-only lookup,
so two cores reading the same hot page bounce the latch's cache line between
them. A **reader-writer latch** does not fix it, because acquiring even the
shared side is still a write; the paper finds that this "still suffers from
significant scalability issue" (§4.2) and measures a 7.4× difference at 192
threads from replacing it (Step 7).

Compare the alternatives the paper lists (Table 1), as the paper rates them:

| design | # pages | metadata | performance | cyclic refs ("graphs") | implementation |
|---|---|---|---|---|---|
| traditional hash table | high | low | low | yes | easy |
| pointer swizzling | high | low | high | **no** | hard |
| LIPAH | **low** | low | high | yes | medium |
| vmcache | medium | **high** | high | yes | medium (kernel module) |
| predictive translation | high | low | high | yes | easy |

The last row is the claim; the rest of the chapter checks whether it is earned.

### Step 3 — the three modern designs, and what each costs

> **In:** Step 2's table.
> **Out:** the five design goals (§2.2) the paper grades everything against, and
> the exact objection to each rival — the objection to swizzling and to
> vmcache are the two this repo's topic-6 README now states in terms of them.

The paper names five goals (§2.2): **(1) high performance** (a translation costs
about one cache miss at most), **(2) low memory overhead** (metadata
proportional to the *cache* size, not the storage size), **(3) portability** (no
kernel modification, no invasive changes to indexes), **(4) multiple page
references** (cyclic references work), **(5) an unlimited number of pages**.
Three rivals, each failing one:

- **Pointer swizzling** (LeanStore, WiredTiger's cache) stores the frame's
  address *in the parent*. §1 and §2.1 give two costs. It "restricts each page
  to a single reference", which rules out "graph-like data structures" and also
  "B+tree scan operations involving sibling pointers" — a leaf cannot be
  pointed to by both its parent and its neighbour. And evicting a page "involves
  exclusively latching both the to-be-evicted page and its parent node".
- **LIPAH** (Logical ID with Physical Address Hinting) stores a *fat pointer*:
  the page id **and** a guessed frame address. If the guess is stale the pool
  notices and falls back to a hash lookup, which removes the single-reference
  restriction. Its limit is the 4-byte page id: 2³² pages × 4 KiB = 17 592 186
  044 416 bytes ≈ 17.6 TB, "far smaller than other approaches and insufficient
  for datacenter-grade NVMe SSDs" (§2.1; the product is computed here), and it
  still needs the index code changed to carry the fat pointer.
- **vmcache** makes the MMU the translation table: page `p` lives at virtual
  address `base + p × 4 KiB`, always. Its costs (§2.1): the good-performance
  variant needs a **kernel module** (exmap), which must track kernel versions
  and, for the paper's own runs, required disabling Secure Boot and Supervisor
  Mode Access Prevention (footnote 1); and it needs **one frame header per
  *storage* page**, because the virtual range covers the whole device. The
  paper's arithmetic: "with a modern enterprise SSD of 30TB… approximately 8
  billion 4KB pages. Assuming the frame header is 64 bytes… 480GB of memory
  overhead". Recomputed here with decimal terabytes: 30 × 10¹² / 4096 ≈ 7.32
  billion pages × 64 B ≈ 469 GB; with binary terabytes (30 × 2⁴⁰) it is 515 GB;
  the paper's 480 GB sits between, and the conclusion — hundreds of GB of
  metadata for a 30 TB drive — does not depend on which. It also forecloses
  *sampling-based eviction* (WATT, HyperBolic), which need per-page statistics
  next to the page.

Predictive translation takes the unremarkable design (hash table: goals 2–5
satisfied) and attacks only goal 1.

### Step 4 — the trick: read a guessed frame beside the lookup

> **In:** Step 1 (dependent vs independent loads) and Step 2 (the lookup is the
> front of the chain).
> **Out:** the **preferred frame** and the three-line fast path (Listing 2),
> with the exact condition under which the guess is allowed to be used.

Give every page id `pid` a **preferred frame**, a fixed slot in the buffer
memory computed from the id alone: "we use a hash function on the PID and apply
a modulo operation with the buffer pool size" (§3.1) — with the division
replaced by a multiplication by precomputed "magic numbers", since integer
division is slow. If the pool has 10 frames, pids 1, 11, 21… all share frame 1
in the identity-hash example the paper uses (§3.2). The page *may or may not*
currently live in its preferred frame; the pool still keeps its hash table
saying where it really is.

The fast path then issues both reads at once. The paper's Listing 2 (§3.1),
line numbers as printed there:

```text
 1  BufferFrame* correct_bf = hashtable.lookup(pid)         // 1 cache miss
 2  // predicting page is in its preferred position
 3  BufferFrame* pred_bf = getPreferredBufferFrame(pid)
 4  if (isPreferredPosition(correct_bf)):   // rely on CPU branch predictor
 5      read(pred_bf)                       // interleave with 1  <- superscalar exec.
 6  else:
 7      read(correct_bf)                    // (rare) slow path
```

Line 4 is the line to focus on. `getPreferredBufferFrame` (line 3) needs only
the pid — no memory access — so `pred_bf` is known immediately. The branch on
line 4 depends on the lookup's result, which is still in flight. A CPU does not
wait for a branch condition: its **branch predictor** guesses the outcome and
the core runs ahead down the guessed path (**speculative execution**),
discarding the work if the guess turns out wrong. If the predictor guesses
"preferred" — and for a hot page it will, because the branch is almost always
taken — the core issues `read(pred_bf)` on line 5 *while line 1's miss is still
outstanding*. The two misses now overlap, and "the perceived latency [is] that of
a single page read" (§3.1). When the lookup returns and confirms, the speculated
read is kept; when it denies (the page is not in its preferred frame), the CPU
flushes the wrong path and executes line 7, paying a branch misprediction plus
the lookup it was waiting for anyway. The paper's argument for why that is
cheap: on a misprediction "the thread would typically be stalled waiting for the
hash table translation anyway" (§3.1).

Note what is *not* happening. Nothing in the code says "do these in parallel";
"the parallel execution… is not triggered explicitly anywhere in the code. The
introduction of the branch… simply gives the CPU the opportunity to speculatively
execute the page read" (§5.2). The technique is a code *shape*, and the micro-lane
below exists because a compiler can quietly destroy that shape (Step 8).

### Step 5 — keeping the guess right: promotion and demotion

> **In:** Step 4's preferred frame.
> **Out:** the page lifecycle that makes hot pages sit in their preferred
> frames, with its two probabilities.

The guess is only useful if hot pages are in their preferred frames. Many page
ids share one preferred frame, so most pages cannot be — only one can. The
paper's observation (§3.2) is that transactional workloads separate hot from
cold, and much of the cold data is **one-hit wonders**, pages touched once or
rarely, which need no fast path. The lifecycle (§3.2):

1. **Load.** A page read from storage goes into an arbitrary free frame, *unless*
   its preferred frame happens to be free — assume it is a one-hit wonder.
2. **Promote.** On a later access, if the page is not in its preferred frame, move
   it there *probabilistically*.
3. **Demote.** If another page occupies the preferred frame, the incumbent is
   moved out to a free frame, but with a much smaller probability, so two hot
   pages that share a frame do not thrash.
4. **Evict** from either position as usual.

The probabilities are given in §5.1: "1/32 if the promotion would not lead to a
demotion, 1/512 otherwise". Nothing tracks hotness; chance does it: a page
accessed often is promoted soon, because the number of accesses until a 1-in-p
event is geometric with mean p. Worked: for a page whose preferred frame is free,
the expected number of accesses before it is promoted is 32; for a page that
would displace an incumbent it is 512. A page seen 5 times in a run
is promoted with probability 1 − (31/32)⁵ = 1 − 0.8541 = 14.6%, a page seen 500
times is promoted with probability 1 − (31/32)⁵⁰⁰ ≈ 1 − 10⁻⁷ — effectively
certain. A promotion is not free: it takes both pages' exclusive latches, copies
one 4 KiB page into a free frame (demotion) and one into the preferred frame
(Figure 4 of the paper walks it). Amortised over ≥ 32 accesses to a page that
then stays put, that is under 4096/32 = 128 bytes copied per access *before* the
page settles and none after; the measured steady state is below one byte per
access (§6.7: "an amortized cost of less than 1B copied per page access").

How well does it work? §6.8 measures, on a 128 GB buffer pool under uniform
random reads:

| data size | pages in their preferred frame | accesses that hit the predicted frame |
|---|---|---|
| 0.5× pool | 75% | 96% |
| 3× pool (out of memory) | 3% | 91% |

The second row is the surprising one: only 3% of resident pages are in their
preferred frame, yet 91% of *accesses* are served from the predicted frame,
because the pages that matter (B-tree inner nodes) are the ones that got
promoted — the access distribution, not the page count, is what the guess
needs. For the first row, a ceiling is easy to compute (this is **not** in the
paper): put n pages into F frames by a uniform hash and at most one page per
frame can hold the preferred slot, so the expected fraction that can is
(1 − e^(−α))/α with α = n/F. At α = 0.5 that is (1 − 0.6065)/0.5 = 0.787, i.e. a
ceiling near 79%, against the paper's measured 75%.

### Step 6 — making the lookup cheap enough to overlap

> **In:** Step 4's requirement that the lookup be both light and non-blocking.
> **Out:** the hash-table design — chaining, an inlined first slot, optimistic
> latching — and a check of its "chains are short" premise against a formula.

Overlap only helps if the lookup does not itself stall the read. Two properties
(§4): the table operations must be "as lightweight as possible", and "lock-free;
otherwise, the translation step could block the concurrent execution of buffer
frame evaluations". The paper's design:

- **Chaining, sized generously, first slot inlined.** A **chaining** hash table
  keeps colliding entries in a linked chain off the bucket. With the table sized
  "at twice the buffer pool capacity" (§6) the **fill factor** — entries per
  bucket — is 0.5 and most chains have one entry, so the first entry is stored
  *inside* the bucket array (§4.1), and the **frame header is stored inside the
  entry** (§4.1), cutting the Shore-MT chain of Step 2 from four dependent loads
  to the bucket (which holds the header) and, speculatively, the frame.
  Separating headers from frames also avoids padding each frame to 512 bytes for
  direct I/O (§4.1).
- **Optimistic latch.** A **version counter** per bucket (or frame) is read
  before the lookup, the lookup runs *without writing anything*, and the counter
  is read again; if it changed a writer intervened and the reader retries (§4.2; Listing 3 is in §5.2). Readers write nothing, so no cache line ping-pongs and the loads
  are free to overlap. The page states fit in one 64-bit atomic together with the
  version (§5.1).

The premise — chains are short — has a closed form. If entries land in buckets
uniformly, the number in one bucket is approximately Poisson with mean α (the
fill factor); condition on the bucket being non-empty and divide by 1 − e^(−α)
(the "zero-truncated" distribution the paper cites, §6.6). Worked: for chain length
k, P(k) = α^k e^(−α) / (k! (1 − e^(−α))).

```text
 fill factor α   P(len 1)   P(len ≤ 2)   P(len ≤ 3)      paper (§4.2, §6.6, Fig. 10)
     0.50         0.771       0.963         0.996        "77% of the chain lengths are one"
     0.75         0.671       0.923         0.986        "92% of chain lengths do not exceed two"
     1.00         0.582       0.873         0.970        "97% of the chains are not longer than three"
```

Computed here with the formula, each cell matches the figure the paper reads off
its simulation, to the digit it quotes. Hence "a majority of chains comprise only
one slot" is a property of the load, not luck, and the inlined first slot is a
bet that pays almost always.

### Step 7 — what the paper measured

> **In:** Steps 4–6.
> **Out:** the headline numbers, with the machine they were taken on and what the
> ablation attributes to each ingredient.

**Machine and system (§6).** Single-socket AMD EPYC 9645P, 96 cores / 192 hardware
threads, 384 GB RAM, one Kioxia CM7-R PCIe 5.0 NVMe SSD, Linux 6.8. PrediCache
is C++ with a B-tree using optimistic latch coupling, 4 KiB pages, MurmurHash2.
Competitors: LeanStore, vmcache with exmap, WiredTiger, LMDB, and the authors'
own "traditional" pool (reader-writer spin latch per bucket, a very large table
so collisions are almost absent, no predictive placement). Logging is off and
every system runs at its lowest isolation level.

**In memory (§6.1, Figure 5).** Three workloads: TPC-C (200 warehouses, 40 GB),
uniform random lookup and skewed YCSB (θ = 0.9) over 100 M entries, in a
256 GB pool. Uniform random lookup is the design's worst case — every leaf is
equally hot, so the guess is right least often — and there PrediCache, vmcache
and LeanStore "perform similarly". On TPC-C at 192 threads PrediCache is **34%
faster than vmcache and 19% faster than LeanStore**. Three reasons the paper
gives for the vmcache gap: new-page allocation triggers exmap system calls;
vmcache's page-state array packs eight 64-bit states per cache line so
neighbouring pages' updates ping-pong; and huge pages — turning them off in
PrediCache costs about 400 k TX/s, "roughly 7%".

**Out of memory, 1 TB data and a 128 GB pool (§6.2, Figure 6).** The hash-table
designs track the others because I/O dominates, so the "traditional" pool is
competitive here; the paper's claim is parity-or-better, not a win everywhere.
During the in-memory-to-out-of-memory transition (§6.3, Figure 8) it ends **19.7%
above vmcache and 24.6% above LeanStore**.

**Ablation (§6.4, Figure 9),** uniform in-memory lookup, stacking the ideas on the
"traditional" baseline:

| step | effect reported |
|---|---|
| read-write spin latch → optimistic latch | **7.4×** at 192 threads; slightly *worse* at one thread |
| + inline the first slot | **+24.8%** at one thread, **+18.6%** at 192 |
| + predictive translation | **+20.6%** over the variant without it, single-threaded |

The shape is the point: optimistic latching is what makes the design *scale*
(it fixes the shared-write problem of Step 2), and predictive translation is
what makes a single access *fast* (it fixes the dependency problem of Step 1).
They are different fixes for different costs.

**Microarchitecture (§6.5)**, single-threaded uniform lookup, hardware counters,
reported per operation (the paper does not label the unit):

| system | cycles | instructions | IPC | L2 misses | dTLB misses | branch misses | front-end stalls |
|---|---|---|---|---|---|---|---|
| PrediCache | 3510 | 1915 | 0.55 | 28 | 3.5 | 11 | 131 |
| vmcache | 3939 | 1315 | 0.33 | 31 | 5.1 | 11 | 150 |
| traditional | 5090 | 1574 | 0.31 | 34 | 3.7 | 11 | 155 |

Dividing: PrediCache executes 1915/1315 = 1.46× vmcache's instructions yet uses
3510/3939 = 0.89× its cycles, because its IPC is 0.55 against 0.33. That is the
paper's evidence that the extra translation work is being hidden: more
instructions, fewer cycles. The traditional pool takes 5090/3510 = 1.45× the
cycles. The authors are careful about what this does not prove: L2 and branch
misses are similar across systems, L1 misses are lower for PrediCache (35 against
46–48) "the reason… is unclear", and the conclusion that superscalar overlap is
responsible is an inference from the counters, not a direct observation.

**Metadata (§6.9, Figure 12)** is the vmcache comparison of Step 3 made
quantitative: PrediCache's overhead scales with the buffer pool, vmcache's with
the storage.

### Step 8 — testing the overlap claim alone: `translation_micro`

> **In:** Step 4's claim and Listing 2.
> **Out:** a measured ns-per-access table for direct, hash and predictive
> access, including the case where the guess is wrong.

The paper's ablation measures the whole system. The question underneath is
narrower and testable in a few dozen lines: given a hash-table lookup and a page
read whose address is guessable, does reading the guess beside the lookup beat
reading after it? `cargo run --release --bin translation_micro` builds 2²⁰
pages of 64 bytes (64 MiB of frames) and an open-addressing table at 50% fill
(32 MiB), then times three access shapes over the same data. The three shapes,
quoted from the file (lines as they occur there):

```rust
// topics/06-buffer-pool/experiments/src/bin/translation_micro.rs — 171-187
 171  fn chain_hash(w: &World) -> u64 {            // hash: the read WAITS for the lookup
 172      let mut pid = w.start[0];
 173      for _ in 0..STEPS {
 174          let f = w.table.lookup(pid);
 175          pid = w.frames[f as usize].next;
 176      }
 177      pid
 178  }
 179  fn chain_pred(w: &World) -> u64 {            // predictive: read the GUESS beside the lookup
 180      let mut pid = w.start[0];
 181      for _ in 0..STEPS {
 182          let f = w.table.lookup(pid) as u64;
 183          let spec = w.frames[pid as usize].next;   // address depends on pid only
 184          pid = if f == pid { spec } else { slow_next(w, f) };
 185      }
 186      pid
 187  }
```

Lines 183 and 184 are the ones to read. Line 183 is Listing 2's line 5 moved
*above* the branch: it reads the frame at the guess (`frames[pid]`, the identity
placement this lane uses) and its address does not depend on the lookup's result
`f`, so the CPU may issue it while line 182 is still waiting on DRAM. Line 184 is
Listing 2's line 4: if the lookup agrees with the guess, keep the speculative
value; otherwise take the out-of-line slow path (lines 128–132: `#[cold]
#[inline(never)] slow_next`). `chain_direct` (164–170) is the same loop with
`pid = w.frames[pid as usize].next` and no table at all.

Two details are load-bearing, and both were found by getting the wrong answer
first. **The slow path is out of line**: if the mispredicted arm were inlined,
both arms are "load a frame's `next`" and the optimizer is free to turn the
branch into a **select** (a conditional move — a data dependency, not a branch),
which makes the read wait for the lookup again. **The guessed read is issued
before the branch, not inside it.** The first version of this lane wrote
`if f == pred { frames[pred].next } else { … }`, and measured no gain: inside the
taken arm the compiler knows `f == pred`, and the measurements are consistent
with it rewriting the load to use `f`, the lookup result, which restores the
dependency. (This was checked with scratch variants — `black_box` on the guess,
a runtime-zero xor on the guess, unchecked indexing — which stayed at the `hash`
time, while the version with the load hoisted above the branch matched `direct`.
The assembly was not read, so "consistent with" is the honest verb.) Lesson for
reading C++ or Rust translations of Listing 2: the technique is a property of
the *generated code*, and one innocent-looking comparison can undo it.

The workload comes in two shapes. *Independent*: the pids come from a
precomputed array, so even the `hash` shape lets the CPU overlap one
iteration's lookup with the next iteration's. *Chained*: each page stores the
pid of the next page, so iteration i+1's address is the result of iteration i's
read — a B-tree descent in miniature, and the shape in which the overlap has
nothing else to hide behind. `miss%` is the fraction of pages deliberately *not*
in their preferred frame (their frames are permuted among themselves), i.e. how
often the guess is wrong. `direct` is correct only when every page is in its
preferred frame, so it is shown only at 0%.

Output of two runs on an Apple M3 Pro (ns per page access; 5 M accesses per
cell; the two numbers in a cell are run 1 / run 2, measured 2026-10-27 on an
otherwise idle machine):

```text
 miss%  workload         direct          hash         predictive
     0  independent    3.9 /  3.6     9.1 /  9.4     7.2 /  7.3
     0  chained       86.9 / 86.3    183.3 / 181.3   99.6 / 100.7
    10  independent       -           9.4 /  9.5      8.1 /  8.1
    10  chained           -         181.5 / 178.5  107.0 / 108.5
    50  independent       -           9.4 /  9.3     13.4 / 13.0
    50  chained           -         182.7 / 178.2  160.9 / 161.6
```

How to read it. **Chained, 0% miss** is the clean test of Step 4: `hash` costs
about 180 ns — two DRAM misses in a row, one for the table slot and one for the
page, each about 90 ns — and `predictive` costs about 100 ns in both runs, 15–17%
above `direct` (86.9 and 86.3), which has no translation at all. Most of the
lookup's latency is hidden, as the paper's Listing 2 promises — what remains is
the lookup's own instructions, which no longer wait on a miss; the saving against
`hash` (about 45%) is larger than the paper's 20.6% (§6.4) because this
lane isolates the one dependent pair and the paper's number is a whole B-tree
lookup with other work in it. **As the guess gets worse the saving erodes
smoothly**: at 10% miss `predictive` is 107–109 ns, at 50% it is 161–162 ns,
against `hash` at 178–183. Treating each access as "fast with probability
1 − m, slow otherwise" and solving from the 50% row (0.5 × 100 + 0.5 × x = 161)
puts the slow path near 222 ns, about 40 ns *above* plain `hash`: the price of a
mispredicted branch and of the wasted speculative read. The break-even miss rate
under that estimate is 100 + m·(222 − 100) = 181 → m = 81 / 122 ≈ 66%; the same
model predicts 100 + 0.1 × 122 = 112 ns at 10% miss, against 107–109 measured. Two predictions
fit in two numbers: the technique wins while most accesses hit their preferred
frame, and loses once most do not — which is why the paper's promotion policy
(Step 5) exists. **Independent** accesses tell a smaller story: the CPU already
overlaps neighbouring iterations, so `hash` is only 9 ns per access and
`predictive` 7 ns at 0%; at 50% miss it is *worse* than `hash` (13 against
9 ns). The overlap trick pays where there is a dependent chain to hide
behind — a tree descent — and costs where there is not.


Deviations from the paper, deliberate and stated in the file's header: open
addressing with linear probing rather than chaining with an inlined first slot;
no latch or version check; 64-byte pages rather than 4 KiB; identity placement
(`preferred = pid`) rather than a hash of the pid modulo the pool. This is the
dependency-chain argument of §3 and nothing else — not PrediCache.

## How to read the paper (with the concepts in hand)

| Section | How to read it | Step |
|---|---|---|
| §1 and Table 1 | The claim and the five-row comparison. Skim; Step 2 reproduces the table. | 2, 3 |
| §2.1–2.2 | **Read carefully.** The objections to swizzling, LIPAH and vmcache, and the five design goals the rest is graded on. | 3 |
| §2.3 | Two snippets of dependent vs independent loads — the only hardware fact needed. | 1 |
| §2.4, Figure 2 | The Shore-MT hot path as a chain of dependent steps. | 2 |
| §3.1, Listings 1–2 | **Read twice.** The preferred frame and the speculative read. | 4 |
| §3.2 | The page lifecycle; why one-hit wonders justify not placing cold pages. | 5 |
| §4.1–4.2, Figure 3 | Chaining, the inlined first slot, optimistic latches; Figure 3 draws the traditional lookup beside the predictive one. | 6 |
| §5.1–5.2, Listing 3, Figure 4 | Promotion step by step; the three conditions of the fast path. | 5, 6 |
| §6.1–6.4 | The throughput results and the ablation (Figure 9). Read the ablation before the headline. | 7 |
| §6.5 | The counters table: the only direct evidence about superscalar overlap. | 7 |
| §6.6–6.8 | Chain lengths, copy cost, preferred-position hit rates. | 5, 6 |
| §7 | Related work; the closing remark that variable-size pages are unsupported. | — |

The paper does not support variable-size pages ("our design currently lacks
native support for variable-size pages", §7) and does not evaluate logging or
recovery ("we turn off logging", §6) — both matter for a database that keeps
data in storage, and both are out of this chapter's scope.

## Questions to answer in notes.md

1. Run `translation_micro` twice and compare `hash` with `predictive` in the
   chained rows at 0% miss. What fraction of the hash cost does the overlap
   remove, and how does that compare with the paper's 20.6% single-threaded
   (§6.4)? Name two reasons your number could differ.
2. At 10% and 50% miss, what happens to `predictive` relative to `hash`? Estimate
   it from first principles: a mispredicted branch costs roughly one pipeline
   flush; what does that say about the *shape* of the curve?
3. Pointer swizzling cannot represent a page with two referents (Step 3). For the
   capstone, GraphBLAS tiles are referenced by a row index *and* a column index.
   Which of the three modern designs is admissible, which metadata overhead does
   each charge at 1 TB of tiles (4 KiB each, 64-byte headers), and which would you
   build first?
4. The paper reports 75% of pages in their preferred frame at 0.5× data size, and
   3% at 3×. Why is the second number *not* a problem for the 91% access hit
   rate? What access distribution would make it a problem?

## Takeaway

Swizzling deletes the translation by making the parent hold the address; vmcache
deletes it by making the MMU hold the address; predictive translation keeps the
table and makes the address guessable, so the table's latency is spent while the
page read is already in flight. The price moves again: instead of a single-owner
rule or a kernel module, it is a probabilistic promotion policy and a hash table
sized at twice the pool. Whether that is a better trade is the paper's §6
question, answered on one 192-thread machine against three rivals; this repo's
micro-lane tests only the overlap, on a laptop.

## Done when

Answer each before unfolding it.

- [ ] You can explain, without the paper, why a hash-table hit is slower than a swizzled-pointer hit, and which of the two costs (dependency, shared writes) each of the paper's fixes removes.

  <details><summary>Answer</summary>

  A hash-table hit is a chain: bucket, then slot or control block, then frame
  header, then frame, each load needing the previous one's address (§2.4). If each
  misses the cache, the costs add rather than overlap (§2.3, "nearly double" for
  two). A swizzled pointer is one load because the parent already holds the
  frame's address (previous chapter).

  The chain is the *dependency* cost. The *shared-write* cost is separate: taking
  a bucket latch, even a reader-writer one, writes the latch word, so every core
  reading a hot page bounces that cache line (§4.2). Optimistic latching removes the
  shared write (the ablation's 7.4× at 192 threads is that, §6.4), and
  predictive translation removes the dependency by reading a guessed frame beside
  the lookup (the ablation's 20.6% at one thread). They fix different costs, which
  is why the ablation shows the first mattering at high thread counts and the
  second at one.

  </details>

- [ ] You can state Listing 2's fast-path condition, and say what the CPU does when the condition is false.

  <details><summary>Answer</summary>

  The lookup returns the page's real frame; the pool compares it with the *preferred*
  frame computed from the pid (§3.1). If they are equal ("is in preferred
  position") the read already issued at the preferred frame is the right one and is
  kept. Because the branch depends on a load that has not returned, the CPU
  predicts it (almost always "equal" for a hot page) and speculatively executes
  the read on the predicted frame in parallel with the lookup (§3.1, §5.2).

  When it is false, the speculative work is thrown away, the pipeline restarts on
  the else-path, and the read is issued at the looked-up frame (Listing 2 line 7).
  The cost is a branch misprediction plus the dependent read, but the thread was
  waiting on the lookup anyway (§3.1), so the paper treats it as cheap. The fast
  path in Listing 3 also requires that the page be in the cache, not exclusively
  latched, and in its predicted frame (§5.2).

  </details>

- [ ] You can say how pages come to sit in their preferred frame, and why 3% of resident pages in their preferred frame still gives 91% of accesses served from it.

  <details><summary>Answer</summary>

  New pages load into an arbitrary free frame unless the preferred frame is free.
  On later accesses a page not in its preferred frame is promoted into it with
  probability 1/32 (1/512 if that would demote an incumbent), copying a 4 KiB
  page and moving the displaced one to a free frame (§3.2, §5.1). Nobody counts
  hotness; frequent access makes promotion near-certain (a page seen 500 times is
  promoted with probability 1 − (31/32)⁵⁰⁰, effectively 1).

  The 3% (out-of-memory, 3× data, §6.8) counts pages; the 91% counts accesses. In
  an out-of-memory run the pool is full of cold pages loaded once and never
  promoted, so few pages are in their preferred frame — but the pages that receive
  most accesses, such as B-tree inner nodes, were promoted early and stay. The
  guess has to be right where the accesses are, not where the pages are.

  </details>

- [ ] You can reproduce the "chains are short" premise with a formula and check it against the paper's three fill factors.

  <details><summary>Answer</summary>

  With entries in buckets uniformly at random, bucket occupancy is approximately
  Poisson with mean α, the fill factor; conditioning on non-empty, P(k) =
  α^k e^(−α) / (k! (1 − e^(−α))). For α = 0.5: P(1) = 0.5 × 0.6065 / 0.3935 =
  0.771, P(≤ 2) = 0.963. For α = 0.75: P(1) = 0.671, P(≤ 2) = 0.923. For α = 1:
  P(≤ 3) = 0.970. The paper reports 77% of chains of length one at fill 0.5, 92%
  not longer than two at 0.75, and 97% not longer than three at 1.0 (§4.2, §6.6) —
  the same numbers, which is the zero-truncated binomial the paper cites. The
  consequence is that inlining the first slot (+24.8% at one thread, §6.4) is a
  bet that is almost always right.

  </details>

- [ ] You can decide, for a graph store whose tiles have several referents, which buffer-pool design to start from, and give the memory cost that rules out the alternative.

  <details><summary>Answer</summary>

  Pointer swizzling is out: a tile referenced by a row index and a column index
  has two referents, and swizzling's single-reference rule (§2.1) forbids it
  without a workaround. vmcache and predictive translation both allow multiple
  references. vmcache charges a frame header per *storage* page: 1 TB of 4 KiB
  tiles is 1 × 10¹² / 4096 ≈ 244 million headers × 64 B ≈ 15.6 GB of metadata
  whether or not the tiles are cached, plus a kernel module. Predictive
  translation charges metadata proportional to the *cache* (hash table at twice
  the pool, §6), needs no kernel module, and costs a probabilistic promotion
  policy. For a store on Linux or macOS with no appetite for a kernel module, the
  hash-table design is the one to build first; the tradeoff to re-check is that
  the paper's results are for 192 threads on one machine against a C++ B-tree,
  not for tiles.

  </details>

## References

- Zinsmeister, Nguyen, Leis, Neumann. *Predictive Translation: High-Performance Buffer Management Without the Trade-Offs.* SIGMOD 2026. [PDF](https://www-db.cs.tum.edu/~zinsmeister/papers/predictive-translation.pdf), DOI 10.1145/3786678.
- Leis, Haubenschild, Kemper, Neumann. *LeanStore: In-Memory Data Management Beyond Main Memory.* ICDE 2018. (pointer swizzling)
- Leis, Alhomssi, Ziegler, Loeck, Dietrich. *Virtual-Memory Assisted Buffer Management.* SIGMOD 2023. (vmcache)
- Previous chapter: [`reading-leanstore-paper.md`](reading-leanstore-paper.md).
