# A Fistful of Bitcoins: two heuristics, and why only one of them is safe

Bitcoin's ledger is public and its participants are pseudonymous, which sounds like a stalemate
until you notice that people *use* wallets, and use leaves fingerprints. Meiklejohn et al. bought
things — a used Boston CD, coffee, silver quarters — across 344 transactions with 30-odd
services, then used those known addresses as seeds and let two clustering heuristics do the rest.
The paper matters here for a reason beyond its result: the two heuristics have opposite risk
profiles, and understanding *why* is a lesson about any system that merges records irreversibly.
One keys on a property of the protocol and cannot be wrong. The other keys on a habit, and being
wrong once welds two strangers together forever.

## The problem in one sentence

**Twelve million public keys are not twelve million people — but deciding which of them are one
person is an inference from behaviour, and a merge you get wrong can never be undone.**

## The concepts, step by step

### Step 1 — Address is not identity, and neither is a cluster

A Bitcoin address is a public key; anyone can make as many as they like, for free, and wallet
software does exactly that. The paper's parse of the chain to 13 April 2013 found **231,207
blocks, 16,086,073 transactions and 12,056,684 distinct public keys** — for a user base orders of
magnitude smaller.

The paper is careful about what a cluster means, and you should be too. It defines *control*, not
ownership: "the controller of an address is the entity that is expected to participate in
transactions involving that address." If you buy a physical bitcoin from a vendor who knows the
private key, and then redeem it at Mt. Gox, three parties have known that key. Clustering answers
"who transacts with this", which is what an investigator wants and is not the same as "who owns
this".

### Step 2 — Heuristic 1: co-spending is a protocol property

> **Heuristic 1.** If two (or more) addresses are inputs to the same transaction, they are
> controlled by the same user.

Because spending an output requires a signature from its key, whoever assembled a transaction
with inputs A and B held both private keys. The relation is transitive — if one transaction
joins {A, B} and another joins {B, C}, then A, B and C are one user — so the whole computation is
a **union-find over the co-spend hypergraph**, one linear pass over the transactions.

The paper's phrasing of why it is safe is worth keeping: "it is also quite safe: the sender in
the transaction must know the private key belonging to each public key used as an input, so it is
unlikely that the collection of public keys are controlled by multiple entities (as these
entities would need to reveal their private keys to each other)."

Result on the 2013 chain: **12,056,684 public keys → 5,579,176 clusters**. Accounting for "sink"
addresses that never spent, at most 6,595,564 distinct users, "although we consider this number a
quite large upper bound."

Lane 3 of this topic's crate reproduces the shape: 12,186 addresses → 5,638 clusters, and
**precision exactly 1.000 at every parameter setting**, because the generator never lets two
entities co-spend. That is not the crate being kind — it is the heuristic being sound.

### Step 3 — Change addresses, and why they leak

A payment rarely matches a UTXO exactly. Spending a 10 BTC output to pay 3 BTC means creating two
outputs: 3 to the payee and 7 back to yourself, at a *fresh* address the wallet generated. The
paper's Definition 4.2 makes the underlying fact precise — "a public key can therefore spend
money only as many times as it has received money (again, because each time it spends money it
must spend all of it at once)."

If you can pick out which output is the change, you have linked the sender's brand-new address to
the addresses they just spent from, and you can keep doing it forever. That is the prize; the
next step is the trap.

### Step 4 — Heuristic 2: Definition 4.3, all four conditions

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

Even after all of that, the refined run produced "a giant super-cluster containing the public keys
of Mt. Gox, Instawallet, BitPay, and Silk Road, among others; in total, this super-cluster
contained **1.6 million public keys**."

Two mechanisms caused it, and both are worth knowing because they are wallet behaviours, not bugs
in the definition:

1. The same change address used twice within a short window. The second use makes the *new*
   address look like the change address, and it is falsely labelled.
2. Self-change addresses (allowed by advanced wallets like Armory and My Wallet) later used
   separately with a new address, so the new address is falsely labelled.

The deep problem is that union-find is transitive and has no undo. A 0.17% error rate on *labels*
is not a 0.17% error rate on the *partition*: each false merge fuses two whole components, so
errors compound multiplicatively while correct merges only add. This is the arithmetic that makes
a heuristic with recall 0.04 and precision 1.000 more useful than one with recall 0.45 and
precision 0.09.

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

The payoff is leverage. Hand-tagging 1,070 addresses through 344 transactions, then clustering,
let the authors name **2,197 clusters accounting for over 1.8 million addresses** — "Heuristic 2
allowed us to name 1,600 times more addresses than our own manual observation provided."

And the structural finding, §5: services are chokepoints. Satoshi Dice alone accounted for about
**60% of all Bitcoin activity** at the time, and **21% of all bets (896,864 of 4,127,979)** were
exactly the 0.01 BTC minimum. Exchanges are chokepoints too, which is what makes the whole
enterprise matter: "the demonstrated centrality of these services makes it difficult for even
highly motivated individuals — e.g., thieves or others strongly attracted to the anonymity
properties of Bitcoin — to stay completely anonymous, if they are interested in cashing out."

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

- [ ] You can state both heuristics and explain why one is a protocol property and one is not.
- [ ] You can recite Definition 4.3's four conditions and say what each one rules out.
- [ ] You can explain why union-find makes a low label-error rate into a high partition-error rate.
- [ ] You can quote the false-positive ladder and name what buys each step.
- [ ] Your `clustering.rs` reproduces lane 3: precision 1.000 for co-spend at every reuse rate,
      and the 1.000 → 0.089 → 0.009 collapse for the change heuristic.
- [ ] You wrote answers to all five questions in notes.md.

## References

- Meiklejohn, Pomarole, Jordan, Levchenko, McCoy, Voelker, Savage. *A Fistful of Bitcoins:
  Characterizing Payments Among Men with No Names.* IMC 2013 —
  [PDF](https://cseweb.ucsd.edu/~smeiklejohn/files/imc13.pdf).
- Androulaki et al. (2013) — "shadow addresses", the earlier change heuristic Definition 4.3
  replaces; the paper explains why its assumptions no longer held.
- Local exercise stub: `topics/41-onchain-analytics/experiments/clustering.rs`.
- Topic 39 (fraud & identity graphs) — the same union-find over the same pair-precision metric,
  with learned weights instead of hand-written conditions.
