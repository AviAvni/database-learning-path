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

## The problem in one sentence

If T1 commits and *then* (in wall-clock reality) T2 starts on another
machine, T2's snapshot must include T1 — but ordinary server clocks
disagree by tens to hundreds of milliseconds, so "then" is exactly what a
distributed system cannot see, and the central timestamp oracle that
fixes it (Percolator's TSO) is a SPOF plus a WAN round trip on every
transaction.

## The concepts, step by step

### Step 1 — external consistency, and why clocks can't give it for free

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

Spanner's TrueTime API never returns a timestamp — it returns an
*interval*. `TT.now()` yields `[earliest, latest]` guaranteed to contain
true time, where the half-width **ε** is the current worst-case clock
error. Google keeps ε at ~1–7 ms with GPS receivers and atomic clocks in
every datacenter, plus clock-drift accounting between synchronizations.
The honesty is the innovation: any machine can say "true time is
definitely not past X yet" — which converts clock uncertainty from a
silent correctness bug into a *waitable quantity*. The cost is hardware:
without it, ε is NTP's hundreds of milliseconds, and Step 3's trick
becomes unaffordable (that's the CockroachDB branch, Step 5).

### Step 3 — commit-wait: sleep until your timestamp is in the past

Spanner assigns `commit_ts = TT.now().latest` (an upper bound on true
time), then simply *waits* until `TT.now().earliest > commit_ts` before
acknowledging the commit — about 2ε, so ~4–14 ms. After the wait,
commit_ts is in the *past* on every machine on earth, so any transaction
that starts afterward — anywhere — reads a clock past it and gets a
higher timestamp. External consistency by sleeping:

```rust
fn commit(txn: &mut Txn, tt: &TrueTime) -> Timestamp {
    let s = tt.now().latest;               // commit_ts: an upper bound on true time
    txn.paxos_apply_at(s);                 // replicate the writes (locks still held)
    while tt.now().earliest <= s {         // COMMIT WAIT: sleep out the uncertainty
        sleep(s - tt.now().earliest);      // ~2ε on average
    }
    txn.release_locks_and_ack(s);          // now every clock on earth has passed s,
    s                                      // so any later txn anywhere gets ts > s
}
```

Note what it costs: pure *latency*, not throughput (commits pipeline
through the wait) — except under contention, where locks are held through
the sleep (Q1).

### Step 4 — the rest of Spanner: 2PC over Paxos, reads without locks

Two more ideas complete the picture. First, every shard is a **Paxos
group** (a handful of replicas keeping a consensus log, topic 15), and a
cross-shard transaction runs classic **two-phase commit (2PC** — all
shards durably prepare, then a coordinator decides**)** — but the
coordinator is *itself* a Paxos group, so the textbook blocking window
(coordinator dies holding everyone's locks, our `tpc.rs`) is closed by
replication rather than removed (contrast Percolator, which removed it).
Second, **lock-free snapshot reads**: because timestamps are externally
consistent, any replica whose Paxos log has caught up past `t` can serve
a consistent read at `t` with no locks at all — timestamps replace read
locks, and read traffic scales across replicas.

### Step 5 — HLC: causal timestamps within skew of the wall clock

No atomic clocks ⇒ ε is hundreds of ms ⇒ commit-wait is unaffordable.
CockroachDB's substitute is the **hybrid logical clock (HLC)**: a
timestamp `(l, c)` where `l` tracks the largest *physical* time seen
anywhere (your clock or any message's), and `c` is a logical counter
breaking ties when `l` stalls — a Lamport clock (increment on every
message to preserve causal order) welded to physical time:

```
send:  l' = max(l, pt)            recv:  l' = max(l, m.l, pt)
       c' = (l'==l) ? c+1 : 0            c' = matches which max won (see stub)
key bound: l never exceeds the largest pt seen anywhere
           => |l - true time| <= skew  (a Lamport clock has no such bound)
```

These are exactly the rules our `hlc.rs` stub implements. The bound is
the point: a pure Lamport clock drifts arbitrarily far from wall time
under message storms; HLC's `max(l, pt)` (never `l+1` past physical time)
pins `l` to the largest physical clock in the cluster, so an HLC
timestamp is *within max clock skew* of true time (Q2 asks for the
induction). Causality is guaranteed; real-time order is not — yet.

### Step 6 — the uncertainty interval: restart the read, not sleep the write

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
   timestamp too far ahead crashes the node rather than silently breaking
   the promise (Step 6).
3. `pkg/kv/kvclient/kvcoord/txn_coord_sender.go:113` — `TxnCoordSender`:
   the client-side coordinator, structured as a stack of interceptors.
4. `txn_interceptor_committer.go:128` (`txnCommitter`, background at
   `:55-83`) — **parallel commits** (Step 7): the STAGING record listing
   all in-flight writes, implicit commit, and the STAGING→COMMITTED
   promotion any observer can perform (`:195-205`).
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

## References

**Papers**
- Corbett et al. — "Spanner: Google's Globally-Distributed Database"
  (OSDI 2012) — §1-4 carry the TrueTime and commit-wait ideas; the
  schema/evaluation sections are skimmable
- Kulkarni et al. — "Logical Physical Clocks" (OPODIS 2014) — the HLC
  paper; the send/recv rules and the bounded-drift theorem

**Code**
- [cockroach](https://github.com/cockroachdb/cockroach)
  `pkg/util/hlc/hlc.go`, `pkg/kv/kvclient/kvcoord/` — the comment at
  `hlc.go:42-47` on maxOffset-as-a-promise is the key design note
