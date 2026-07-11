# Reading guide — Spanner (OSDI '12), TrueTime, and CockroachDB's HLC answer

Papers: *Spanner: Google's Globally-Distributed Database* (OSDI '12);
*Logical Physical Clocks* (Kulkarni et al., OPODIS '14 — the HLC paper).
Code: [`~/repos/cockroach`](https://github.com/cockroachdb/cockroach) (`pkg/util/hlc/`, `pkg/kv/kvclient/kvcoord/`).

## The problem both solve

Snapshot isolation needs timestamps that respect real-world order: if txn
T1 commits and *then* (in wall-clock reality) T2 starts on another
machine, T2's snapshot must see T1. A central TSO (Percolator) gives this
trivially but is a SPOF and a WAN round trip. Spanner and CockroachDB are
two answers to "timestamps without the oracle."

```
                 external consistency without a TSO
                        /                    \
        Spanner: bound the clock ERROR      CRDB: bound the clock SKEW
        TrueTime ε (GPS+atomic, ~1-7ms)     max-offset (NTP, ~250-500ms)
        commit-wait: sleep out ε            uncertainty INTERVAL: restart
        => reads never doubt                reads that land inside it
```

## Spanner in four ideas

1. **TrueTime**: `TT.now()` returns an *interval* `[earliest, latest]`
   guaranteed to contain true time. Hardware (GPS + atomic clocks per DC)
   keeps ε small.
2. **Commit wait**: assign `commit_ts = TT.now().latest`, then *wait*
   until `TT.now().earliest > commit_ts` before acknowledging. After the
   wait, every machine's clock has passed commit_ts — so any later txn
   anywhere gets a higher timestamp. External consistency by sleeping ~2ε.
3. **2PC over Paxos groups**: each shard is a Paxos group; the 2PC
   coordinator is *itself* a Paxos group, so the blocking window of our
   `tpc.rs` (coordinator dies holding everyone's locks) is closed by
   replication rather than removed.
4. **Lock-free snapshot reads**: any replica can serve a read at `t` once
   its Paxos log is caught up past `t` — timestamps replace read locks.

## HLC: the software substitute

No atomic clocks ⇒ ε is hundreds of ms ⇒ commit-wait is unaffordable. HLC
instead makes timestamps *causally* consistent (Lamport) while staying
within max clock skew of physical time — the rules our `hlc.rs` stubs
implement:

```
send:  l' = max(l, pt)            recv:  l' = max(l, m.l, pt)
       c' = (l'==l) ? c+1 : 0            c' = matches which max won (see stub)
key bound: l never exceeds the largest pt seen anywhere
           => |l - true time| <= skew  (a Lamport clock has no such bound)
```

The price: HLC alone gives causal order, not external consistency. CRDB
patches the gap at read time with the **uncertainty interval**
`[read_ts, read_ts + max_offset]`: a value with a timestamp inside it
*might* have committed first in real time, so the read restarts above it.

## CockroachDB code walk

1. `pkg/util/hlc/hlc.go:38` — `type Clock`: wall + logical, exactly our
   `Hlc { l, c }`. Read the comment at `:42-47` on how `maxOffset` is a
   *promise* the deployment makes, not a measurement.
2. `hlc.go:411` — `Now()`: the send rule. `hlc.go:471` — `Update()`: the
   receive rule (every RPC response carries a timestamp; clocks gossip
   ambiently). `:517` — `UpdateAndCheckMaxOffset`: a remote timestamp too
   far ahead crashes the node rather than silently breaking the promise.
3. `pkg/kv/kvclient/kvcoord/txn_coord_sender.go:113` — `TxnCoordSender`:
   the client-side coordinator, structured as a stack of interceptors.
4. `txn_interceptor_committer.go:128` (`txnCommitter`, background at
   `:55-83`) — **parallel commits**: instead of prewrite-everything *then*
   commit (two sequential consensus rounds), CRDB writes a txn record in
   `STAGING` state listing all in-flight writes and issues them in
   parallel. The txn is *implicitly committed* the instant all writes
   succeed; any observer can verify this and promote STAGING→COMMITTED
   (`:195-205`). This is Percolator's any-reader-can-resolve idea applied
   to shave a latency round.
5. `txn_interceptor_pipeliner.go:311` (`SendLocked`) — pipelining: don't
   wait for a write's consensus before issuing the next; track "in-flight"
   writes and prove them at commit. Parallel commits (`:89-168` comments)
   is the natural endpoint.

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
