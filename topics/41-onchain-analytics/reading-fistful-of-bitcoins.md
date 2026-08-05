# A Fistful of Bitcoins: two heuristics, and why only one of them is safe

Bitcoin's ledger is public and its participants are pseudonymous, which sounds like a stalemate
until you notice that people *use* wallets, and use leaves fingerprints. Meiklejohn et al. bought
things — a used Boston CD, coffee, silver quarters — across 344 transactions with 30-odd
services, then used those known addresses as seeds and let two clustering heuristics do the rest.
The paper matters here for a reason beyond its result: the two heuristics have opposite risk
profiles, and understanding *why* is a lesson about any system that merges records irreversibly.
One keys on a property of the protocol and cannot be wrong. The other keys on a habit, and being
wrong once welds two strangers together forever.

Every number below is quoted from *A Fistful of Bitcoins* (Meiklejohn et al., IMC 2013): the parse
counts and cluster counts from §2–§4, the false-positive ladder from §4.5, the service figures from
§5. Where a figure is one this repo measured instead, it is labelled as lane 3 and traces to this
topic's `notes.md` and [`../../FINDINGS.md`](../../FINDINGS.md).

## The problem in one sentence

**Twelve million public keys are not twelve million people — but deciding which of them are one
person is an inference from behaviour, and a merge you get wrong can never be undone.**

## The concepts, step by step

### Step 1 — Address is not identity, and neither is a cluster

> **In:** the public Bitcoin blockchain parsed to 13 April 2013.
> **Out:** 12,056,684 distinct public keys and a working definition of *control* — the relation the
> two heuristics will cluster on. Step 2 is the first heuristic.

A Bitcoin **address** is a public key; anyone can make as many as they like, for free, and wallet
software does exactly that. (A **public key** is one half of a cryptographic keypair; spending
money sent to it requires the matching private key, so possession of that private key is what
"control" ultimately means.) The paper's parse of the chain to 13 April 2013 found **231,207
blocks, 16,086,073 transactions and 12,056,684 distinct public keys** — for a user base orders of
magnitude smaller.

The paper is careful about what a cluster means, and you should be too. It defines **control**, not
ownership — the entity that can sign for an address, which is not necessarily its economic owner:
"the controller of an address is the entity (or in exceptional cases multiple entities) that is
expected to participate in transactions involving that address." If you buy a physical bitcoin from
a vendor who knows the private key, and then redeem it at Mt. Gox, three parties have known that
key. Clustering answers "who transacts with this", which is what an investigator wants and is not
the same as "who owns this".

### Step 2 — Heuristic 1: co-spending is a protocol property

> **In:** the transaction set from Step 1, treated as a hypergraph whose edges are transactions.
> **Out:** a partition of the 12M public keys into 5,579,176 co-spend clusters — the *safe* half of
> the method. Step 3 sets up the second, riskier heuristic.

> **Heuristic 1.** If two (or more) addresses are inputs to the same transaction, they are
> controlled by the same user.

Because spending an output requires a signature from its key, whoever assembled a transaction
with inputs A and B held both private keys. The relation is transitive — if one transaction
joins {A, B} and another joins {B, C}, then A, B and C are one user — so the whole computation is
a **union-find over the co-spend hypergraph**, one linear pass over the transactions. (**Union-find**
is the near-linear disjoint-set algorithm for merging groups under "these two belong together"; a
**hypergraph** is a graph whose edges can join more than two vertices at once, which is exactly
what a multi-input transaction is — one edge over all its input addresses.)

The paper's phrasing of why it is safe is worth keeping: "it is also quite safe: the sender in
the transaction must know the private signing key belonging to each public key used as an input,
so it is unlikely that the collection of public keys are controlled by multiple entities (as these
entities would need to reveal their private keys to each other)."

Result on the 2013 chain: **12,056,684 public keys → 5,579,176 clusters**. Accounting for "sink"
addresses that never spent, at most 6,595,564 distinct users, "although we consider this number a
quite large upper bound."

Lane 3 of this topic's crate reproduces the shape: 12,186 addresses → 5,638 clusters, and
**precision exactly 1.000 at every parameter setting**, because the generator never lets two
entities co-spend. That is not the crate being kind — it is the heuristic being sound.

### Step 3 — Change addresses, and why they leak

> **In:** the co-spend clusters from Step 2, still missing the links that co-spending never
> reveals.
> **Out:** the *change address* — the fresh address a wallet sends surplus to — as the leak a
> second heuristic can exploit. Step 4 states that heuristic precisely.

A payment rarely matches a UTXO exactly. Spending a 10 BTC output to pay 3 BTC means creating two
outputs: 3 to the payee and 7 back to yourself, at a *fresh* address the wallet generated. That
fresh self-directed output is the **change address**: a brand-new address, made by the sender's own
wallet, holding the leftover of a spend. The paper's Definition 4.2 makes the underlying fact
precise — "a public key can therefore spend money only as many times as it has received money
(again, because each time it spends money it must spend all of it at once)."

If you can pick out which output is the change, you have linked the sender's brand-new address to
the addresses they just spent from, and you can keep doing it forever. That is the prize; the
next step is the trap.

### Step 4 — Heuristic 2: Definition 4.3, all four conditions

> **In:** each transaction `t`, and the co-spend clusters from Step 2.
> **Out:** at most one output of `t` labelled its *one-time change address*, adding a new edge to
> the cluster graph — or a refusal when the transaction is ambiguous. Step 5 measures how often
> that label is wrong.

A public key `pk` is a *one-time change address* for a transaction `t` when:

```
   1. d+_addr(pk) = 1              this is the first appearance of pk
   2. t is not a coin generation
   3. no pk' ∈ outputs(t) is also in inputs(t)     — no self-change
   4. no OTHER output pk' ≠ pk also satisfies condition 1
```

Condition 4 is the one people forget, and it is the heuristic's conscience: if two outputs are
both brand-new addresses, there is no way to tell the payment from the change, and the heuristic
must decline. The crate's `definition_4_3_refuses_ambiguous_transactions` test exists to make you
implement that refusal rather than guessing.

Condition 3 exists because self-change is common — the paper measured **23% of all transactions**
in the preceding six months as self-change, that being the default for the wallet service My
Wallet and how Deepbit paid its miners.

The paper is blunt about the difference in kind: this heuristic "takes advantage of a peculiarity
of usage rather than an inherent property of the Bitcoin protocol (as Heuristic 1 does), it does
lack robustness in the face of changing (or adversarial) patterns in the network."

### Step 5 — The false-positive ladder: precision bought with latency

> **In:** the change labels Heuristic 2 assigns at each block height (Step 4).
> **Out:** a false-positive rate per refinement — 13% down to 0.17% — measured behaviourally, and
> the lesson that latency buys precision. Step 6 shows why even 0.17% is dangerous.

The authors had no ground truth, so they measured the false-positive rate *behaviourally*: if an
address met Definition 4.3 at block height h — meaning it looked like a one-time change address —
and was then used again later, the label was wrong.

```
   naive Definition 4.3 ................ 555,348 false positives = 13% of labels
   + ignore the Satoshi Dice payout pattern ................... 1%
   + wait a DAY before labelling ............................ 0.28%
   + wait a WEEK before labelling ......... 0.17%  (7,382 addresses)
```

Read that as an engineering result, not a footnote: the same heuristic is 76× more precise if you
are willing to wait a week to apply it. Precision is being bought with latency, which is the
trade every streaming system in topic 27 makes and every online/offline split in topic 32 makes.
Exercise 5 of this topic asks you to reproduce the curve.

### Step 6 — Cluster collapse: why a 0.17% error rate is still dangerous

> **In:** the 0.17% mislabelled change addresses from Step 5, fed into the union-find of Step 2.
> **Out:** the reason a tiny *label* error rate becomes a catastrophic *partition* error — worked
> as pair-precision arithmetic below. Step 7 weighs the payoff against this risk.

Even after all of that, the refined run produced "a giant super-cluster containing the public keys
of Mt. Gox, Instawallet, BitPay, and Silk Road, among others; in total, this super-cluster
contained **1.6 million public keys**."

Two mechanisms caused it, and both are worth knowing because they are wallet behaviours, not bugs
in the definition:

1. The same change address used twice within a short window. The second use makes the *new*
   address look like the change address, and it is falsely labelled.
2. Self-change addresses (allowed by advanced wallets like Armory and My Wallet) later used
   separately with a new address, so the new address is falsely labelled.

The deep problem is that union-find is transitive and has no undo. Measure cluster quality by
**pair precision** — of all address *pairs* placed in the same cluster, the fraction that really
are the same user — and **pair recall** — of all pairs that really are the same user, the fraction
grouped together. A false merge and a missed merge damage these asymmetrically:

```
   Two real users, 1,000 addresses each. Correct same-user pairs = 2 × C(1000,2) = 999,000.

   ONE false merge unites them into one 2,000-address cluster:
       same-cluster pairs = C(2000,2)              = 1,999,000
       of which cross-user (all wrong) = 1000×1000 = 1,000,000
       pair precision = 999,000 / 1,999,000        ≈ 0.50     ← one mistake halves precision

   ONE missed merge instead splits a true 1,000-cluster into 500+500:
       lost true pairs = 500×500                   =   250,000
       pair recall = (999,000−250,000) / 999,000   ≈ 0.75     ← precision stays 1.000
```

A false merge creates `a×b` wrong pairs (multiplicative in the cluster sizes); a missed merge only
withholds pairs (it costs recall, never precision). That asymmetry is why a heuristic with recall
0.04 and precision 1.000 is more useful than one with recall 0.45 and precision 0.09 — you can
always union more clusters later, but you can never un-merge a wrong one.

Lane 3 of the crate plants exactly mechanism 1 and sweeps it:

```
   change reuse   H1+2 precision   largest cluster
           0.00            1.000    93  (1% of addresses)
           0.01            0.661   366  (3%)
           0.05            0.089  1894  (16%)
           0.10            0.009  7991  (71%)
```

One reused change address in a hundred costs a third of the precision. One in ten and 71% of the
chain is a single cluster. BlockSci, on the 2019 chain, reports a supercluster of **over 17
million addresses** and says it is "likely a result of such a collapse."

### Step 7 — Why the clusters were worth it anyway

> **In:** the co-spend + change clusters (Steps 2–6) seeded with 1,070 hand-tagged addresses.
> **Out:** 2,197 named clusters over 1.8M addresses, and the structural finding that services are
> chokepoints. This closes the paper's argument that pseudonymity leaks at the exchange.

The payoff is leverage. Hand-tagging 1,070 addresses through 344 transactions, then clustering,
let the authors name **2,197 clusters accounting for over 1.8 million addresses** — "Heuristic 2
allowed us to name 1,600 times more addresses than our own manual observation provided."

And the structural finding, §5: services are chokepoints. Satoshi Dice alone accounted for about
**60% of all Bitcoin activity** at the time, and **21% of all bets (896,864 of 4,127,979)** were
exactly the 0.01 BTC minimum. Exchanges are chokepoints too, which is what makes the whole
enterprise matter: "the demonstrated centrality of these services makes it difficult for even
highly motivated individuals — e.g., thieves or others attracted to the anonymity properties of
Bitcoin — to stay completely anonymous, provided they are interested in cashing out."

## How to read the paper (with the concepts in hand)

- **§2.3 Bitcoin network statistics.** The parse numbers (231,207 blocks / 16M transactions /
  12M keys) and Figures 2–3. Note the observation that 64% of all bitcoins had never been spent.
- **§3 Data collection.** Skim, but read it once for texture — this is a paper where the authors
  bought a Boston CD to get ground truth. Table 1 is the service taxonomy the rest depends on.
- **§4.1 Defining account control.** Short, and the definitional care is the point. Read against
  Step 1.
- **§4.2 Graph structure.** Definitions 4.1 and 4.2. The "must spend it all at once" property in
  4.2 is *why* change addresses exist; make sure you see that before moving on.
- **§4.3 Heuristics.** Heuristic 1 and its safety argument, then Heuristic 2 and Definition 4.3.
  Check each of the four conditions against Step 4, and predict what breaks if you drop condition
  4 — then check your prediction against the crate's ambiguity test.
- **§4.5 Refining Heuristic 2.** The false-positive ladder and the super-cluster. This is the most
  useful page in the paper for a systems engineer. The two failure mechanisms are named
  explicitly; find them.
- **§5 Service centrality.** Satoshi Dice's 60% of activity, and §5.2's argument about exchanges
  as chokepoints.
- **After the paper.** Implement `multi_input_clusters`, `change_output` and `full_clusters` in
  `clustering.rs`, then reproduce lane 3's collapse curve. Then do exercise 5 — the
  wait-before-labelling delay — and see whether you can recover the paper's 76× precision gain.

## Questions to answer in notes.md

1. Heuristic 1 is "safe" because faking it requires sharing private keys. Name one real-world
   construction that breaks it anyway, and say what BlockSci does about it (hint: the paper is
   from 2013 and the construction is from 2013 too).
2. Drop condition 4 from Definition 4.3 — always label the *first* fresh output as change. Predict
   the effect on precision and recall in lane 3 at reuse rate 0, then measure it. Was the
   direction obvious?
3. A false merge is permanent and transitive; a missed merge is not. Write the arithmetic: for a
   partition of `n` addresses into clusters, how does one false merge affect pair precision
   compared to one missed merge affecting pair recall? Use lane 3's numbers.
4. The false-positive rate drops 13% → 0.17% by waiting a week. What is the operational cost of
   that week for (a) an exchange screening a deposit, (b) a law-enforcement investigation, (c) a
   research paper? Which of the three should use the naive version?
5. Lane 3 shows co-spend recall of only 0.041 while the change heuristic reaches 0.397. Argue
   both sides of "ship the safe one only", and say which you would ship at an exchange and why.

## Done when

Answer each before unfolding it.

- [ ] You can state both heuristics and explain why one is a protocol property and one is not.
  <details><summary>Answer</summary>

  Heuristic 1: addresses that are inputs to the same transaction share a controller — safe, because
  co-spending requires holding every input's private *signing* key, so faking it means strangers
  swapping private keys (Step 2). Heuristic 2: the one-time change address of Definition 4.3 — a
  *usage* pattern, not a protocol rule, so it "lack[s] robustness in the face of changing (or
  adversarial) patterns" (Step 4).
  </details>
- [ ] You can recite Definition 4.3's four conditions and say what each one rules out.
  <details><summary>Answer</summary>

  (1) `pk` appears for the first time — change is a fresh address. (2) `t` is not a coin generation
  — coinbase outputs are not change. (3) No output address is also an input — rules out self-change
  (23% of transactions). (4) No *other* output is also brand-new — forces the heuristic to decline
  when it cannot tell payment from change (Step 4). Condition 4 is the one people forget.
  </details>
- [ ] You can explain why union-find makes a low label-error rate into a high partition-error rate.
  <details><summary>Answer</summary>

  Merges are transitive and cannot be undone, so one false merge of clusters sized a and b creates
  a×b wrong same-user pairs. Two 1,000-address clusters wrongly merged make 1,000,000 false pairs
  and drop pair precision from 1.000 to ≈0.50 in a single mistake, while a missed merge only costs
  recall (Step 6). Errors compound multiplicatively; corrections only add.
  </details>
- [ ] You can quote the false-positive ladder and name what buys each step.
  <details><summary>Answer</summary>

  Naive Definition 4.3: 555,348 false positives = 13%. Ignore the Satoshi Dice payout pattern → 1%.
  Wait a day before labelling → 0.28%. Wait a week → 0.17% (7,382 addresses) (Step 5). Precision is
  bought with latency — the same heuristic is ~76× more precise if you are willing to wait a week.
  </details>
- [ ] Your `clustering.rs` reproduces lane 3: precision 1.000 for co-spend at every reuse rate,
      and the 1.000 → 0.089 → 0.009 collapse for the change heuristic.
  <details><summary>Answer</summary>

  Co-spend (Heuristic 1) precision stays 1.000 at every change-reuse rate because the generator
  never lets two entities co-spend. The change heuristic collapses as reuse rises: precision 1.000
  at 0.00 (largest cluster 93), 0.661 at 0.01 (366), 0.089 at 0.05 (1894), 0.009 at 0.10 (7991 =
  71% of addresses) (Step 6, this topic's `notes.md`).
  </details>
- [ ] You wrote answers to all five questions in notes.md.
  <details><summary>Answer</summary>

  Done when notes.md holds your five written answers — a real construction that breaks Heuristic 1
  and BlockSci's response, the effect of dropping condition 4, the pair-precision-vs-recall
  arithmetic for one false vs one missed merge, the operational cost of the week's delay for three
  actors, and which single heuristic you would ship at an exchange.
  </details>

## References

- Meiklejohn, Pomarole, Jordan, Levchenko, McCoy, Voelker, Savage. *A Fistful of Bitcoins:
  Characterizing Payments Among Men with No Names.* IMC 2013 —
  [PDF](https://cseweb.ucsd.edu/~smeiklejohn/files/imc13.pdf).
- Androulaki et al. (2013) — "shadow addresses", the earlier change heuristic Definition 4.3
  replaces; the paper explains why its assumptions no longer held.
- Local exercise stub: `topics/41-onchain-analytics/experiments/clustering.rs`.
- Topic 39 (fraud & identity graphs) — the same union-find over the same pair-precision metric,
  with learned weights instead of hand-written conditions.
