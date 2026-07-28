# 93% of Bitcoin addresses are "tainted". That's a bug in the rule, not a fact about Bitcoin.

*Draft — rewrite in your own voice before publishing. Every number below is
attributed; the ones marked "measured" come from code you can run.*

---

In 2012 someone stole 46,653 BTC from Linode's hosted wallets. Four years later,
researchers at Cambridge traced that theft forward through the blockchain using the
tainting rule the cryptocurrency-forensics industry had settled on.

It marked **16,855,619 addresses as tainted — just over 93% of every address in
existence.**

They then traced the same theft with a different rule and got **245,120 addresses,
or 1.35%**.

Same blockchain, same theft, same question. A 69× difference in the answer, entirely
down to a modelling choice that almost nobody examines. And the rule that gives the
useful answer comes from an English court case decided in 1816.

## The problem: money doesn't have identity

Say I steal one coin from you and mix it with nine of my own in a single transaction
that pays out ten coins. Which of those outputs is your coin?

There is no fact of the matter. A Bitcoin transaction consumes some outputs and
creates new ones; it does not carry per-satoshi provenance. So "is this coin stolen?"
is not a question the ledger answers — it's a question your *policy* answers, and
you have to pick one.

Möser, Böhme and Breuker named the two policies the industry actually uses, and
there's a third from the law.

```
   inputs                POISON              HAIRCUT             FIFO
   ┌──────┐ clean  3     everything          each output         lay the satoshis
   ├──────┤ STOLEN 2     downstream is       gets 2/9            end to end and cut
   ├──────┤ clean  4     fully tainted       stolen              the outputs off the
   └──────┘                                                      front, in order
                         D: all 9 stolen     D: 0.67 stolen      D: 3 clean
                         E: all 9 stolen     E: 0.44 stolen      E: 2 STOLEN
                         F: all 9 stolen     F: 0.89 stolen      F: 4 clean
```

**Poison** says any tainted input contaminates every output completely. It's simple,
and it counts far more money as stolen than was ever stolen — the total grows without
bound as the chain fans out.

**Haircut** says each output inherits the tainted *fraction* of the inputs. It
conserves the total exactly, which feels principled, and it became the default.

**FIFO** says the first satoshi in is the first satoshi out.

## What haircut actually does

Haircut is the one that produced the 93%. It's worth being precise about why, because
it isn't obviously wrong.

Haircut doesn't invent money. Trace a theft with it and the total tainted value at the
end equals exactly what was stolen — the arithmetic is conservative and correct. The
problem is *where* that value ends up. Every transaction with a tainted input gives
**every** output a nonzero share. So the tainted set grows at every hop, and after a
few hops it's most of the economy holding homeopathic quantities of your theft.

I built a synthetic UTXO chain to watch this happen: 400 entities, 20,400
transactions, 30,342 addresses, and one stolen coinbase worth 0.25% of all the money
on the chain. Running haircut to the end (measured):

```
   tainted UTXOs        3657 of 3734   (97.9%)
   tainted addresses    3553 of 3627   (98.0%)
   tainted value      1000000 of 400000000   (the theft was 1000000)

   of those 3657 UTXOs:  658 are under 0.1% tainted
                        2997 are 0.1%-5%
                           2 are above 5%
```

Ninety-eight percent of everybody is holding a trace, and **two** UTXOs in the entire
chain hold a share worth arguing about. The Cambridge team's summary of the real-chain
version is blunter than anything I'd write: haircut tainting "smears the taint over
the actively traded bitcoin stock", and the effect of enforcing it through regulated
exchanges "might be more akin to a tax on all users."

A rule that taints 93% of everyone is not a forensic tool. It's a tax.

## Clayton's Case, 1816

In 1816 a bank called Devaynes, Dawes, Noble & Co. failed, and the court had to
work out what it owed a customer whose account had seen a long series of deposits and
withdrawals before the collapse. Which deposits had funded which withdrawals?

The Master of the Rolls — one of the most senior judges in England — set down a rule
of stunning simplicity: **withdrawals are deemed to be drawn against the deposits
first made.** First in, first out.

Applied to a Bitcoin transaction: lay the input satoshis end to end in input order,
then cut the outputs off the front of that queue in output order. In the diagram
above, the two stolen satoshis land entirely in output E. Outputs D and F are clean.
Not 22% clean. Clean.

## Why FIFO wins, and it isn't the fairness

The interesting property isn't that FIFO is more "accurate" — it's a convention, not
a discovery. It's that FIFO is **lossless**.

A satoshi under FIFO is stolen or it is not. There's no fractional state, so nothing
accumulates rounding, and no information is destroyed when funds merge. The Cambridge
paper puts it well: "the taint does not spread or diffuse, the transaction processes
it in a lossless way. This means that we can trace a bitcoin's heritage backwards as
well as tracing taint forwards."

Three things follow, and they're all things haircut cannot give you:

- **Provenance survives arbitrarily many hops.** Under haircut, after two merges your
  number is a fraction of a fraction of a fraction. Under FIFO it's still a satoshi
  with a name on it.
- **You can run it in reverse.** "Where did *this particular* satoshi come from?" is
  answerable. Under haircut it isn't, at any price.
- **It's deterministic.** Two investigators running FIFO on the same chain get the
  same answer and can be cross-examined on it. That matters more in a courtroom than
  in a paper.

## The whole algorithm is fifteen lines

Here's the core, adapted from the authors' own Rust implementation. Represent an
output's provenance as a queue of runs — contiguous stretches of same-origin value —
and the entire rule is one function that cuts `value` satoshis off the front,
splitting the run that straddles the boundary:

```rust
struct TaintPart { name: u16, value: u64 }   // name 0 = clean

fn extract_taint(queue: &mut VecDeque<TaintPart>, value: u64) -> VecDeque<TaintPart> {
    let mut remaining = value;
    let mut taken = VecDeque::new();
    while remaining > 0 {
        match queue.pop_front() {
            None => {                                   // queue dry: the rest is clean
                taken.push_back(TaintPart { name: 0, value: remaining });
                remaining = 0;
            }
            Some(run) if remaining >= run.value => {    // whole run fits
                remaining -= run.value;
                taken.push_back(run);
            }
            Some(mut run) => {                          // run straddles the cut: split it
                run.value -= remaining;
                taken.push_back(TaintPart { name: run.name, value: remaining });
                queue.push_front(run);
                remaining = 0;
            }
        }
    }
    taken
}
```

Processing a transaction is then: concatenate the input queues in input order, and
call this once per output, in output order. That's it. Note that `name` is a `u16` and
not a `bool` — real chains have more than one crime in them, and the queue tracks
which.

Measured on my synthetic chain: **20,400 transactions in 6.6 ms**, or 3.1 million
transactions per second. The expensive-sounding thing is a queue splice.

## The three policies, side by side

Same chain, same theft (measured):

| policy | tainted UTXOs | tainted addresses | value flagged | vs actually stolen |
|---|---|---|---|---|
| poison | 3690 (98.8%) | 3585 | 394,674,821 | **394.67×** |
| haircut | 3657 (97.9%) | 3553 | 1,000,000 | 1.00× |
| **FIFO** | **32 (0.9%)** | **32** | 1,000,000 | 1.00× |

Poison declares 394 times more money stolen than was ever taken, because it re-counts
each descendant output's full value. Haircut conserves the total and destroys it as
information. FIFO conserves the total *and* keeps it in one place — 32 UTXOs, one of
which holds 22.5% of everything flagged.

The real-chain numbers have the same shape. Linode 2012: 93% under haircut, 1.35%
under FIFO. The 2014 Flexcoin hack: 10,421,112 addresses under haircut (over 57% of
all of them), **15,265** under FIFO.

## Two consequences nobody expects

**Tracing matters legally, not just academically.** *Nemo dat quod non habet* — no one
gives what they do not own — is a principle of nearly every legal system. If Alice
steals Bob's horse and sells it to Charlie, Charlie doesn't own the horse. The
exception that used to matter in Britain, *market overt*, was abolished in 1995;
exceptions remain for **money** and bills of exchange, and the USA has designated
Bitcoin a *commodity*. So a theft victim can pursue stolen coins through however many
hands they've passed. Which means a traceable coin is a coin with a claim attached.

**And mixers make things worse, not better.** The received wisdom is that a laundry
launders: put one black coin in with nine white, get ten white out. The Cambridge
argument inverts it, and it's a legal argument rather than a technical one. Getting
good title requires acquiring in good faith. Every transaction is public. Coin
checking exists and exchanges claim to do it. So passing a coin through a mixer puts
every later holder *on notice* that something may be wrong — and therefore "the likely
outcome of feeding one black coin and nine white coins into a bitcoin laundry isn't
ten white coins, but ten black ones."

Their conclusion, which I've thought about more than anything else in the paper:
"people designing money laundering mechanisms have been using quite the wrong metrics
of quality."

## The part that undercuts all of it

I'd be doing the paper a disservice if I stopped there, because its last section
quietly demolishes its own premise, and it's the most valuable page in it.

Having built the tracing machinery, the authors went looking for theft victims to help
— and found that "with one exception, the victims we talked to were using **hosted
wallets**." The exchange holds the keys, the customer sees a balance, and increasingly
the exchange doesn't move coins on-chain at all: it settles internally against other
customers. If the transaction never reaches the chain, no amount of chain analysis
will ever see it. "In no case could we find any clear documentation of the actual
ownership of the missing cryptocurrency."

The real problem, they conclude, is not cryptography but "the emergence of a shadow
banking system."

Take that as a methodological warning that generalises well past blockchains: **your
analysis is only ever as good as the coverage of the log you are analysing.** You can
pick the right tainting rule, implement it perfectly, run it at three million
transactions a second — and still be answering a question about the 40% of activity
that happened to be visible.

---

*The synthetic chain, the three tainting policies and the measurements above are
topic 41 of [a database-internals curriculum I'm writing](https://github.com/AviAvni/database-learning-path)
— 44 topics where every claim ships with the benchmark that produced it.
`./verify.sh 41` reproduces the tables in this post.*

**Sources**

- Anderson, Shumailov, Ahmed & Rietmann, *Bitcoin Redux*, WEIS 2018 — the Linode and
  Flexcoin figures, Clayton's Case, `nemo dat`, and the hosted-wallet finding.
  [PDF](https://www.cl.cam.ac.uk/archive/rja14/Papers/bitcoin-redux.pdf)
- Möser, Böhme & Breuker (2013, 2014) — the poison and haircut policies.
- *Devaynes v Noble* (1816), commonly Clayton's Case.
- [TaintChain/RustyTaintChain](https://github.com/TaintChain/RustyTaintChain) — the
  authors' FIFO implementation, which `extract_taint` above is adapted from.
