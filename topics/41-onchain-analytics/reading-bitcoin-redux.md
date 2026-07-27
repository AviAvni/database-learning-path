# Bitcoin Redux: an 1816 court case fixes taint tracking

Most systems papers propose an algorithm. This one proposes a *precedent*. Anderson, Shumailov,
Ahmed and Rietmann set out to build a blacklist of stolen bitcoin, found that the industry's
standard taint-tracking rule smears a single theft across 93% of all addresses, and then
discovered that English lawyers had solved the same problem two hundred years earlier — when a
bank failed in 1816 and nobody could say which deposits had funded which withdrawals. The rule
the Master of the Rolls set down, first-in-first-out, turns out to be not just the legal answer
but the *computationally* right one, because it is lossless. This guide reads the paper alongside
RustyTaintChain, the authors' Rust implementation, whose core is fifteen lines.

## The problem in one sentence

**Trace a stolen coin forward through a transaction graph where money is constantly split and
merged, and end up with an answer narrow enough to act on rather than a trace of taint on
everybody.**

## The concepts, step by step

### Step 1 — Why the question is legally live: `nemo dat`

`Nemo dat quod non habet` — no one gives what they do not own — is a principle of nearly every
legal system. "If Alice steals Bob's horse and sells it to Charlie, Charlie doesn't end up owning
it; when Bob sees him riding it, he can simply demand it back."

The exception that used to matter, *market overt* (buy openly in a public market and you get good
title), was abolished in Britain in 1995. Two exceptions remain, for **money** and for **bills of
exchange** — and the USA has designated bitcoin a *commodity*, not money. So: "Unless
cryptocurrencies acquire this privileged status, there is no general exception to the nemo dat
rule — so a theft victim can pursue and retrieve his stolen property."

That is why taint tracking is not an academic exercise. If a stolen coin can be followed, it can
be reclaimed, and every exchange that touched it has a problem.

### Step 2 — Poison and haircut, and what the default does

Möser, Böhme and Breuker named the two policies the industry actually uses.

**Poison**: if any input to a transaction is tainted, *every* output is entirely tainted.
**Haircut**: each output is tainted by the fraction of input value that was tainted.

Haircut became the default. Here is what it does, traced forward over real thefts to 2016:

```
   2012 Linode theft, 46,653 BTC
     haircut ..... 16,855,619 addresses tainted — "just over 93% of the total"
     FIFO ............ 245,120 addresses tainted — "just over 1.35%"

   2014 Flexcoin hack ("the world's first bitcoin bank")
     haircut ..... 10,421,112 addresses — "over 57% of all addresses"
     FIFO ............. 15,265 accounts
```

The paper's summing up: "'haircut' tainting smears the taint over the actively traded bitcoin
stock. Bitcoin laundries are designed to make this even worse." And the consequence, stated
without hedging: "the effect of aggressive asset recovery via regulated exchanges might be more
akin to a tax on all users."

Lane 1 of this topic's crate reproduces the mechanism on a synthetic chain: one stolen coinbase
worth 0.25% of all the money ends up tainting **97.9% of the UTXO set and 98.0% of addresses**,
with 2,997 of those UTXOs carrying between 0.1% and 5% taint and exactly two carrying more than
5%. The total is conserved to the satoshi — haircut does not invent money — it is just no longer
information.

### Step 3 — Clayton's Case, 1816

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

The paper's Figures 1–3 draw exactly this for poison, haircut and FIFO with four colours of
tainted input. Note what FIFO does that the others cannot: "the taint does not go across in
percentages, but to individual components (indeed, individual Satoshis) of each output."

### Step 4 — Lossless, and therefore reversible

This is the property that matters and it is easy to skate past:

> As the taint does not spread or diffuse, the transaction processes it in a lossless way. This
> means that we can trace a bitcoin's heritage backwards as well as tracing taint forwards, and
> we can do tracing extremely efficiently once the appropriate index tables have been built.

A satoshi under FIFO is stolen or it is not; there is no fractional state to accumulate rounding
in, and no information is destroyed at a merge. So provenance survives arbitrarily many hops, and
you can ask "where did *this particular* satoshi come from" as well as "where did the theft go".
Haircut destroys that on the first merge: 2/9 of 3/7 of 5/11 is a number, not a history.

It also makes the tracing **deterministic**, which matters legally as much as technically: two
investigators running FIFO on the same chain get the same answer, and can be cross-examined on
it.

### Step 5 — `extract_taint`: Clayton's Case in fifteen lines

RustyTaintChain represents an output's provenance as a queue of runs:

```rust
// src/callbacks/bootstrap_taint_fifo.rs:52
pub struct TaintPart {
    name : u16,   // 0 = clean; other values name distinct crime sources
    value: u64,   // satoshis in this contiguous run
}
```

and the whole of the FIFO rule is one function that cuts `value` satoshis off the front of a
queue, splitting the run that straddles the boundary:

```rust
// :142, lightly trimmed
fn extract_taint(given_taints: &mut VecDeque<TaintPart>, value: u64) -> VecDeque<TaintPart> {
    let mut remaining = value;
    let mut new_tainted_balance = VecDeque::new();
    while remaining > 0 {
        if given_taints.is_empty() {
            new_tainted_balance.push_back(TaintPart { name: 0, value: remaining });
            remaining = 0;
        } else {
            let mut ctaint = given_taints.pop_front().unwrap();
            if remaining >= ctaint.value {
                remaining -= ctaint.value;
                new_tainted_balance.push_back(ctaint);          // whole run fits
            } else {
                ctaint.value -= remaining;                      // run straddles the cut
                new_tainted_balance.push_back(TaintPart { name: ctaint.name, value: remaining });
                given_taints.push_front(ctaint);                // put the remainder back
                remaining = 0;
            }
        }
    }
    new_tainted_balance
}
```

Three branches: the queue ran dry (the rest is clean), the run fits entirely, the run straddles
the cut and must be split. Getting the third one right is the whole exercise — the crate's
`extract_taint_splits_runs_at_the_boundary` test asks for 4 satoshis out of a 10-satoshi stolen
run and insists you get back a 4 and leave a 6.

Processing a transaction is then: concatenate the input queues in input order, call
`extract_taint` once per output in output order. Measured in lane 2: **3.1 million transactions
per second**, because that is all it is.

### Step 6 — The two operations `extract_taint` needs around it

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

The received wisdom is that a mixer launders coins: put one black coin in with nine white ones,
get ten white ones out. The paper inverts it, and the argument is legal rather than technical.

Because getting good title requires acquiring in **good faith**, and because every transaction is
public, "the act of passing a bitcoin through a laundry should put all its subsequent owners on
notice that something may very well be wrong." Coin checking exists, exchanges claim to do it, so
it is a reasonable expectation. Therefore: "the likely outcome of feeding one black coin and nine
white coins into a bitcoin laundry isn't ten white coins, but ten black ones."

The conclusion is the sentence to remember: "people designing money laundering mechanisms have
been using quite the wrong metrics of quality."

### Step 8 — And then the paper undermines itself, honestly

The last third is the part most summaries skip, and it is the most valuable. Having built the
tracing machinery and gone looking for theft victims, the authors found that "with one exception,
the victims we talked to were using **hosted wallets**" — the exchange holds the keys, the
customer sees a balance, and increasingly the exchange does not actually move coins on-chain at
all: "many bitcoin exchanges do not now give their customers actual bitcoin, but rather do
off-chain transactions with other exchange customers or transact on customers' behalf with
outsiders."

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

- [ ] You can state `nemo dat` and why bitcoin being a commodity rather than money matters.
- [ ] You can give the Linode and Flexcoin haircut-vs-FIFO numbers from memory.
- [ ] You can explain "lossless" and why it makes backwards tracing possible.
- [ ] You can write `extract_taint`'s three branches without looking.
- [ ] Your `taint.rs` reproduces lane 2: poison 394.67×, haircut 1.00× over 97.9% of UTXOs, FIFO
      1.00× over 0.9%.
- [ ] You wrote answers to all five questions in notes.md.

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
