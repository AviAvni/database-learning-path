# Spanner & HLC: timestamps without the oracle

Snapshot timestamps that respect real-time order are easy with a central
oracle — and the oracle is a SPOF and a WAN round trip. This chapter reads
the two production escapes side by side: Spanner buys a tiny clock-error
bound ε with GPS and atomic clocks and then *sleeps it out* at commit,
while CockroachDB accepts NTP-grade skew and pays with hybrid logical
clocks plus uncertainty restarts at read time. It builds each idea step
by step — what external consistency demands, TrueTime, commit-wait, the
HLC rules, uncertainty intervals, and parallel commits — then walks
CockroachDB's `pkg/util/hlc`, the exact rules our `hlc.rs` stub
implements.

Spanner figures below are cited to Corbett et al., **"Spanner: Google's
Globally-Distributed Database"** (OSDI 2012) by section, figure or table; HLC
rules to Kulkarni et al., **"Logical Physical Clocks"** (OPODIS 2014). Code
anchors are CockroachDB at the SHA pinned in `resources/codebases.md`. Spanner
and HLC are *different* systems with different guarantees — the guide keeps
them apart, and so should you.

## The problem in one sentence

If T1 commits and *then* (in wall-clock reality) T2 starts on another
machine, T2's snapshot must include T1 — but ordinary server clocks
disagree by tens to hundreds of milliseconds, so "then" is exactly what a
distributed system cannot see, and the central timestamp oracle that
fixes it (Percolator's TSO) is a SPOF plus a WAN round trip on every
transaction.

## The concepts, step by step

### Step 1 — external consistency, and why clocks can't give it for free

> **In:** nothing yet — this step defines the guarantee (external
> consistency) the whole chapter is trying to buy without a TSO.
> **Out:** the two escape routes (bound the *error*, Steps 2–4; bound the
> *skew*, Steps 5–6) that the rest of the guide develops.

**Snapshot isolation** (topic 9) hands every transaction a timestamp;
readers see exactly the writes with smaller timestamps. That's only
honest if timestamps respect real-world order — a guarantee called
**external consistency** (if T2 begins after T1 commits in real time, T2
gets the larger timestamp; also called linearizability for transactions).
With one central clock (the TSO) it's trivial. With per-node clocks it
breaks: node B's clock running 200 ms behind stamps T2 *before* the T1 it
causally follows, and T2's snapshot silently misses committed data. The
two production escapes both start by *bounding* clock wrongness, then
differ in who pays:

```
                 external consistency without a TSO
                        /                    \
        Spanner: bound the clock ERROR      CRDB: bound the clock SKEW
        TrueTime ε (GPS+atomic, ~1-7ms)     max-offset (NTP, ~250-500ms)
        commit-wait: sleep out ε            uncertainty INTERVAL: restart
        => reads never doubt                reads that land inside it
```

### Step 2 — TrueTime: a clock that confesses its error

> **In:** the "bound the clock error" branch from Step 1.
> **Out:** an interval clock whose half-width ε is a *waitable* quantity —
> the input Step 3 sleeps out.

Spanner's TrueTime API never returns a timestamp — it returns an
*interval*. `TT.now()` yields `[earliest, latest]` guaranteed to contain
true time, where the half-width **ε** is the current worst-case clock
error. The paper is specific about the size (§3): ε "is typically a
sawtooth function of time, varying from about 1 to 7 ms over each poll
interval… therefore 4 ms most of the time." That sawtooth is manufactured
from a 30-second daemon poll interval and a 200 µs/s applied drift rate
(30 s × 200 µs/s = 6 ms of drift, plus ~1 ms of time-master communication
delay → the 1–7 ms band). Do **not** quote "7 ms" as typical: 7 ms is the
sawtooth *peak* just before a poll resets it; 4 ms is typical. And these are
the production-environment figures, not a hard maximum — §5.3, Figure 6
reports ε as a measured *distribution* (90th / 99th / 99.9th percentiles
across machines up to 2200 km apart), the honest way to characterise it.

The honesty is the innovation: any machine can say "true time is
definitely not past X yet" — which converts clock uncertainty from a
silent correctness bug into a *waitable quantity*. The cost is hardware
(GPS receivers and atomic clocks in every datacenter): without it, ε is
NTP's hundreds of milliseconds, and Step 3's trick becomes unaffordable
(that's the CockroachDB branch, Step 5).

### Step 3 — commit-wait: sleep until your timestamp is in the past

> **In:** TrueTime's confessed error ε from Step 2.
> **Out:** external consistency purchased with a bounded sleep — the cost
> Step 6 tries to avoid paying on every read.

Spanner assigns `commit_ts = TT.now().latest` (an upper bound on true
time), then simply *waits* until `TT.now().earliest > commit_ts` before
acknowledging the commit. Because the leader chose `s` from
`TT.now().latest` and waits until that is guaranteed past, "the expected
wait is at least 2∗ε" (§4.2.1). Work it on the ε band from Step 2:

```
ε (ms)   commit-wait = 2ε (ms)     where ε comes from
  1              2                 sawtooth trough (§3)
  4              8                 "4 ms most of the time" (§3)
  7             14                 sawtooth peak (§3)
```

So the *design* cost is ~2–14 ms, ~8 ms typically. But the measured
1-replica commit wait is only **~5 ms** (§5.1, Table 3, vs ~9 ms Paxos
latency) — *lower* than 2×4 ms because "this wait is typically overlapped
with Paxos communication" (§4.2.1): the leader is replicating the commit
record during the same window it is sleeping. After the wait, commit_ts is
in the *past* on every machine on earth, so any transaction that starts
afterward — anywhere — reads a clock past it and gets a higher timestamp.
External consistency by sleeping:

```
ILLUSTRATION — Spanner §4.2.1 commit-wait; paper-only pseudocode
(Spanner has no public source to anchor).

fn commit(txn, tt):
    s = tt.now().latest            # commit_ts: an upper bound on true time
    txn.paxos_apply_at(s)          # replicate writes (locks held; overlaps the wait)
    while tt.now().earliest <= s:  # COMMIT WAIT: sleep out the uncertainty
        sleep(s - tt.now().earliest)   # expected >= 2*epsilon
    txn.release_locks_and_ack(s)   # every clock on earth has now passed s,
    return s                       # so any later txn anywhere gets ts > s
```

Note what it costs: pure *latency*, not throughput (commits pipeline
through the wait, and the wait overlaps Paxos) — except under contention,
where locks are held through the sleep (Q1).

### Step 4 — the rest of Spanner: 2PC over Paxos, reads without locks

> **In:** externally-consistent timestamps from Step 3.
> **Out:** how a whole transaction commits (2PC whose coordinator is itself
> replicated) and how reads go lock-free — the contrast Step 5 leaves behind
> when it drops TrueTime.

Two more ideas complete the picture. First, every shard is a **Paxos
group** (a handful of replicas keeping a consensus log, topic 15), and a
cross-shard transaction runs classic **two-phase commit (2PC)** — all
shards durably prepare, then a coordinator decides — but the
coordinator is *itself* a Paxos group, so the textbook blocking window
(coordinator dies holding everyone's locks, our `tpc.rs`) is closed by
replication rather than removed (contrast Percolator, which removed it).
Second, **lock-free snapshot reads**: because timestamps are externally
consistent, any replica whose Paxos log has caught up past `t` can serve
a consistent read at `t` with no locks at all — timestamps replace read
locks, and read traffic scales across replicas.

### Step 5 — HLC: causal timestamps within skew of the wall clock

> **In:** the "NTP-grade skew, no atomic clocks" branch from Step 1.
> **Out:** an HLC timestamp `(l, c)` that is causal *and* stays within ε of
> the wall clock — but not yet externally consistent, which Step 6 fixes.

No atomic clocks ⇒ ε is hundreds of ms ⇒ commit-wait is unaffordable.
CockroachDB's substitute is the **hybrid logical clock (HLC)**: a
timestamp `(l, c)` where `l` tracks the largest *physical* time seen
anywhere (your clock or any message's), and `c` is a logical counter
breaking ties when `l` stalls — a Lamport clock (increment on every
message to preserve causal order) welded to physical time. These are the
exact update rules of the paper's Figure 5 (write `l'` for the previous
`l`, `pt` for the local physical clock, `l.m`/`c.m` for a message's
timestamp):

```
HLC — Kulkarni et al. 2014, Figure 5 (rules for node j)

Send or local event:
    l' = l
    l = max(l', pt)
    if l == l':  c = c + 1
    else:        c = 0

Receive event of message m:
    l' = l
    l = max(l', l.m, pt)
    if   l == l' == l.m:  c = max(c, c.m) + 1
    elif l == l':         c = c + 1
    elif l == l.m:        c = c.m + 1
    else:                 c = 0
```

These are exactly the rules our `hlc.rs` stub implements. The bound is
the point (Theorem 3: `l` is the maximum physical time heard; Corollary 1:
`|l − pt| ≤ ε`): a pure Lamport clock drifts arbitrarily far from wall time
under message storms; HLC's `max(l', pt)` (never `l'+1` past physical time)
pins `l` to the largest physical clock in the cluster, so an HLC timestamp
is *within max clock skew* of true time (Q2 asks for the induction).
Causality is guaranteed (Theorem 1); real-time order is not — yet.

### Step 6 — the uncertainty interval: restart the read, not sleep the write

> **In:** HLC's causal-but-not-external timestamps from Step 5.
> **Out:** external consistency recovered at *read* time by restarting, not
> at write time by sleeping — the CRDB dual of Step 3's commit-wait.

HLC alone gives causal order, not external consistency: a write by a
fast-clocked node can carry a timestamp *above* a later reader's — the
reader would wrongly skip it. CRDB patches this at read time. Every
deployment promises a **max-offset** (maximum clock skew between any two
nodes, default 500 ms — a promise, not a measurement). A read at `ts`
treats `[ts, ts + max_offset]` as its **uncertainty interval**: a value
timestamped *inside* it might have committed before the read began in
real time (the writer's clock may be ahead by up to max-offset), so the
read **restarts** at just above that value's timestamp; a value *above*
the interval provably committed after the read began and is safely
ignored (Q3). Spanner's ~2ε sleep on every read-write commit became a
restart penalty paid only when a read actually collides with a recent
write in the window.

### Step 7 — parallel commits: shaving the second consensus round

> **In:** the settled-timestamps machinery (Steps 5–6).
> **Out:** the two consensus rounds of a distributed commit collapsed to one
> — Percolator's resolve-from-data idea (the other chapter) reused for latency.

With timestamps settled, CRDB attacks commit latency. Naively a
distributed commit is two sequential consensus rounds: replicate the
intents (staged writes), then replicate the "committed" decision.
**Parallel commits** merges them: the coordinator writes a transaction
record in `STAGING` state listing every in-flight write, and issues all
of them in parallel. The transaction is **implicitly committed** the
instant all staged writes succeed — a fact any observer can verify by
checking the STAGING record's list, then promote to an explicit
COMMITTED record. That is Percolator's any-reader-can-resolve idea,
repurposed to save a latency round instead of to survive coordinator
death (Q4 asks what replaces the "primary lock still held" test).
**Pipelining** is the same instinct one level down: don't wait for one
write's consensus before issuing the next; prove all in-flight writes at
commit time.

## Where each step lives in the code

CockroachDB, in reading order:

1. `pkg/util/hlc/hlc.go:38` — `type Clock`: wall + logical, exactly our
   `Hlc { l, c }` (Step 5). Read the comment at `:42-47` on how
   `maxOffset` is a *promise* the deployment makes, not a measurement
   (Step 6).
2. `hlc.go:411` — `Now()`: the send rule. `hlc.go:471` — `Update()`: the
   receive rule (every RPC response carries a timestamp; clocks gossip
   ambiently) — Step 5. `:517` — `UpdateAndCheckMaxOffset`: a remote
   timestamp more than `maxOffset` ahead is **rejected with an error**
   (`errUntrustworthyRemoteWallTimeErr`, "remote wall time is too far ahead
   … to be trustworthy", `:520-526`) and the message is dropped — the node
   does *not* crash here. Self-termination is a *separate* mechanism keyed on
   a different field, `toleratedOffset` (`:49-51`: "the tolerated clock skew
   … beyond which the node will self-terminate"), enforced via the
   forward-clock-jump `Fatalf` path in `checkPhysicalClock` (`:396-404`).
   Don't conflate the two (Step 6).
3. `pkg/kv/kvclient/kvcoord/txn_coord_sender.go:113` — `TxnCoordSender`:
   the client-side coordinator, structured as a stack of interceptors.
4. `txn_interceptor_committer.go:128` (`txnCommitter`; design comment at
   `:55-83`) — **parallel commits** (Step 7): the STAGING record listing
   all in-flight writes, implicit commit, and the STAGING→COMMITTED
   promotion any observer can perform (the `case roachpb.STAGING` arm at
   `:205`, validation at `:195-215`).
5. `txn_interceptor_pipeliner.go:311` (`SendLocked`) — pipelining
   (Step 7): don't wait for a write's consensus before issuing the next;
   track "in-flight" writes and prove them at commit. Parallel commits
   (`:89-168` comments) is the natural endpoint.

For Spanner itself there is no code to read — the paper is the artifact;
see the reading route in the References (§1-4 carry TrueTime and
commit-wait; schema/evaluation sections are skimmable).

## Questions to answer while reading

1. Commit-wait sleeps ~2ε per read-write txn. Why does that *not* cap
   throughput (only latency)? What does it do to contended workloads,
   given locks are held through the wait?
2. Derive why HLC's `l <= max pt seen` bound holds by induction over the
   send/recv rules — then find which rule breaks it if you replace
   `max(l, pt)` with `l+1` (Lamport).
3. A CRDB read at ts=100 with max_offset=500 finds a value at ts=300.
   Walk through why ignoring it can violate real-time order, and why a
   value at ts=700 is safe to ignore.
4. Parallel commits: a coordinator dies leaving a STAGING record. How does
   a reader decide commit vs abort, and what plays the role of
   Percolator's "primary lock still held" test?
5. Our `hlc.rs` test asserts two silent nodes at the same `pt` produce
   *equal* timestamps. Where does CRDB inject the tiebreak, and why is it
   fine for MVCC that two *different keys'* writes tie?
6. M29 mapping: FalkorDB won't have TrueTime. Between (a) a TSO à la
   TiKV's PD and (b) HLC + uncertainty restarts, which fits a
   single-region graph store, and what changes if we go multi-region?

## Done when

Answer each before unfolding it.

- [ ] You can define external consistency and say why clocks do not give it for free.

  <details><summary>Answer</summary>

  External consistency (linearizability for transactions): if T2 begins after
  T1 commits in real time, T2 gets the larger timestamp and its snapshot
  includes T1 (Step 1). Per-node clocks can't guarantee it for free because
  they disagree by tens–hundreds of ms, so a lagging node stamps a
  causally-later transaction with a *smaller* timestamp and silently drops
  committed data. A central TSO fixes it but is a SPOF plus a WAN round trip —
  the thing both Spanner and CRDB are escaping.

  </details>

- [ ] You can explain TrueTime as a clock that confesses its error, and what commit-wait does with that.

  <details><summary>Answer</summary>

  TrueTime returns an interval `[earliest, latest]` guaranteed to bracket true
  time; the half-width ε is the confessed worst-case error (~1–7 ms sawtooth,
  ~4 ms typical, §3; a measured distribution in §5.3 Fig 6). Commit-wait sets
  `commit_ts = TT.now().latest` and sleeps until `TT.now().earliest >
  commit_ts` — expected ≥ 2ε (§4.2.1) — so the timestamp is provably in the
  past everywhere before the commit is acknowledged, which is exactly external
  consistency.

  </details>

- [ ] You can explain why commit-wait's ~2ε sleep does not cap throughput.

  <details><summary>Answer</summary>

  Because it is latency, not a serialization bottleneck: independent commits
  pipeline through their waits concurrently, and the wait "is typically
  overlapped with Paxos communication" (§4.2.1) — which is why the *measured*
  1-replica commit wait is ~5 ms (§5.1, Table 3), below 2×4 ms. Throughput
  only suffers when the same *keys* are contended, because then locks are held
  across the sleep and later writers on those keys queue (Q1).

  </details>

- [ ] You can derive HLC's `l <= max pt seen` bound by induction.

  <details><summary>Answer</summary>

  By Figure 5, every update sets `l = max(l', l.m, pt)` — never `l'+1` — so `l`
  is always the max of quantities that are themselves ≤ the largest physical
  time any node has read (Theorem 3: `l.f` is the maximum clock value heard).
  Base case: initially `l = 0`. Step: each send/receive takes a max over the
  prior `l` (≤ max pt by hypothesis), an incoming `l.m` (≤ max pt by the same
  hypothesis on the sender), and local `pt`. Hence `|l − pt| ≤ ε`
  (Corollary 1). Replacing `max(l', pt)` with `l'+1` (Lamport) drops the `pt`
  ceiling, so `l` can run away under a message storm — the bound breaks.

  </details>

- [ ] You can explain the uncertainty-interval alternative: restart the read rather than sleep the write.

  <details><summary>Answer</summary>

  CRDB accepts NTP skew and promises a **max-offset** (default 500 ms,
  `base/constants.go:15`). A read at `ts` treats `[ts, ts+max_offset]` as
  uncertain: a value timestamped *inside* it might really have committed
  before the read began (writer's clock could be ahead), so the read
  **restarts** just above that value; a value *above* the interval provably
  committed later and is safely ignored (Step 6, Q3). So the ε cost moves from
  a sleep on *every* commit (Spanner) to a restart paid only when a read
  actually collides with a recent write in the window.

  </details>

- [ ] You wrote answers to all questions in notes.md, and can connect the HLC bound to the invariant the `hlc.rs` test asserts.

  <details><summary>Answer</summary>

  Self-check: the `hlc.rs` test asserting two silent nodes at equal `pt`
  produce *equal* timestamps is the `l == l'` / `c` tiebreak arm of Figure 5
  in action; Q5 is where CRDB injects the logical tiebreak and why equal
  timestamps on two *different* keys are harmless for MVCC. Q4 should map the
  STAGING-record resolution onto Percolator's "primary lock still held" test;
  Q6 should pick TSO vs HLC for single- vs multi-region FalkorDB.

  </details>

## References

**Papers**
- Corbett et al. — "Spanner: Google's Globally-Distributed Database"
  (OSDI 2012) — §3 defines TrueTime and the ε sawtooth; §4.1.2/§4.2.1 the
  commit-wait rule and its expected-≥2ε cost; §5.1 Table 3 the measured
  ~5 ms commit wait; §5.3 Figure 6 the ε distribution
- Kulkarni et al. — "Logical Physical Clocks" (OPODIS 2014) — the HLC
  paper; Figure 5 is the send/receive rules, Theorem 3 and Corollary 1 the
  bounded-drift result

**Code**
- [cockroach](https://github.com/cockroachdb/cockroach)
  `pkg/util/hlc/hlc.go`, `pkg/kv/kvclient/kvcoord/` — the comment at
  `hlc.go:42-47` on maxOffset-as-a-promise is the key design note
