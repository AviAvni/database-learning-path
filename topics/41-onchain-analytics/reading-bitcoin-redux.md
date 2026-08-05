# Bitcoin Redux: an 1816 court case fixes taint tracking

Most systems papers propose an algorithm. This one proposes a *precedent*. Anderson, Shumailov,
Ahmed and Rietmann set out to build a blacklist of stolen bitcoin, found that the industry's
standard taint-tracking rule smears a single theft across 93% of all addresses, and then
discovered that English lawyers had solved the same problem two hundred years earlier — when a
bank failed in 1816 and nobody could say which deposits had funded which withdrawals. The rule
the Master of the Rolls set down, first-in-first-out, turns out to be not just the legal answer
but the *computationally* right one, because it is lossless. This guide reads the paper alongside
RustyTaintChain, the authors' Rust implementation, whose core is fifteen lines.

Every code anchor below is RustyTaintChain at commit `4e12fd0` (the revision this repo pins),
all of it in one file, `src/callbacks/bootstrap_taint_fifo.rs`, quoted with the line numbers the
code occupies in that version. Every paper number cites the section or figure of *Bitcoin Redux*
(Anderson, Shumailov, Ahmed & Rietmann, WEIS 2018) it came from. Where a figure is one this
repo measured, it is labelled as a bench lane and traces to
[`../../FINDINGS.md`](../../FINDINGS.md) row 41 and this topic's `notes.md`.

## The problem in one sentence

**Trace a stolen coin forward through a transaction graph where money is constantly split and
merged, and end up with an answer narrow enough to act on rather than a trace of taint on
everybody.**

## The concepts, step by step

### Step 1 — Why the question is legally live: `nemo dat`

> **In:** nothing yet — this step is the legal motivation for building a tracer at all.
> **Out:** the reason a *forward* taint trace has teeth: if a stolen coin can be followed, it
> can be reclaimed. Step 2 asks how to follow it.

**Nemo dat quod non habet** — "no one gives what they do not own" — is the principle that you
cannot pass better title to property than you yourself hold. It is part of nearly every legal
system. "If Alice steals Bob's horse and sells it to Charlie, Charlie doesn't end up owning it;
when Bob sees him riding it, he can simply demand it back."

The rule has exceptions. **Market overt** — an old rule that buying openly in a recognised public
market gives you good title even to stolen goods — was the one that used to matter; Britain
abolished it in 1995 after thieves abused it to launder stolen antiques (§2). Two exceptions
remain, for **money** and for **bills of exchange** (a bill of exchange is a transferable written
order to pay, like a cheque). A **commodity**, by contrast, is an ordinary tradable good with no
such exception — and the USA has designated bitcoin a commodity, not money. So: "Unless
cryptocurrencies acquire this privileged status, there is no general exception to the nemo dat
rule — so a theft victim can pursue and retrieve his stolen property."

That is why taint tracking is not an academic exercise. If a stolen coin can be followed, it can
be reclaimed, and every exchange that touched it has a problem. Note the framing is careful: the
paper *assumes* bitcoin is a commodity (§2, "In what immediately follows, we will assume that
bitcoin is a commodity") and reasons from there — it does not claim FIFO tracing is settled law.
The conditional is the honest version, and this guide keeps it.

### Step 2 — Poison and haircut, and what the default does

> **In:** the graph of transactions, and one output flagged stolen (Step 1's premise).
> **Out:** for every UTXO, a *taint fraction* between 0 and 1 — under two rival rules. This is
> the dataset Step 3 replaces with a lossless alternative.

A **taint policy** is a rule for propagating "this money is stolen" across a transaction that
mixes tainted and clean inputs. Möser, Böhme and Breuker (their 2014 risk-scoring paper, [MBB14],
cited by *Bitcoin Redux* §3.1) named the two the industry actually uses:

- **Poison**: if *any* input to a transaction is tainted, *every* output is entirely tainted
  (taint fraction 1.0). Taint spreads like a contagion and never dilutes.
- **Haircut**: each output is tainted by the *fraction* of input value that was tainted. A
  **taint fraction** is the share of an output's value that traces to the theft, a real number in
  [0, 1].

Haircut became the default. Here is what it does, traced forward over real thefts to 2016
(*Bitcoin Redux* §3.3):

```
   2012 Linode theft, 46,653 BTC
     haircut ..... 16,855,619 addresses tainted — "just over 93% of the total"
     FIFO ............ 245,120 addresses tainted — "just over 1.35%"

   2014 Flexcoin hack ("the world's first bitcoin bank")
     haircut ..... 10,421,112 addresses — "over 57% of all addresses"
     FIFO ............. 15,265 accounts
```

**Why haircut smears everything — worked by hand.** Haircut's fraction is
`out_taint = in_taint / in_total`, where `in_taint` is the tainted satoshis flowing in and
`in_total` is all satoshis flowing in. Follow one stolen coin through merges that each add nine
times as much clean money (a **merge** is a transaction whose inputs include both tainted and
clean UTXOs):

```
   stolen coinbase: 1,000,000 sat, fraction 1.000  (100% tainted)

   hop 1:  1,000,000 tainted  +  9,000,000 clean  = 10,000,000 in
           fraction = 1,000,000 / 10,000,000       = 0.100   (10%)
   hop 2:  same 10x dilution                        = 0.010   (1%)
   hop 3:  same 10x dilution                        = 0.001   (0.1%)

   total tainted value, summed over ALL outputs at every hop = 1,000,000 sat  (conserved)
```

Three merges of ten-fold dilution and a descendant coin is **0.1% tainted** — below any threshold
worth acting on — yet the theft has now touched every output on all three hops. The total tainted
value never changes (haircut conserves it to the satoshi; it does not invent money), it is just
smeared thinner at each hop until "is this coin tainted?" stops meaning anything.

Lane 1 of this topic's crate reproduces exactly that mechanism on a synthetic chain: one stolen
coinbase worth 0.25% of all the money ends up tainting **97.9% of the UTXO set (3657 of 3734) and
98.0% of addresses (3553 of 3627)**, and of those tainted UTXOs **658 carry less than 0.1% taint,
2,997 carry between 0.1% and 5%, and exactly two carry more than 5%** ([FINDINGS.md](../../FINDINGS.md)
row 41). The 658 sub-0.1% UTXOs are the coins three-or-more dilutions downstream in the worked
example above. That is the headline of this topic: 98% of everybody holds a trace, and two UTXOs
in the whole chain hold a share worth arguing about.

The paper's summing up: "'haircut' tainting smears the taint over the actively traded bitcoin
stock. Bitcoin laundries are designed to make this even worse." And the consequence, stated
without hedging: "the effect of aggressive asset recovery via regulated exchanges might be more
akin to a tax on all users."

### Step 3 — Clayton's Case, 1816

> **In:** the same merging transactions Step 2 fed to haircut.
> **Out:** for every output, a *queue of satoshi runs* tagged stolen-or-clean instead of a single
> fraction. Step 4 shows why that representation is the whole argument.

**FIFO (first-in-first-out)** is the rule that the earliest money in is the earliest money out.
Applied to taint it is a two-hundred-year-old legal precedent, not an algorithm the authors
invented — **Clayton's Case** (formally *Devaynes v Noble*, 1816), which is a rule of English
equity, adopted here as an accounting convention rather than as settled cryptocurrency law:

> In English law, there is a long-standing legal precedent on tracing stolen funds. It was
> established in 1816, when a court had to tackle the problem of mixing funds after a bank went
> bust and its obligations relating to one customer account depended on what sums had been
> deposited and withdrawn in what order before the insolvency. Clayton's case sets a simple rule
> of first-in-first-out (FIFO): withdrawals from an account are deemed to be drawn against the
> deposits first made to it.

Applied to a transaction: lay the input satoshis end to end in input order, then cut the outputs
off the front of that queue in output order.

```
   inputs                    FIFO outputs              haircut outputs
   ┌──────┐ clean  3         ┌──────┐ D: 3 clean       D: 2/9 stolen
   ├──────┤ STOLEN 2   ==>   ├──────┤ E: 2 STOLEN  vs  E: 2/9 stolen
   ├──────┤ clean  4         ├──────┤ F: 4 clean       F: 2/9 stolen
   └──────┘                  └──────┘
```

Read the diagram against Step 2's arithmetic: haircut gives all three outputs the same
`2/9 = 0.222` fraction, so the taint is *everywhere and dilute*; FIFO puts the whole 2 stolen
satoshis on output E and leaves D and F provably clean, so the taint is *somewhere and exact*.
The paper's Figures 1–3 draw this for poison, haircut and FIFO with four colours of tainted
input. Note what FIFO does that the others cannot: "the taint does not go across in percentages,
but to individual components (indeed, individual Satoshis) of each output."

### Step 4 — Lossless, and therefore reversible

> **In:** the per-output satoshi-run queues from Step 3.
> **Out:** the property — losslessness — that makes those queues worth the extra storage. Step 5
> is the code that maintains them.

**Lossless** here means no information is destroyed at a merge: a satoshi stays labelled stolen or
clean, and nothing is rounded or averaged away. That is the property that matters and it is easy
to skate past:

> As the taint does not spread or diffuse, the transaction processes it in a lossless way. This
> means that we can trace a bitcoin's heritage backwards as well as tracing taint forwards, and
> we can do tracing extremely efficiently once the appropriate index tables have been built.

A satoshi under FIFO is stolen or it is not; there is no fractional state to accumulate rounding
in, and no information is destroyed at a merge. So provenance survives arbitrarily many hops, and
you can ask "where did *this particular* satoshi come from" as well as "where did the theft go".
Haircut destroys that on the first merge: 2/9 of 3/7 of 5/11 is a number, not a history — you
cannot invert a product of fractions back into which coin came from where.

**Deterministic** means two runs on the same chain produce the same answer bit-for-bit. FIFO is
deterministic (given a fixed input/output ordering), which matters legally as much as technically:
two investigators running FIFO on the same chain get the same answer, and can be cross-examined on
it.

### Step 5 — `extract_taint`: Clayton's Case in fifteen lines

> **In:** an output's provenance as a `VecDeque<TaintPart>` (Step 3's queue of runs), plus the
> number of satoshis this output claims.
> **Out:** a new queue holding exactly those satoshis, cut off the front of the input queue.
> Step 6 wraps two more operations around this one.

RustyTaintChain represents an output's provenance as a queue of runs. A **run** is a contiguous
block of satoshis that share one provenance — `name` identifies the source (0 = clean, other
values name distinct crimes), `value` counts the satoshis in the block:

```rust
// src/callbacks/bootstrap_taint_fifo.rs:51-55 — the run type
51  #[derive(PartialEq, Eq, Hash, Default, Debug, RustcDecodable, RustcEncodable, Clone)]
52  pub struct TaintPart {
53      name : u16,   // 0 = clean; other values name distinct crime sources
54      value: u64    // satoshis in this contiguous run
55  }
```

Line 53's `name: u16` is the whole reason FIFO is lossless (Step 4): it is a *label*, not a
fraction. The whole of the FIFO rule is then one function that cuts `value` satoshis off the
front of a queue, splitting the run that straddles the boundary:

```rust
// src/callbacks/bootstrap_taint_fifo.rs:142-172 — extract_taint (asserts on 149/154/159 elided)
142  fn extract_taint(given_taints: &mut VecDeque<TaintPart>, value: u64)->VecDeque<TaintPart>{
143      let mut remaining = value;
144      let mut new_tainted_balance = VecDeque::new();
146      while remaining > 0{
147          if given_taints.is_empty(){                          // branch 1: queue ran dry
148              new_tainted_balance.push_back(TaintPart{name: 0, value:remaining});  // rest is clean
150              remaining = 0;
151          }else{
152              let mut ctaint = given_taints.pop_front().unwrap();
153              if remaining >= ctaint.value{                    // branch 2: whole run fits
155                  remaining -= ctaint.value;
156                  new_tainted_balance.push_back(ctaint);
157              }else{                                           // branch 3: run straddles the cut
158                  ctaint.value -= remaining;
160                  new_tainted_balance.push_back(TaintPart{name:ctaint.name, value:remaining});
161                  given_taints.push_front(ctaint);             // put the remainder back
162                  remaining = 0;
163              }
164          }
165      }
167      if remaining > 0{                                        // belt-and-braces: any leftover is clean
168          new_tainted_balance.push_back(TaintPart{name:0, value: remaining});
169      }
171      return new_tainted_balance;
172  }
```

The line that carries the argument is **158–161**, branch 3: when the requested `value` lands in
the middle of a run, it splits the run, keeps the front piece with the *same* `name`, and pushes
the remainder back on the queue for the next output. Branch 1 (147–150) covers a queue that ran
dry — the rest is clean — and branch 2 (153–156) is the run that fits entirely. Getting branch 3
right is the whole exercise: the crate's `extract_taint_splits_runs_at_the_boundary` test asks for
4 satoshis out of a 10-satoshi stolen run and insists you get back a 4 and leave a 6.

Processing a transaction is then: concatenate the input queues in input order, call
`extract_taint` once per output in output order. Measured in lane 2: **20,400 transactions in
6.6 ms = 3.1 million transactions per second** (this topic's `notes.md`), because that is all it
is.

### Step 6 — The two operations `extract_taint` needs around it

> **In:** the output queues `extract_taint` produces (Step 5), fed back in as inputs to later
> transactions.
> **Out:** merged, coalesced queues that stay bounded in size. Step 7 leaves the code and returns
> to the paper's argument.

Real chains need two more pieces, both in the same file:

- **`combine_taints:174`** — when two provenance queues meet, runs with different `name`s collide,
  and the implementation counts those collisions. This is why `TaintPart.name` is a `u16` and not
  a `bool`: money from several crimes gets entangled. The paper notes it: "taint from theft
  becomes entangled with taint from drug trafficking and from the trade in cybercrime tools."
- **`reduce_taint:250`** — run-length coalescing. Without it, a queue fragments a little more at
  every hop and grows without bound. Adjacent runs with the same name merge back into one. That
  is exercise 3 of this topic, and it is the difference between a taint property you can store on
  a graph node and one you cannot.

### Step 7 — Why mixers make it worse, not better

> **In:** the FIFO tracer of Steps 3–6 plus the legal frame of Step 1.
> **Out:** the paper's counter-intuitive claim about **mixers** — services that pool many users'
> coins to break the on-chain link between input and output. Step 8 is the caveat that undermines
> the whole method, honestly.

The received wisdom is that a **mixer** (also *tumbler* or *laundry* — a service that pools coins
from many users and pays out unrelated coins, to break the on-chain link) launders coins: put one
black coin in with nine white ones, get ten white ones out. The paper inverts it, and the argument
is legal rather than technical.

Because getting good title requires acquiring in **good faith**, and because every transaction is
public, "the act of passing a bitcoin through a laundry should put all its subsequent owners on
notice that something may very well be wrong." Coin checking exists, exchanges claim to do it, so
it is a reasonable expectation. Therefore: "the likely outcome of feeding one black coin and nine
white coins into a bitcoin laundry isn't ten white coins, but ten black ones."

The conclusion is the sentence to remember: "people designing money laundering mechanisms have
been using quite the wrong metrics of quality."

### Step 8 — And then the paper undermines itself, honestly

> **In:** the whole tracing method of Steps 1–7.
> **Out:** the boundary of what any chain analysis can see — coins that never move on-chain. This
> is the honest weaker claim the method has to live with.

The last third is the part most summaries skip, and it is the most valuable. Having built the
tracing machinery and gone looking for theft victims, the authors found that "with one exception,
the victims we talked to were using **hosted wallets**" — a hosted wallet is one where the
exchange holds the keys, the customer sees a balance, and increasingly the exchange does not
actually move coins on-chain at all: "many bitcoin exchanges do not now give their customers
actual bitcoin, but rather do off-chain transactions with other exchange customers or transact on
customers' behalf with outsiders."

If the transaction never reaches the chain, no amount of chain analysis will see it. "In no case
could we find any clear documentation of the actual ownership of the missing cryptocurrency." The
real problem, they conclude, is not cryptography but "the emergence of a shadow banking system".

Take that as a methodological warning that generalises well beyond blockchains: your analysis is
only as good as the coverage of the log you are analysing. It is the same caveat topic 40's lane 1
makes about session collection, and the same one topic 34 makes about sampling.

## Where each step lives in the code

Repo: [`~/repos/RustyTaintChain`](https://github.com/TaintChain/RustyTaintChain) @ `4e12fd0`,
all in `src/callbacks/bootstrap_taint_fifo.rs`.

| step | anchor | what to read for |
|---|---|---|
| 5 | `:52` `TaintPart` | `name: u16` and `value: u64` — a run, not a fraction |
| 5 | `:142` `extract_taint` | the three branches; find the straddling-run split |
| 6 | `:174` `combine_taints` | merging two queues, and `number_of_collisions` |
| 6 | `:250` `reduce_taint` | run-length coalescing; note the single-run special case at the end |
| — | `:79` `TaintFifo` | the whole tracer's state: `utxo_set`, `bootstrap_addresses`, `dirtmapper` |
| — | `:100`/`:104` | `count_fragments` / `count_accounts` — the metrics exercise 3 asks you to reproduce |
| — | `README.md` | the authors' own FAQ, including "the order of transactions in a block is arbitrary, so why assign meaning to it?" — read their answer before you decide it is a flaw |

## Questions to answer in notes.md

1. FIFO's answer depends on the order of inputs and outputs within a transaction, which is
   arbitrary. Read the README's defence of this, then state the strongest counter-argument and
   decide which you believe. Does determinism-across-investigators outweigh
   arbitrariness-of-ordering?
2. Lane 2 measures poison at 394.67× the stolen value. Where does the extra money come from, in
   one sentence? Then say why poison is nevertheless the right policy for a *mixer* output, as
   the paper argues.
3. `reduce_taint` coalesces adjacent same-name runs. Construct a transaction sequence where the
   queue grows without bound *despite* coalescing, and say what a real implementation must do
   about it.
4. The paper argues a mixer turns nine white coins black rather than one black coin white.
   Restate that as a statement about *sets and notice* rather than about cryptography, and say
   what technical fact it depends on.
5. Section 5 finds that most victims' coins never moved on-chain at all. Write two sentences on
   what that does to every number in Section 3 — and name the equivalent blind spot in topic 40's
   lane 1.

## Done when

Answer each before unfolding it.

- [ ] You can state `nemo dat` and why bitcoin being a commodity rather than money matters.
  <details><summary>Answer</summary>

  `Nemo dat quod non habet` — you cannot pass better title than you hold, so a thief's buyer does
  not own the goods and the victim can reclaim them (Step 1). The exceptions are money and bills of
  exchange; **market overt** was abolished in Britain in 1995. Because the USA classes bitcoin a
  *commodity*, not money, no exception applies and stolen coins remain reclaimable — which is the
  entire reason a forward tracer has legal teeth. The paper *assumes* commodity status rather than
  asserting it as settled law.
  </details>
- [ ] You can give the Linode and Flexcoin haircut-vs-FIFO numbers from memory.
  <details><summary>Answer</summary>

  Linode (46,653 BTC, 2012): haircut taints 16,855,619 addresses ("just over 93%"), FIFO 245,120
  ("just over 1.35%"). Flexcoin (2014): haircut 10,421,112 addresses ("over 57%"), FIFO 15,265
  accounts (*Bitcoin Redux* §3.3). The repo's synthetic lane 1 echoes it: 98.0% of addresses
  tainted, 658 of them under 0.1% ([FINDINGS.md](../../FINDINGS.md) row 41).
  </details>
- [ ] You can explain "lossless" and why it makes backwards tracing possible.
  <details><summary>Answer</summary>

  Under FIFO a satoshi keeps a stolen-or-clean *label* (`TaintPart.name`, line 53), never a
  fraction, so no information is destroyed at a merge (Step 4). Provenance therefore survives any
  number of hops and you can trace a coin's heritage backwards, not just taint forwards. Haircut
  destroys it on the first merge: a product of fractions like 2/9 × 3/7 is a number, not a history,
  and cannot be inverted.
  </details>
- [ ] You can write `extract_taint`'s three branches without looking.
  <details><summary>Answer</summary>

  (1) Queue empty (`bootstrap_taint_fifo.rs:147`) — the rest of the requested value is clean, push
  a `name: 0` run. (2) Whole run fits (`:153`) — pop it, subtract its value, keep it. (3) Run
  straddles the cut (`:157–162`) — split it, keep the front piece with the same `name`, push the
  remainder back on the front of the queue. Branch 3 is the load-bearing one.
  </details>
- [ ] Your `taint.rs` reproduces lane 2: poison 394.67×, haircut 1.00× over 97.9% of UTXOs, FIFO
      1.00× over 0.9%.
  <details><summary>Answer</summary>

  Lane 2 (this topic's `notes.md`): poison inflates tainted value to 394.67× the theft (it invents
  taint at every merge); haircut conserves it (1.00×) but smears it across 97.9% of UTXOs (3657 of
  3734); FIFO conserves it (1.00×) and confines it to 0.9% (32 of 3734 UTXOs, the largest holding
  22.5%). Throughput: 20,400 tx in 6.6 ms ≈ 3.1M tx/s.
  </details>
- [ ] You wrote answers to all five questions in notes.md.
  <details><summary>Answer</summary>

  Done when notes.md holds your five written answers — the arbitrariness-of-ordering argument, the
  source of poison's extra money, an unbounded-queue-despite-coalescing sequence, the mixer claim
  restated in terms of sets and notice, and the on-chain-coverage blind spot shared with topic 40.
  </details>

## References

- Anderson, Shumailov, Ahmed, Rietmann. *Bitcoin Redux.* WEIS 2018 —
  [PDF](https://www.cl.cam.ac.uk/archive/rja14/Papers/bitcoin-redux.pdf).
- Code: [TaintChain/RustyTaintChain](https://github.com/TaintChain/RustyTaintChain) — "the
  simplest of implementations for FIFO (Clayton's case from 1816) money tracking".
- Möser, Böhme, Breuker (2013, 2014) — the poison and haircut policies, and the study of Bitcoin
  Fog / BitLaundry that found one laundry was "just a single fat wallet".
- *Devaynes v Noble* (1816), commonly *Clayton's Case* — the precedent itself.
- Local exercise stub: `topics/41-onchain-analytics/experiments/taint.rs`.
- Topic 1 (RUM conjecture) — poison/haircut/FIFO as a read-cost / space / usefulness triangle.
