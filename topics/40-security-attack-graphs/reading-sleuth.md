# SLEUTH: 38.5 million events, one 130-event story

The other three guides in this topic are about the graph *before* the breach — who could attack
what. SLEUTH is about the graph *after*: a host's audit log is a provenance graph of processes,
files and sockets connected by system calls, and reconstructing an attack means finding a
subgraph. The obstacle is not subtlety, it is volume and connectivity. Enterprise hosts emit
billions of events a day, more than 99.9% of them benign, and naive backward tracing from an
alert reaches almost everything — the *dependency explosion* problem. SLEUTH's answer is worth
reading as a database paper: a purpose-built main-memory graph at under 10 bytes per event
(against ~250 bytes/edge for STINGER and ~3 KB for NetworkX — the two *main-memory-optimized*
graph stores the paper actually measures), plus a tag system
that turns pruning into a shortest-path problem with tag-derived edge costs. The combination gets
79 hours of audit data analysed in 14 seconds.

## The problem in one sentence

**An alert names one suspicious process; backward tracing through the audit log to find how the
attacker got in reaches millions of nodes, almost all irrelevant — so the reconstruction has to
prune while it searches, not after.**

## The concepts, step by step

### Step 1 — The provenance graph

> **In:** a host's raw audit log — Windows event logs, Linux audit, or FreeBSD DTrace.
> **Out:** the OS-neutral provenance graph (subjects = processes, objects = files/pipes/sockets, edges = timestamped information-flow events), and what "an attack" is as a connected subgraph of it.

Two vertex types and one edge type:

```
   subjects = processes           (pid, command line, owner, code tag, data tag)
   objects  = files, pipes, sockets  (name, type, owner, tags)
   edges    = audit events: read, write, execve, fork, connect, chmod, rename, ...
              labelled and timestamped, directed by information flow
```

An attack is a connected subgraph of this: a socket the attacker connected from, a process that
read it, a file it wrote, a process that later executed that file. The graph is OS-neutral —
SLEUTH normalises Windows event logs, Linux audit and FreeBSD DTrace into the same shape.

### Step 2 — Why a general graph database is the wrong tool here

> **In:** the provenance graph and enterprise event volumes (billions to tens of billions/day).
> **Out:** the memory argument — general graph stores cost too much per edge — and SLEUTH's <10-bytes-per-event encoding (a 6-byte bidirectional edge), the same domain-specific-encoding move as topic 12.

§2 is unusually direct about this, and it is the part a database engineer should read twice.
General graph databases (Neo4J, Titan) it dismisses qualitatively — their memory use is simply
"too high", with no figure. The numbers it *does* give are for the two stores optimized for
main-memory performance: **STINGER ≈ 250 bytes per graph edge**, **NetworkX ≈ 3 KB per edge**.
At "billions to tens of billions of events per day" that is terabytes of RAM. SLEUTH's design
gets to **under 10 bytes per event** — a **25× (vs STINGER) to 300× (vs NetworkX) reduction** — and
the techniques are the same ones this book applies to columnar and index storage:

- **32-bit identifiers instead of 64-bit pointers.** Enough for 4 billion objects/subjects per
  host; the largest data set had orders of magnitude fewer.
- **Events stored inside subjects**, eliminating subject-to-event pointers and event identifiers
  entirely. Since events outnumber objects and subjects by about two orders of magnitude, event
  compactness is what matters.
- **Variable-length encoding.** A subject-event record is **4 bytes** in the typical case, 8, 12
  or 16 when needed. Event names are 3 bits or fewer for the frequent ones (open, close, read,
  write); object references are 8 bits or fewer, because a process touches few distinct objects
  and they are indexed per subject "like file descriptors".
- **Delta timestamps.** Millisecond resolution instead of microsecond, stored relative to the
  last event on the same subject, so **16 bits** suffice typically; a special `timegap`
  pseudo-event covers longer intervals.
- **Object-event records only where they matter** — for `read`/`write`, the events that create a
  dataflow — and stored as a *relative index* into the subject's event list rather than a copy,
  fitting in 12 bits and so 16 bits per record.

Net: a bidirectional timestamped edge in as little as **6 bytes** (4 for the subject-event
record, 2 for the object-event record). Measured: **38M events in 329 MB**. Decoding costs under
100 ns, "many orders of magnitude faster than disk latencies".

If you have read topic 12, this is exactly the columnar-compression argument — domain-specific
encodings beat a general-purpose layout by two orders of magnitude — arriving at the same
conclusion from the security side.

### Step 3 — Tags: two dimensions, and the split that matters

> **In:** the compact provenance graph, and the need to prune traffic that is >99.9% benign.
> **Out:** the two tag dimensions (trustworthiness t-tags, confidentiality c-tags), the code-vs-data t-tag split, and the conservative propagation rule — plus Table 10's measured payoff for the split.

Every subject and object carries tags summarising *provenance-derived* trust and sensitivity.

**Trustworthiness tags (t-tags)**, three levels:

```
   benign authentic  data/code from a trusted source, authenticity verified
   benign            believed benign, authentication not performed
   unknown           no information — "such data can sometimes be malicious"
```

**Confidentiality tags (c-tags)**: `secret` (credentials, private keys) → `sensitive` → `private`
→ `public`.

The design decision that carries most of the paper's results: a subject gets **two** t-tags, one
for its **code** and one for its **data**. A process that reads an untrusted file has untrusted
*data* but still trusts its own *code*; a process that executes an untrusted file has untrusted
code. Conflating them over-taints everything. Table 10 measures the split against a single tag,
per detection policy:

```
   policy                              avg reduction in (false) alarms
   untrusted execution                        45.39x
   modification by low code t-tag subject    517x
   preparation of untrusted data for exec      6.24x
   confidential data leak                    112x
```

Tags are initialised by policy at the `init` trigger (network connections from outside get
`unknown`; pre-existing files get `benign authentic`) and propagated by policy at each event
trigger. The default propagation is conservative — an output takes the *lowest* trustworthiness
and the *highest* confidentiality among its inputs — so it "can err on the side of over-tainting,
but will not cause attacks to go undetected".

### Step 4 — Detection: four policies about means and motive

> **In:** tagged subjects and objects from Step 3.
> **Out:** the four objective-based detection policies (means = an untrusted source, recorded by the `unknown` t-tag; motive = a goal-advancing event) attached to trigger points, and how external detectors compose by setting a code t-tag.

SLEUTH deliberately avoids application-specific knowledge and detects on attacker *objectives*
instead. The reasoning: an attacker needs both motive (an event advances a goal) and means (the
data or code came from an untrusted source, which is what the `unknown` t-tag records). The four
policies:

- **Untrusted code execution** — a subject executes an object with a lower code t-tag.
- **Modification by a subject with lower code t-tag** — untrusted code writing a trusted file.
- **Confidential data leak** — a subject with a `sensitive` c-tag and an `unknown` code t-tag
  sends data out.
- **Preparation of untrusted data for execution** — `chmod`/`mprotect` making untrusted content
  executable.

Policies attach to *trigger points* rather than to raw events (Table 2), so one policy covers
several syscalls with the same information-flow direction. External detectors compose cleanly:
flag a subject, set its code t-tag to `unknown`, and every downstream policy inherits the
suspicion.

### Step 5 — Backward analysis as shortest path with tag-derived costs

> **In:** alarms (flagged subjects) and the tagged graph.
> **Out:** backward analysis reframed as Dijkstra with tag-derived edge costs (0 / high / 1), why it can stop the moment an entry point joins the shortest-path tree, and how it resolves multiple candidate entry points.

This is the algorithmic core. Backward analysis starts from alarms and walks the graph in reverse
to find entry points (in-degree zero, untrusted — typically network connections). Two problems:
the graph has hundreds of millions of edges, and many entry points are backward-reachable from
any suspect node while APT-style attacks usually have exactly one real one.

The insight: tags are already a path computation, so reuse them as **edge costs** and run
Dijkstra.

```
   unknown ──▶ benign      cost 0     the malicious/benign boundary — must be in the path
   benign  ──▶ benign      cost HIGH  flows among trusted entities — exclude
   unknown ──▶ unknown     cost 1     inside the suspicious region — likely part of the attack
```

Dijkstra discovers paths in increasing cost order and grows a shortest-path tree, so the search
can **stop as soon as an entry point enters the tree** — you need not traverse the graph. The
formulation also answers the multiple-entry-point problem for free: it prefers the entry point
closest by path cost.

### Step 6 — Forward analysis and simplification

> **In:** the entry point found by backward analysis.
> **Out:** forward impact analysis (same cost metric, plus a distance threshold `d_th`) reduced 100×–500×, and the three simplifications that make the graph human-readable.

Forward analysis from the entry point assesses impact, and has the mirror-image size problem:
"a naive analysis produced impact graphs with millions of edges, whereas our refined algorithm
reduces this number by **100x to 500x**". Same cost metric, plus a tunable distance threshold
`d_th` to exclude nodes that are "too far", and a confidentiality-aware variant of the costs.

Three simplifications then produce something a human can read:

- **Prune uninteresting nodes** — dependencies no suspect node depends on (cache files, logs).
- **Merge entities with the same name** — same program, different pids and arguments.
- **Filter repeated events** — collapse N writes between the same pair, keeping first and last.

### Step 7 — The reduction, measured end to end

> **In:** all of the above, run on the DARPA Transparent Computing campaigns.
> **Out:** Table 11 read by column — single t-tag 4.68×, split t-tags 1305×, simplification 41.8× — plus the runtime (79 h in 14 s) and accuracy (174 correct / 0 wrong / 2 missed) figures.

Table 11 is the summary the whole paper builds to. For each DARPA Transparent Computing campaign:
initial event count, final scenario-graph event count, and the reduction attributable to each
stage.

```
   dataset   initial events   final   single t-tag   split t-tags   simplif.     total
   W-1               100 K       51           4.4x         1394x        1.4x     1951x
   L-2              38.5 M      130           7.3x         2971x        100x   297100x
   L-3              19.3 M       45           7.6x         1208x        356x   430048x
   F-2              5.86 M       39           1.9x          689x        218x   150202x
   average                                    4.68x        1305x       41.8x    54517x
```

Read the columns, not the totals: forward analysis with a **single** t-tag reduces by 4.68× on
average; splitting code and data t-tags reduces by **1305×**. One modelling decision is worth
two and a half orders of magnitude. Simplification adds another 41.8×.

Runtime and memory (Table 9, one core of a 2.8 GHz Opteron, 48 GB RAM):

```
   L-2:  79:06:39 of audit data, 38.5M events → 329 MB, 114.14 s   (speed-up 2.5 K)
   F-3:  79:04:54,                5.68M events →  95 MB,  15.75 s   (speed-up 18.1 K)
   W-mean                                          6.5 MB           (speed-up 26.3 K)
```

"Speed-up" here is the ratio of data duration to analysis time — the number of simultaneous
hosts one core could keep up with. The accuracy side (Table 7): across eight campaigns, **174
entities correctly identified, 0 incorrectly, 2 missed**, and the two misses were a cataloguing
gap in sensitive-file definitions, not an algorithmic failure. In a separate benign environment
with four security updates and 110 packages upgraded across 30M+ events, **no alarms fired** —
because one policy rule declares `dpkg` invoked by `apt`/`unattended-upgrades` to be trusted.
That single rule is the difference between a usable tool and an alert firehose.

## How to read the paper (with the concepts in hand)

- **§1.1 Approach overview + Fig 1.** The four-stage pipeline. The headline numbers (79 hours in
  14 s at 84 MB; 38.5M events → 130) are here.
- **§2 Main-memory dependency graph.** Read this as a storage-engine section, because it is one.
  The motivation is the memory comparison: Neo4J/Titan dismissed as "too high" with no figure,
  STINGER quoted at ~250 B/edge and NetworkX at ~3 KB/edge; then work through the encoding bullet
  by bullet against Step 2 and convince yourself the 6-byte bidirectional edge is real.
- **§3 Tags and attack detection.** §3.1 for the tag lattices; the paragraph on splitting code and
  data t-tags is the one to mark. §3.2 for the four policies and the motive/means argument.
- **§4 Policy framework + Table 2.** Trigger points as a level of indirection over events. Note
  that policies compile to C++ functions, not an interpreted rule language.
- **§5 Bi-directional analysis.** The heart. §5.1's cost assignment (0 / high / 1) and *why*
  Dijkstra rather than BFS — read the sentence about stopping as soon as an entry point joins the
  shortest-path tree. §5.2 for the forward direction and `d_th`; §5.3 for the three
  simplifications.
- **§6.2–6.3 + Table 3.** The data sets. Note "more than 99.9% of the events corresponded to
  benign activity" — that is the needle-in-a-haystack ratio the reduction numbers are against.
- **§6.7 + Table 9, §6.8 + Table 10, §6.9 + Table 11.** Runtime/memory, the split-tag benefit, and
  the end-to-end reduction. Table 11's *columns* are the finding.
- **§6.6 + Table 8.** False alarms in a benign environment. The `dpkg`-under-`apt` rule.
- **After the paper.** Exercise 7 in the topic README: implement backward analysis over lane 1's
  graph with tag-derived edge costs and compare the entry points it finds against plain BFS
  ancestry.

## Questions to answer in notes.md

1. SLEUTH gets to <10 bytes/event where a general graph store uses ~250 bytes/edge. List the four
   encoding decisions that buy the most, and say which of them would *not* survive if the graph
   had to be updatable and queryable by arbitrary Cypher.
2. Splitting one t-tag into code and data t-tags is worth 1305× versus 4.68× (Table 11, columns 4
   and 5). Explain the mechanism: what specific over-tainting does the split prevent, and give a
   concrete two-process example.
3. The backward-analysis cost function assigns 0 to unknown→benign edges and a high cost to
   benign→benign. Why is 0 (rather than 1) the right cost for the boundary edges, given that
   Dijkstra returns minimum-cost paths?
4. The default tag propagation "can err on the side of over-tainting, but will not cause attacks
   to go undetected". State that as a soundness/completeness claim, and say which of the two the
   design sacrifices.
5. This topic's other three guides analyse a graph of *permissions*; SLEUTH analyses a graph of
   *events*. Both do reachability. Name two things the event graph makes harder (hint: one is in
   §2, one is in §5.1) and one thing it makes easier.

## Done when

Answer each before unfolding it.

- [ ] You can state the dependency-explosion problem and why post-hoc filtering does not solve it.

  <details><summary>Answer</summary>

  Dependency explosion: naive backward tracing from a single alert follows information-flow edges
  until it reaches almost every node — an enterprise host emits billions of events/day, >99.9% of
  them benign, and everything is transitively connected. Post-hoc filtering does not help because
  you would first have to *materialize* the millions-of-edges graph you are trying to avoid;
  SLEUTH instead prunes *during* the search — Dijkstra stops the moment an entry point joins the
  shortest-path tree (Step 5).

  </details>

- [ ] You can explain the <10 bytes/event encoding well enough to sketch the record layout.

  <details><summary>Answer</summary>

  32-bit ids (4 billion entities/host) not 64-bit pointers; events stored *inside* subjects, which
  removes subject→event pointers and event ids entirely (events outnumber objects/subjects ~100×,
  so event compactness is what matters); variable-length subject-event records — **4 bytes**
  typical, up to 16 — with **3-bit** event names for frequent syscalls and **≤8-bit** per-subject
  object references "like file descriptors"; **delta timestamps** at ms resolution relative to the
  subject's last event (**16 bits**, with a `timegap` pseudo-event for long gaps); object-event
  records only for `read`/`write`, stored as a **12-bit** relative index. Net: a bidirectional edge
  in ~**6 bytes** (4 + 2); 38M events in 329 MB.

  </details>

- [ ] You can name the two tag dimensions, the three t-tag levels, and why code and data t-tags
      are separate.

  <details><summary>Answer</summary>

  Dimensions: **trustworthiness** (t-tags) and **confidentiality** (c-tags). Three t-tag levels:
  *benign authentic* → *benign* → *unknown*. c-tags: *secret* → *sensitive* → *private* →
  *public*. A subject carries **two** t-tags — one for its **code**, one for its **data** — because
  a process that reads an untrusted file has untrusted data but still-trusted code; conflating them
  over-taints everything downstream. Table 11 measures the split at **1305×** against **4.68×** for
  a single tag.

  </details>

- [ ] You can explain backward analysis as Dijkstra and give the three edge costs.

  <details><summary>Answer</summary>

  Backward analysis walks the graph in reverse from alarms toward entry points (in-degree zero,
  untrusted — typically outside network connections). Reuse the tags as edge costs:
  `unknown → benign` = **0** (the malicious/benign boundary — must be on the path);
  `benign → benign` = **HIGH** (trusted flows — exclude); `unknown → unknown` = **1** (inside the
  suspicious region). Dijkstra discovers paths in increasing cost order, so it can **stop as soon
  as an entry point enters the shortest-path tree**, and it naturally prefers the lowest-cost entry
  point when several are reachable.

  </details>

- [ ] You can read Table 11 by column and say which stage contributes what.

  <details><summary>Answer</summary>

  The columns are the finding, not the totals. Forward analysis with a *single* t-tag: **4.68×**
  average. Splitting code and data t-tags: **1305×** — two and a half orders of magnitude from one
  modelling decision. Simplification (prune / merge / filter): **41.8×**. On L-2 the chain is
  38.5M events → 130 (297,100× total), of which the split column alone is 2971×.

  </details>

- [ ] You wrote answers to all five questions in notes.md.

  <details><summary>Answer</summary>

  The five: (1) which encoding decisions survive if the graph must be updatable and Cypher-queryable;
  (2) the mechanism behind the 1305× code/data split, with a concrete two-process example; (3) why
  **0** (not 1) is the right cost for boundary edges under Dijkstra; (4) the propagation rule stated
  as a soundness-vs-completeness claim and which is sacrificed; (5) two things the event graph makes
  harder and one it makes easier, versus a permission graph.

  </details>

## References

- Hossain, Milajerdi, Wang, Eshete, Gjomemo, Sekar, Stoller, Venkatakrishnan. *SLEUTH: Real-time
  Attack Scenario Reconstruction from COTS Audit Data.* USENIX Security 2017 —
  [PDF](https://www.usenix.org/system/files/conference/usenixsecurity17/sec17-hossain.pdf).
- King & Chen. *Backtracking Intrusions.* SOSP 2003 — the forensic ancestor SLEUTH makes real-time.
- Local exercise: topic README exercise 7 — tag-derived edge costs over `ad_graph.rs`.
- Topic 12 (columnar storage) — the same "domain-specific encoding beats general layout by two
  orders of magnitude" argument, from the analytics side.
- Topic 33 (temporal graphs) — the provenance graph is a contact sequence, and "time-respecting
  path" is exactly what a causal dependency is.
