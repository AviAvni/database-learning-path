# SLEUTH: 38.5 million events, one 130-event story

The other three guides in this topic are about the graph *before* the breach — who could attack
what. SLEUTH is about the graph *after*: a host's audit log is a provenance graph of processes,
files and sockets connected by system calls, and reconstructing an attack means finding a
subgraph. The obstacle is not subtlety, it is volume and connectivity. Enterprise hosts emit
billions of events a day, more than 99.9% of them benign, and naive backward tracing from an
alert reaches almost everything — the *dependency explosion* problem. SLEUTH's answer is worth
reading as a database paper: a purpose-built main-memory graph at under 10 bytes per event
(against ~250 for a general graph database and ~3 KB for STINGER/NetworkX), plus a tag system
that turns pruning into a shortest-path problem with tag-derived edge costs. The combination gets
79 hours of audit data analysed in 14 seconds.

## The problem in one sentence

**An alert names one suspicious process; backward tracing through the audit log to find how the
attacker got in reaches millions of nodes, almost all irrelevant — so the reconstruction has to
prune while it searches, not after.**

## The concepts, step by step

### Step 1 — The provenance graph

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

§2 is unusually direct about this, and it is the part a database engineer should read twice.
Neo4j-class stores use roughly **250 bytes per graph edge**; STINGER and NetworkX about **3 KB**.
At "billions to tens of billions of events per day" that is terabytes of RAM. SLEUTH's design
gets to **under 10 bytes per event** — a **25× to 300× reduction** — and the techniques are the
same ones this book applies to columnar and index storage:

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

Forward analysis from the entry point assesses impact, and has the mirror-image size problem:
"a naive analysis produced impact graphs with millions of edges, whereas our refined algorithm
reduces this number by **100x to 500x**". Same cost metric, plus a tunable distance threshold
`d_th` to exclude nodes that are "too far", and a confidentiality-aware variant of the costs.

Three simplifications then produce something a human can read:

- **Prune uninteresting nodes** — dependencies no suspect node depends on (cache files, logs).
- **Merge entities with the same name** — same program, different pids and arguments.
- **Filter repeated events** — collapse N writes between the same pair, keeping first and last.

### Step 7 — The reduction, measured end to end

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
  The comparison against Neo4j/Titan (250 B/edge) and STINGER/NetworkX (3 KB) is the motivation;
  then work through the encoding bullet by bullet against Step 2 and convince yourself the 6-byte
  bidirectional edge is real.
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

- [ ] You can state the dependency-explosion problem and why post-hoc filtering does not solve it.
- [ ] You can explain the <10 bytes/event encoding well enough to sketch the record layout.
- [ ] You can name the two tag dimensions, the three t-tag levels, and why code and data t-tags
      are separate.
- [ ] You can explain backward analysis as Dijkstra and give the three edge costs.
- [ ] You can read Table 11 by column and say which stage contributes what.
- [ ] You wrote answers to all five questions in notes.md.

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
