# Why AWS writes TLA+: exhaustively testable pseudo-code

The experience report that moved TLA+ from academia to industrial default for
distributed protocols. Read it for the *economics*, not the math: what class of
bug justifies days of spec-writing, what the specs actually cost in lines, and
what the method still cannot do — a boundary the authors state more bluntly than
most vendors would. This chapter builds the concepts the argument rests on
(what a spec is, what a model checker does, why testing cannot reach the bugs it
finds), then works the paper's own table into a cost-per-bug figure, then quotes
the limitations verbatim, because they are the half most summaries drop.

Every figure below comes from the paper's table *Applying TLA+ to some of our
more complex systems* or from the named narrative section; the paper is
Newcombe, Rath, Zhang, Munteanu, Brooker and Deardeuff, dated 29 September 2014
and published as *How Amazon Web Services Uses Formal Methods*, CACM 58(4),
April 2015. It frames every other chapter in this topic.

## The problem in one sentence

DynamoDB's replication and group-membership design had a data-loss bug whose
**shortest** error trace was **35 high-level steps**, and it "had passed
unnoticed through extensive design reviews, code reviews, and testing" — because
no human and no test generator reliably explores interleavings that deep, while
a breadth-first search does not get bored.

## The concepts, step by step

### Step 1 — a specification is the design, written so a machine can explore it

> **In:** a design that currently exists as prose, a whiteboard, or state
> machine diagrams.
> **Out:** the same design as a state machine a tool can execute, and the two
> languages AWS actually used to write them.

A **specification** describes a system as a state machine: the variables that
constitute a **state**, the initial states, and the allowed transitions. Nothing
about threads, packets or code — just "from this state, these next states are
legal." **TLA+** is a language for writing exactly that, and it deliberately
reads like pseudo-code with mathematics instead of control flow.

The paper is careful about a distinction most summaries lose. TLA+ ships with a
**second language, PlusCal**:

> "TLA+ is accompanied by a second language called PlusCal which is closer to a
> C-style programming language, but much more expressive as it uses TLA+ for
> expressions and values. In fact, PlusCal is intended to be a direct
> replacement for pseudo-code. … PlusCal is automatically translated to TLA+
> with a single key press. … Also, tools such as the TLC model checker work at
> the TLA+ level."

This matters for reading the table in Step 4: **four of the six specs listed are
PlusCal, not TLA+**. "AWS writes TLA+" is true only in the sense that PlusCal
becomes TLA+ before anything checks it.

The formality is not rigour for its own sake. A design written this way can be
executed exhaustively by a tool; a design written in prose can only be reviewed
by tired humans. The companion chapter,
[reading-tlaplus-raft.md](reading-tlaplus-raft.md), teaches the language itself.

### Step 2 — model checking: enumerate, don't sample

> **In:** a spec plus fixed finite parameters — 3 replicas, 3 log entries.
> **Out:** either "no reachable state violates the invariant" or a concrete
> counterexample trace, and a precise statement of what "enumerate" buys over
> "sample".

A **model checker** — TLC, for TLA+ — takes a spec and a **model** (concrete
values for the constants) and searches the *entire* reachable state graph
breadth-first, checking a stated **invariant** (a predicate that must hold in
every reachable state, e.g. "committed data survives failover") at each state.

Contrast the testing spectrum of topic 16: a test — even a property-based
generator — *samples* behaviours from a distribution you do not control; TLC
*enumerates* them. When the invariant fails, TLC prints the exact step-by-step
trace that breaks it, which is a debugging artefact a fuzzer's seed is not.

This topic's own model, `specs/WalReplication.tla`, is the miniature: **1080
distinct states** from **2583 generated**, search depth **14**, `Durability`
holds — measured in `notes.md`. Flip `SyncCommit` to `FALSE` and TLC reports
**123 distinct states**, depth **5**, and the invariant **VIOLATED** with a
trace. Two numbers, one button.

The limitation is equally crisp: it checked 3 replicas × 3 entries and nothing
more. That gap is Step 6.

### Step 3 — the 35-step claim, attributed correctly

> **In:** Step 2's exhaustive search.
> **Out:** the paper's central evidence, with the right system attached to it —
> this is the fact most often mis-cited.

The 35-step bug is **DynamoDB's**, not S3's. From the paper's *First Big Success
at Amazon* section: author T.R. (Tim Rath) built DynamoDB's replication and
fault-tolerance mechanisms, did extensive fault-injection testing with a
simulated network layer, stress-tested on real hardware, *and* wrote detailed
informal proofs — which "did indeed find several bugs in early versions of the
design." Then:

> "This time the model checker found a bug that could lead to losing data if a
> particular sequence of failures and recovery steps was interleaved with other
> processing. This was a very subtle bug; the shortest error trace exhibiting
> the bug contained 35 high level steps."

And the sentence that answers "but our reviews are good":

> "The bug had passed unnoticed through extensive design reviews, code reviews,
> and testing, and T.R. is convinced that we would not have found it by doing
> more work in those conventional areas."

Note the shape of the argument. It is not that AWS's engineers reason badly; it
is that review quality is not the bottleneck, the state space is. The paper
pre-empts the "but that combination is improbable" objection directly:
"historically, AWS has observed many combinations of events at least as
complicated as those that could trigger this bug."

The checking itself was not free: the DynamoDB spec was checked with the
**distributed TLC model checker on a cluster of ten `cc1.4xlarge` EC2 instances,
each with 8 cores plus hyperthreads and 23 GB of RAM**. Hold that number against
the one-second local run of Step 2 — the cost of exhaustive search is entirely
set by the model size, and Step 6 is about choosing it.

### Step 4 — the table, and the cost per bug you can compute from it

> **In:** the paper's table *Applying TLA+ to some of our more complex systems*
> — six rows, each a system, a component, a line count excluding comments, and
> a benefit.
> **Out:** an arithmetic answer to "is this worth it", and a result that
> contradicts the intuition that bigger specs find more bugs.

Here is the table as printed, with the language column made explicit:

| System | Component | Lines | Language | Benefit (paper's words) |
|---|---|---|---|---|
| S3 | Fault-tolerant low-level network algorithm | 804 | PlusCal | Found 2 bugs. Found further bugs in proposed optimizations. |
| S3 | Background redistribution of data | 645 | PlusCal | Found 1 bug, and found a bug in the first proposed fix. |
| DynamoDB | Replication & group-membership system | 939 | TLA+ | Found 3 bugs, some requiring traces of 35 steps |
| EBS | Volume management | 102 | PlusCal | Found 3 bugs. |
| Internal distributed lock manager | Lock-free data structure | 223 | PlusCal | Improved confidence. Failed to find a liveness bug as we did not check liveness. |
| Internal distributed lock manager | Fault tolerant replication and reconfiguration algorithm | 318 | TLA+ | Found 1 bug. Verified an aggressive optimization. |

**Work it.** Total spec lines: `804 + 645 + 939 + 102 + 223 + 318 = 3031`.
Explicitly counted bugs: `2 + 1 + 3 + 3 + 0 + 1 = 10`. That is **303 lines of
spec per design bug found**, across six specs and four systems.

Now break it down per row and the average stops being the interesting number:

- **EBS volume management: 102 lines, 3 bugs — 34 lines per bug.** The smallest
  spec in the table has the best return by a factor of nine.
- **DynamoDB: 939 lines, 3 bugs — 313 lines per bug**, and it needed a ten-node
  EC2 cluster to check.
- **The 223-line lock-free data structure found zero bugs**, and the paper says
  why in the benefit column: "Failed to find a liveness bug as we did not check
  liveness." That row is not a "confidence win" — it is a **miss**, and the
  authors printed it.

So the relationship between spec size and bugs found is, on this data, weakly
*negative*. What predicts a find is not spec length; it is whether the algorithm
had a subtle concurrency argument in it. Two of the rows also record bugs found
in *fixes and optimizations* — the S3 rows both do — which is the recurring
payoff nobody budgets for: the spec keeps earning after the first bug.

Against those lines, the cost the paper quotes: engineers "from junior to
Principal have been able to learn TLA+ from scratch and get useful results in
**2 to 3 weeks**"; T.R. wrote the 939-line DynamoDB spec "in a couple of weeks";
B.M. "spent two weeks learning TLA+ and writing the spec" and TLC "found the bug
in a few seconds." Adoption at the time of writing: TLA+ used on **10 large
complex real-world systems**, **7 teams** using it.

### Step 5 — the pitch that worked, and why the wording is load-bearing

> **In:** a proven technique and an engineering organisation that has heard
> "formal methods" before and did not like it.
> **Out:** the specific rhetorical moves the paper credits with adoption, and
> the transferable lesson.

AWS did not sell "formal verification". The paper is explicit about the framing:

> "Engineers think in terms of debugging rather than 'verification', so we
> called the presentation 'Debugging Designs'. Continuing that metaphor, we have
> found that software engineers more readily grasp the concept and practical
> value of TLA+ if we dub it: **Exhaustively testable pseudo-code**."

And the omissions were deliberate too:

> "We initially avoid the words 'formal', 'verification', and 'proof', due to
> the widespread view that formal methods are impractical. We also initially
> avoid mentioning what the acronym 'TLA' stands for, as doing so would give an
> incorrect impression of complexity."

The reframing is load-bearing rather than cosmetic: PlusCal "is intended to be a
direct replacement for pseudo-code" (Step 1), so the spec has a reason to exist
*before* anyone checks it — it is the design document, written precisely — and
checking is then a button rather than a research project. The paper reports the
practice that followed: "first writing a conventional prose design document,
then incrementally refining parts of it into PlusCal or TLA+. Often this gives
important insights without ever going as far as a full specification or model
checking."

Steal the structure for any tool-adoption argument: attach the new cost to an
artefact people already have to produce.

### Step 6 — model small, learn big — and who actually said so

> **In:** the observation that TLC only checked 3 replicas × 3 entries.
> **Out:** the justification for believing a small model anyway, attributed to
> the right source, plus the case where it fails.

Checking 3 replicas × 3 entries is not a proof: the bug could in principle
appear only at N = 7. The empirical claim that protocol design bugs almost never
work that way — a broken quorum or ordering argument breaks at the smallest size
where the concepts exist, usually 2–3 processes — is the **small-scope
hypothesis**, and it is **Daniel Jackson's**, from the Alloy line of work
(*Software Abstractions*). It does not appear in this paper; do not cite it to
Newcombe et al.

What the paper actually claims is narrower and worth quoting for its hedging:
the model checker verified a part of the DynamoDB algorithm "for a **sufficiently
large instance** of the system to give very high confidence that it is correct."
"Sufficiently large" is an engineering judgement about a particular model, not a
general hypothesis, and "very high confidence" is not "proof".

The paper's own caveat, in its closing section, is blunter than either: "All
models are wrong, some are useful."

Know where the hypothesis fails. Bugs triggered by resource-boundary edge cases
— a B+tree page becoming exactly full, topic 3 — are about magnitudes, not
protocol logic, and a model small enough for TLC to finish never reaches them
(question 4).

### Step 7 — what formal specification is *not* good for

> **In:** the successes of Steps 3 and 4.
> **Out:** the three boundaries the authors state themselves, each with the
> section it comes from — this is the honest half of the report.

**Performance.** The section is titled *What Formal Specification Is Not Good
For*, and it names the failure mode precisely: "sustained emergent performance
degradation" — a momentary slowdown (say a Java GC pause) breaches client
timeouts, clients retry, retries add load, the server slows further. "In such
scenarios the system will eventually make progress; it is not stuck in a logical
deadlock, livelock, or other cycle. But from the customer's perspective it is
effectively unavailable." They considered specifying an upper bound on response
time as a real-time safety property and rejected it, because the underlying
disks, OS and network "do not support hard real-time scheduling or guarantees,
so real-time safety properties would not be realistic." The conclusion: "We
don't yet know of a feasible way to model a real system that would enable tools
to predict such emergent behavior."

**Code conformance.** The section is titled *The Most Frequently Asked
Question*, and the answer is one sentence: "On learning about TLA+, engineers
usually ask, 'How do we know that the executable code correctly implements the
verified design?' **The answer is that we don't.**" They add that they know of
no tools "that can handle distributed systems as large and complex as those we
are building", and that conventional static analysis is "largely limited to
finding 'local' issues in the code, and cannot verify compliance with a
high-level specification." The paper's constructive answer is indirect: formal
methods help engineers find strong system invariants, and those become
assertions in the code.

**Liveness.** Note that the paper does not make a general claim here. The only
evidence in the report is a single table cell — the 223-line lock-free data
structure, "Failed to find a liveness bug as we did not check liveness." That is
one team's choice on one spec, not a stated policy. Our own
`specs/WalReplication.tla` makes the same choice: its `.cfg` lists `TypeOK` and
`Durability` as `INVARIANTS`, both safety properties, and no `PROPERTIES` at all.

The drift point deserves the most respect: TLC verified the *design*, and nothing
keeps the implementation honest against it afterwards. Question 5 asks what our
capstone CI could do about that.

### Step 8 — the backstory, and why it is evidence rather than colour

> **In:** the paper's *First Steps To Formal Methods* section.
> **Out:** the reason a tool choice was made, which is the part you can actually
> reuse when choosing one yourself.

C.N. (Chris Newcombe) did not start with TLA+. He started dissatisfied with
systems that were "considered very successful, and yet bugs and operational
problems still remained", and observed that reactive mechanisms — pervasive
assertions, recovery-oriented computing — "cannot recover from the class of bugs
that cause permanent damage to customer data".

He was moved off the bias against formal methods by **Pamela Zave's** Alloy work
finding serious bugs in the membership protocol of **Chord**, a design from "a
strong group at MIT" that had won a 10-year test-of-time award at SIGCOMM 2011.
He then evaluated Alloy himself and rejected it on expressiveness: "we could not
find a practical way in Alloy to represent rich data structures such as dynamic
sequences containing nested records with multiple fields."

That is a reusable evaluation criterion, and it is why this topic's spec is TLA+
rather than Alloy: a WAL is a *sequence*, and `specs/WalReplication.tla:40`
appends to one.

## How to read the paper (with the concepts in hand)

It is a short CACM piece — read all of it, in order, in one sitting.

- The **table** carries the economics (Step 4). Read the language column, not
  just the line counts, and read the *Benefit* column as prose: two rows record
  bugs found in proposed *fixes*, and one records a miss.
- ***The Value of Formal Methods for 'Real-world Systems'*** has the adoption
  numbers (10 systems, 7 teams, 2–3 weeks).
- ***First Big Success at Amazon*** is the 35-step story (Step 3). Note how much
  conventional verification T.R. had already done before TLA+ found it — that
  ordering is the argument.
- ***Persuading More Engineers…*** has the pitch (Step 5) and the S3, EBS and
  lock-manager stories that populate the table's other rows.
- ***What Formal Specification Is Not Good For*** and ***The Most Frequently
  Asked Question*** are Step 7. Read them before you quote the successes.
- ***First Steps To Formal Methods*** is the Alloy-versus-TLA+ evaluation
  (Step 8).

Read it with `specs/WalReplication.tla` open: every claim the paper makes at S3
scale has a miniature counterpart in that 92-line model, including the choice not
to check liveness.

## Questions (answer in notes.md)

1. Compute lines-per-bug for each row of the paper's table, then rank the six.
   What property of the *algorithm* — not the spec — predicts the rows at the
   top? Which of the capstone's protocols has that property?
2. Which capstone protocol clears the paper's cost/benefit bar for a spec —
   MVCC visibility, delta-matrix `wait` concurrency, or WAL replication — and
   which is fine with proptest alone (topic 16)? Justify with a line estimate
   and the 2–3 week figure.
3. The 35-step bug: what makes an interleaving reachable but rare? Relate it to
   why our `SyncCommit = FALSE` counterexample is only **5 steps** deep — what
   does the toy model not have that DynamoDB's did?
4. Why does the small-scope hypothesis hold for protocols but not for a B+tree
   split bug (topic 3) that needs a page-full edge case? State the property of
   the bug, not of the tool.
5. Spec-code drift: sketch how the capstone's CI could keep
   `WalReplication.tla` honest against the real replication code. Use the
   paper's own constructive answer (invariants become assertions) as the
   baseline and say what it does and does not catch.
6. The paper reports 10 systems and 7 teams but only 6 specs in the table. What
   would you want to know about the 4 unreported specs before treating the
   303-lines-per-bug figure as a planning number?

## Done when

Answer each before unfolding it.

- [ ] You can explain what model checking does and why exhaustive enumeration differs in kind from testing — with this topic's two measured numbers.

  <details><summary>Answer</summary>

  TLC takes a spec plus a finite model and searches the *entire* reachable state
  graph breadth-first, checking the invariant at every state; a test samples
  behaviours from a distribution nobody fully controls. The difference is not
  thoroughness, it is coverage semantics: TLC's "no violation" is a statement
  about all reachable states of that model, and its failure output is a concrete
  minimal-depth trace.

  Measured here (`notes.md`): `WalReplication.tla` with `SyncCommit = TRUE` gives
  **2583 states generated, 1080 distinct, depth 14, `Durability` holds**; with
  `SyncCommit = FALSE`, **123 distinct states, depth 5, VIOLATED** with a trace.

  </details>

- [ ] You can state the 35-step claim with the right system attached, and say what had already been tried.

  <details><summary>Answer</summary>

  It is **DynamoDB's** replication and group-membership system — the 939-line
  TLA+ spec — not S3's. Before TLA+, author T.R. had done extensive
  fault-injection testing with a simulated network layer, long stress tests on
  real hardware, *and* detailed informal proofs (which found several earlier
  bugs). TLC then found a data-loss bug whose **shortest** trace was **35 high
  level steps**, and which "had passed unnoticed through extensive design
  reviews, code reviews, and testing."

  Checking it took the distributed TLC on **ten `cc1.4xlarge` EC2 instances,
  8 cores plus hyperthreads and 23 GB each**.

  </details>

- [ ] You can compute the paper's cost per bug and say why the average is the least useful number in the table.

  <details><summary>Answer</summary>

  Lines: `804 + 645 + 939 + 102 + 223 + 318 = 3031`. Bugs: `2 + 1 + 3 + 3 + 0 + 1
  = 10`. Average: **303 lines per bug**.

  The average hides the spread. EBS volume management is **102 lines / 3 bugs =
  34 per bug**; DynamoDB is **939 / 3 = 313**; the 223-line lock-free data
  structure found **none** — and the paper says why: "Failed to find a liveness
  bug as we did not check liveness." Spec size does not predict finds; the
  presence of a subtle concurrency argument does. Two rows also record bugs found
  in proposed *fixes and optimizations*, a payoff that arrives after the spec is
  written and is missing from any per-bug figure.

  </details>

- [ ] You can say how many of the six specs were PlusCal rather than TLA+, and why the distinction matters.

  <details><summary>Answer</summary>

  **Four of six are PlusCal** — 804 + 645 + 102 + 223 = **1774 lines** — and two
  are TLA+ — 939 + 318 = **1257 lines**. PlusCal is "closer to a C-style
  programming language" and "intended to be a direct replacement for
  pseudo-code"; it is translated to TLA+ "with a single key press", and "tools
  such as the TLC model checker work at the TLA+ level."

  It matters twice: for the adoption argument (Step 5's pitch is literally about
  pseudo-code, and the language that looks like pseudo-code is the one most of
  the specs are written in), and for reading line counts (a PlusCal line and a
  TLA+ line are not the same unit of work).

  </details>

- [ ] You can attribute the small-scope hypothesis correctly and quote what the paper itself claims instead.

  <details><summary>Answer</summary>

  The **small-scope hypothesis is Daniel Jackson's**, from the Alloy line of work
  (*Software Abstractions*). It is not in this paper. Newcombe et al. claim
  something narrower: the model checker verified part of the DynamoDB algorithm
  "for a **sufficiently large instance** of the system to give very high
  confidence that it is correct" — an engineering judgement about one model, and
  "very high confidence", not proof. Their own caveat is "All models are wrong,
  some are useful."

  Where it fails: bugs about magnitudes rather than protocol logic — a page
  becoming exactly full, a counter wrapping — because those need a scale the
  model deliberately does not have.

  </details>

- [ ] You can name the three things the paper says formal specification did not do for AWS, and cite the section for each.

  <details><summary>Answer</summary>

  1. **Emergent performance degradation** — section *What Formal Specification Is
     Not Good For*. Real-time safety properties were rejected as unrealistic on
     infrastructure without hard real-time guarantees: "We don't yet know of a
     feasible way to model a real system that would enable tools to predict such
     emergent behavior."
  2. **Code conformance** — section *The Most Frequently Asked Question*: "How do
     we know that the executable code correctly implements the verified design?
     The answer is that we don't." No tools at that scale; static analysis finds
     only local issues.
  3. **Liveness** — but carefully: the only evidence is one table cell, the
     223-line lock-free spec, "Failed to find a liveness bug as we did not check
     liveness." That is a choice on one spec, not a stated policy. Our
     `WalReplication.cfg` makes the same choice: two `INVARIANTS`, no
     `PROPERTIES`.

  </details>

- [ ] You wrote answers to all six questions in notes.md, including the lines-per-bug ranking and which capstone protocol clears the bar.

  <details><summary>Answer</summary>

  The ranking to check yours against: EBS 34, S3-network 402, S3-redistribution
  645, DynamoDB 313, lock-manager replication 318, lock-manager lock-free ∞ (zero
  bugs). What the top rows share is a fault-tolerance or concurrency argument
  whose correctness depends on interleaving, which is exactly what enumeration
  buys you and sampling does not.

  Record the *reasoning* for the capstone choice, not just the pick: a protocol
  earns a spec when its correctness argument is about orderings of concurrent
  events across failure, and when getting it wrong loses data rather than
  degrades performance. WAL replication under failover is that; a single-node
  page-split boundary is not.

  </details>

## References

**Papers**
- Chris Newcombe, Tim Rath, Fan Zhang, Bogdan Munteanu, Marc Brooker, Michael
  Deardeuff — *How Amazon Web Services Uses Formal Methods*, CACM 58(4), April
  2015 (the preprint is titled *Use of Formal Methods at Amazon Web Services*,
  dated 29 September 2014). Short; read all of it. The table *Applying TLA+ to
  some of our more complex systems* carries the economics of Step 4;
  *What Formal Specification Is Not Good For* and *The Most Frequently Asked
  Question* carry Step 7.
- Pamela Zave — *Using Lightweight Modeling to Understand Chord* — the Alloy
  work the paper credits with overcoming its own bias against formal methods
  (Step 8).
- Daniel Jackson — *Software Abstractions: Logic, Language, and Analysis* — the
  actual source of the small-scope hypothesis of Step 6, and of Alloy, the tool
  AWS evaluated and rejected on expressiveness.

**In this topic**
- `specs/WalReplication.tla` (92 lines) and `specs/WalReplication.cfg` — the
  miniature: 3 replicas, `MaxLog = 3`, `Quorum = 2`, invariants `TypeOK` and
  `Durability`, no liveness properties.
- `notes.md` — the measured TLC runs quoted in Steps 2 and 3.
- [reading-tlaplus-raft.md](reading-tlaplus-raft.md) — the language itself, and
  what happens to the state space when the protocol gets real.
