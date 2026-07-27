# Monotonicity: how attack graphs went from 2²²⁹ states to 229 nodes

In 2002 the state of the art in network vulnerability analysis was a model checker: encode the
network as boolean state variables, encode the attacker's goal as a temporal-logic formula, and
let NuSMV enumerate every counterexample. It worked, at five hosts. Ammann, Wijesekera and
Kaushik's CCS'02 paper is eight pages long and contains no clever algorithm — its whole content
is one modelling assumption, *the attacker never needs to backtrack*, which turns an exponential
state space into a linear one. Four years later MulVAL (CCS'06) showed that the fixpoint this
buys you is just a tabled Datalog evaluation, so you can get it from an off-the-shelf logic
engine and inherit cycle handling and memoization for free. Together they are a clean case study
in the thing this book keeps circling: the representation, not the algorithm, is where the
complexity lives.

## The problem in one sentence

**Enumerating the network states an attacker can reach is exponential in the number of system
variables, but enumerating the *facts* an attacker can establish is linear — and if no exploit
ever un-establishes a fact, the two questions have the same answer.**

## The concepts, step by step

### Step 1 — The state-based attack graph, and its size

Sheyner et al. (Oakland'02) model the network as a collection of boolean variables — which
service runs where, which host trusts which, what privilege the attacker holds on each machine —
and an exploit as a state transition. The attack graph is then the set of reachable states with
transitions between them, produced as the complete set of model-checker counterexamples.

The numbers Ammann et al. quote for the scaling exercise in that line of work:

```
   5 hosts, 8 exploits
   NuSMV runtime .............. 2 hours (most of it graph manipulation)
   attack graph ............... 5,948 nodes / 68,364 edges
   state space ................ 229 bits  →  2^229 possible states
```

Five hosts. The paper's dry observation is that with 229 bits of state you need "at most 229
nodes" to record which bits an attacker can turn on — *if* turning one on never turns another
off.

### Step 2 — Attributes and exploits

The replacement model is deliberately flat. Let `A = {a₀ … a_N}` be **attributes**: atomic facts
about the system. An attribute can be a vulnerability ("host 3 runs a vulnerable sshd"), a
connectivity fact ("host 1 can reach host 2 on the ftp port"), a trust fact ("host 2's .rhosts
trusts host 1"), or an attacker privilege ("attacker has root on host 2"). Attributes are the
graph's nodes.

An **exploit** is an atomic transformation with a set of preconditions and a set of
postconditions, both sets of attributes. The paper's worked example is `rcp`:

```
   preconditions                          postconditions
   ─────────────                          ──────────────
   rcp available on attacker         ┐
   victim trusts attacker (.rhosts)  ├──▶ [ rcp exploit ] ──▶ attacker files available on victim
   shell access on attacker          │      attacker=host1
   connectivity to victim            ┘      victim=host2
```

Exploits are instantiated per host tuple: `port-forward(attacker, middleman, victim)` becomes one
concrete exploit per assignment of the three roles, so the exploit count is quadratic in hosts
for two-host exploits and cubic for three-host ones. In the paper's three-host example: 6
vulnerabilities × 3 hosts = 18 attributes, 3 connectivity relations × 9 host pairs = 27, 6 trust,
9 privilege — **60 attributes total**, and 4 generic exploits become **30 instantiated ones**.

### Step 3 — Monotonicity, stated precisely

> "The precondition of a given exploit is never invalidated by the successful application of
> another exploit. In other words, the attacker never needs to backtrack."

Three technical consequences, and it is worth being pedantic about them because they are what the
polynomial bound rests on:

1. Attributes go from *not satisfied* to *satisfied* and never the reverse.
2. **No negation in preconditions** — an unsatisfied attribute can become satisfied later, so a
   negated precondition would be non-monotone by construction.
3. Preconditions are conjoined (a disjunction is modelled by splitting the exploit in two), and
   postconditions are conjoined.

And a fourth, used by the complexity argument: `preConds(e) ∩ postConds(e) = ∅` — an exploit
never has an attribute as both input and output.

### Step 4 — Where the assumption bends, and why it survives

The paper is honest about this and the examples are worth remembering:

- **`port forward`** genuinely consumes a port on the middleman, so that port is now unavailable
  for a different port-forwarding attack. Non-monotone. But "a clever attacker can often get by
  with a single port by merely switching back and forth between the two exploits", so modelling
  it monotonically loses nothing real.
- **`code green`**, a worm that *patches* the hole it entered through. Clearly non-monotone. Same
  argument: nothing stops the attacker re-opening it.
- The second postcondition of `sshd buffer overflow` is "host T is not running sshd" — the
  service crashed. Modelled away, because with root on T the attacker can restart it.

The empirical defence is the strongest part: the authors' lab had been encoding exploits for
training pentesters for over a year and "has yet to encounter an exploit where the monotonicity
assumption wasn't at least as plausible as in the 'port forward' or 'code green' examples". You
are choosing a model, not proving a theorem about reality.

### Step 5 — `markAttributes`: BFS over facts, in layers

With monotonicity, forward reachability is a fixpoint computed layer by layer. Layer 1 is
everything one exploit can establish from the initial state; layer n is everything reachable in n
chained exploits.

```
   markAttributes(S, att):
     U₀ = initially satisfied attributes
     repeat for n = 1, 2, ...:
       for each attribute aₖ and exploit eⱼ:
         if preConds(eⱼ) ⊆ U_{n-1} and aₖ ∈ postConds(eⱼ)
              and aₖ not already marked at level ≤ n:
           mark aₖ with (eⱼ, level n)
       Uₙ = U_{n-1} ∪ {attributes marked at level n}
     until Vₙ = ∅
```

Cost: **O(|A|² · |E|)**, from two facts. `Uₙ` only grows and is bounded by `A`, so there are at
most `|A|` layers; and because `preConds(e) ∩ postConds(e) = ∅`, each layer does at most
`|A| · |E|` work. The layer number is not bookkeeping — it is the minimum number of chained
exploits to establish the attribute, which is what `findShort` later uses.

### Step 6 — The three analyses you get for free

Once the marked attribute/exploit graph exists, you do not need to materialize an attack tree:

- **`findMinimal(S, att)`** — one minimal attack: recursively pick a minimal exploit set covering
  the unsatisfied goals. Quadratic in `|E|` (steps 1 and 3 may take `E²` in the worst case),
  assuming bounded pre/postcondition counts. *Result 1: the attack returned is minimal.*
- **`findAll(S, E_all)`** — every exploit that participates in some attack. *Results 2 and 3:
  findAll misses no such exploit and includes no other* — it can't miss any because forward
  marking already applied every feasible exploit, and it can't over-include because it searches
  backward from the goal.
- **`findShort(S, att)`** — an attack of minimal *depth*, using the layer numbers. `O(E²)` to get
  a minimal shortest attack.

Finding a *minimum-cardinality* attack is NP-complete (Sheyner et al.); minimal is easy. Know the
difference before you promise an optimizer.

### Step 7 — Cut sets: §2.3, one paragraph, the whole defensive story

> "It is also useful to think in terms of 'cut sets' of either exploits or attributes. These
> approaches ask the question: what set of exploits (edges) or attributes (nodes) in our graph
> must be removed to disconnect the goal state from the initial state? Standard graph analysis
> algorithms can be applied."

That is the entire defensive half of the field, in three sentences, in 2002. Lane 2 of this
topic's crate is that paragraph made precise: in the reverse graph rooted at the goal, node `d`
dominates node `u` iff every path from `u` to the goal crosses `d`, so a single dominator-tree
pass prices every single-node cut exactly — measured 0.8 ms against 543 ms for 3400 individual
reachability re-runs, agreeing on every node.

### Step 8 — MulVAL: the fixpoint is a Datalog derivation

Ou, Boyer & McQueen (CCS'06) make the same move as a logic program. A MulVAL interaction rule:

```prolog
execCode(Attacker, Host, User) :-
    networkService(Host, Program, Protocol, Port, User),
    vulExists(Host, VulID, Program, remoteExploit, privEscalation),
    netAccess(Attacker, Host, Protocol, Port).
```

Predicates are either **primitive** (configuration facts from a scanner) or **derived** (computed
by iterating the rules). The attack graph is then the *derivation graph* of a successful Datalog
query, with two node types:

```
   ▭ derivation node = one rule application  = AND (all children needed)
   ◯ fact node       = one attribute         = OR  (any derivation suffices)
   ● primitive fact  = a leaf, from the scanner
```

The engine is XSB, a **tabled** Prolog: tabling memoizes subgoals, which both terminates on
cyclic rules and computes *all* answers, so a single query traversal yields every derivation.
`assert_trace` is bolted onto each rule to record the derivation steps, and a linear pass over
the trace builds the graph (Fig 6).

Complexity, all three theorems worth knowing:

- **Theorem 1**: evaluating the rules over N hosts takes **O(N²)** derivation steps — the
  worst rule has two host variables, so N² instantiations.
- **Theorem 2**: the logical attack graph has size **O(N²)** — one derivation node per trace step,
  bounded fan-in.
- **Theorem 3**: graph building is **O(δN²)** where δ is the table-lookup cost; with a
  `std::map` that is `log(N²)`, giving **O(N² log N)**.

And the measurement, on a Pentium 4 with 1 GB of RAM: attack graphs for **fully connected
networks of 1000 machines**, CPU growing between O(N²) and O(N³) depending on topology. Fig 14
puts MulVAL beside Sheyner's toolkit on the same inputs: Sheyner's is off the chart by 10 hosts;
MulVAL is at ~1 second at 50.

### Step 9 — Cycles and "useless edges"

Tabling stops the *evaluation* from looping, but the recorded trace can still contain cycles,
because two rules can be mutually satisfiable:

```prolog
accessFile(A, H, write, Path) :- execCode(A, H, root).
execCode(A, H, root)          :- accessFile(A, H, write, Path).
```

Both fire, both traces get written, and the graph has a back edge that means nothing — the reason
node 2 is true is that node 3 is true, not the other way round. The paper's fix is a definition
rather than a DFS heuristic (which it shows is wrong, Fig 8): an edge `(u,v)` is **useless** if
`v` is still derivable after removing `u`. Testing that per edge is quadratic in graph size. If
you have read topic 27, this is stratified negation and provenance-tracking territory, and the
"useless edge" test is a why-provenance query.

## How to read the papers (with the concepts in hand)

**Ammann, Wijesekera & Kaushik, CCS'02** (8 pages — read it all, it is short):

- **§1 Introduction.** The Sheyner numbers (5948 nodes / 68364 edges / 229 bits / 2 hours) are
  here. Note the sentence "at most, 229 nodes" — that is the whole paper.
- **§2 opening + Figure 1.** The `rcp` exploit as pre/postcondition sets. Read against Step 2.
- **§2 monotonicity paragraphs.** The three implications and the `port forward` / `code green`
  caveats. Ask yourself which of your own edge kinds in `ad_graph.rs` are non-monotone.
- **§2.1 Model + Figure 2.** The attribute layering. Confirm layers are BFS distance.
- **§2.2 Analysis.** The four boxed algorithms. Derive the `O(|A|²·|E|)` bound yourself from the
  two bullet points — it is two lines.
- **§2.3 Application.** Three sentences. This is lane 2.
- **§3 Example.** The 60-attribute / 30-exploit instantiation, then the observation that only
  **8 attributes out of 54** ever change value in an attack on the goal. That sparsity is why the
  approach works.

**Ou, Boyer & McQueen, CCS'06** (10 pages):

- **§1.1 + Figure 1.** The Datalog interaction rule. If you have not written Prolog, read the
  `:-` as a reversed implication and move on.
- **§2 Related work.** The paragraph on Ammann's O(|A|²·|E|) → "this will give us an O(N⁶)
  complexity" is the setup for their O(N²) claim. Note the honesty: it is a conservative bound
  on someone else's algorithm.
- **§3 + Figures 4, 5.** Derivation nodes vs fact nodes, AND vs OR. Draw the two-derivation
  example (node 2 with children r2a and r2b) yourself.
- **§4 + Figure 6.** The graph-building algorithm — ten numbered lines. §4.1 on loops.
- **§5.** Theorems 1–3. Each proof is a paragraph.
- **§6 + Figures 9–14.** The scaling data. Figure 14 is the money shot; Figure 12 (derived fact
  nodes grow *linearly*, while trace steps grow quadratically) is the more interesting one.
- **After the papers.** Implement the choke-point half in `chokepoint.rs` and reproduce lane 2's
  tiered/flat contrast — a directory where the top choke point covers 99.6% of exposure, and the
  same directory where no single cut frees anyone.

## Questions to answer in notes.md

1. Take three edge kinds from `ad_graph.rs` (`MemberOf`, `HasSession`, `GenericAll`) and decide
   whether each is monotone. `HasSession` is the interesting one — sessions expire. What does
   modelling it monotonically over-report, and is that the safe direction to err?
2. Ammann bounds his algorithm at `O(|A|²·|E|)`; MulVAL argues attributes are quadratic in hosts,
   hence `O(N⁶)`, and claims `O(N²)` instead. Where exactly does the extra work go — is MulVAL
   doing less work, or counting a different thing?
3. Reconstruct the layer numbers for the paper's three-host example (§3, the 8 attributes with
   their rounds). Then explain why `findShort` needs the layer numbers but `findMinimal` does not.
4. A logical attack graph is the derivation graph of a Datalog query. Sketch how you would
   maintain it *incrementally* under a stream of configuration changes (a host is patched, a
   firewall rule changes). Which topic-27 machinery applies, and what does a *retraction* mean
   given that the underlying analysis is monotone?
5. Lane 2 finds no single-node choke point in the flat directory. Restate that result in Ammann's
   vocabulary: what does it say about the minimum cut set, and why does the dominator formulation
   return nothing rather than returning the answer slowly?

## Done when

- [ ] You can state monotonicity in one sentence and list its three technical consequences.
- [ ] You can derive `O(|A|²·|E|)` from the two facts the paper gives.
- [ ] You can explain the difference between minimal and minimum attacks, and which is NP-complete.
- [ ] You can draw a logical attack graph with both node types and say which is AND and which OR.
- [ ] You can quote §2.3's cut-set paragraph and connect it to the dominator tree in `chokepoint.rs`.
- [ ] Your `chokepoint.rs` reproduces lane 2: exact agreement with the naive oracle on every node,
      and the tiered/flat contrast.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Ammann, Wijesekera, Kaushik. *Scalable, Graph-Based Network Vulnerability Analysis.* CCS 2002,
  pp. 217–224.
- Ou, Boyer, McQueen. *A Scalable Approach to Attack Graph Generation.* CCS 2006 —
  [PDF](https://cse.usf.edu/~xou/publications/ccs06.pdf).
- Sheyner, Haines, Jha, Lippmann, Wing. *Automated Generation and Analysis of Attack Graphs.*
  IEEE S&P 2002 — the model-checking approach monotonicity replaced.
- Local exercise stub: `topics/40-security-attack-graphs/experiments/chokepoint.rs` — §2.3's cut
  sets as a dominator tree.
- Topic 27 (streaming & IVM) — Datalog fixpoints, provenance, and what incremental maintenance of
  a derivation graph would take.
