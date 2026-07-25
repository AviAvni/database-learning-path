# Redis doctors: a database that confesses at a price it can afford

Redis ships with its own diagnosticians: SLOWLOG remembers every
command that took too long, the latency monitor keeps a fixed-size
history of bad events, LATENCY DOCTOR and MEMORY DOCTOR turn that
history into prose advice, and the software watchdog interrupts a
stuck server to log where it is standing. For a FalkorDB developer
this is not analogy — this code IS the host process your module runs
inside, and `slowlogPushEntryIfNeeded` fires after every
`GRAPH.QUERY`. The repo is cloned at `~/repos/redis`; this is a
code-read, ~1.5h, across four small files. The anchor table below
maps each step to an exact file:line.

## The problem in one sentence

A production database must diagnose itself — record slow commands,
spot latency spikes, explain memory bloat, unstick a hung event loop
— while adding effectively zero overhead to the fast path, because
any instrumentation that costs more than the anomaly it detects will
be turned off.

## The concepts, step by step

### Step 1 — the theorem: tier the diagnosis surface by cost

Redis solves the observer-cost problem by splitting diagnosis into
three tiers, each with a hard budget it can always afford:

```
 tier        surface                cost per command      when it runs
 ─────────── ────────────────────── ───────────────────── ─────────────
 always-on   SLOWLOG threshold      ~1 integer compare    every command
 armed       latency monitor        few writes into a     when threshold
             (160-sample rings)     fixed ring, on spike  is configured
 on-demand   LATENCY/MEMORY DOCTOR, walk data, build      only when a
             software watchdog      text / take SIGALRM   human asks
```

The invariant: expensive analysis never rides the hot path. The hot
path only *deposits* evidence — cheaply, into bounded structures —
and analysis is deferred to the moment someone asks. Every step below
is one tier of this theorem. Why it matters: M34 ports this surface
to the FalkorDB Rust engine with a measured overhead budget; the
budget is only meetable because of this tiering.

### Step 2 — tier 1: SLOWLOG's one-compare fast path

`slowlogPushEntryIfNeeded` (`src/slowlog.c:103`) is called after
EVERY command completes, with the measured duration in microseconds:

```c
void slowlogPushEntryIfNeeded(client *c, robj **argv, int argc, long long duration) {
    if (server.slowlog_log_slower_than < 0 || server.slowlog_max_len == 0) return;
    if (duration >= server.slowlog_log_slower_than)
        listAddNodeHead(server.slowlog, slowlogCreateEntry(c,argv,argc,duration));
    /* trim list down to slowlog-max-len: oldest evicted */
```

The whole always-on tier is those two comparisons (`:104`–`:105`).
The exact semantics are load-bearing and easy to get subtly wrong:
`>=` not `>` (a command equal to the threshold logs); a *negative*
threshold disables entirely (0 logs everything); the list is trimmed
to `slowlog-max-len` with the OLDEST evicted; and entry ids
(`server.slowlog_entry_id`, stamped in `slowlogCreateEntry`)
increase monotonically and are NOT reset by SLOWLOG RESET, so a
poller never confuses a new entry with one it already saw. Topic
34's `SlowLog` Rust stub is exactly this function reshaped: its
contract tests encode >=-logs, negative-disables,
fixed-ring-oldest-evicted, ids-monotonic-across-reset — pass them
and you have reimplemented `:103`–`:111`.

### Step 3 — the evidence must not become the problem

`slowlogCreateEntry` (`src/slowlog.c:28`) shows the second-order
discipline: the diagnostic record itself is bounded. Arguments are
capped at `SLOWLOG_ENTRY_MAX_ARGC` (the last slot becomes
`"... (N more arguments)"`), and any string longer than
`SLOWLOG_ENTRY_MAX_STRING` is truncated with a
`"... (N more bytes)"` suffix:

```
 SET giant-key <10 MB value>        slowlog entry:
 ──────────────────────────►   [SET][giant-key][first 128 B "... (10485632 more bytes)"]
```

Non-shared argument objects are duplicated, not refcounted — the
comment at `:58` explains why: sharing an robj between the slowlog
and the keyspace means FLUSHALL ASYNC could free it under the log's
feet. Why it matters: a slow command is often slow *because* its
arguments are huge; the log observes values, it must never own them.

### Step 4 — tier 2: latency rings, zero-cost when disarmed

The latency monitor tracks named *events* (fork, expire-cycle,
command, aof-write...), each in a fixed ring of 160 one-second
samples — `#define LATENCY_TS_LEN 160` (`src/latency.h:17`): memory
per event is `160 * 8` bytes, forever. Instrumentation is macros:

```c
#define latencyStartMonitor(var) if (server.latency_monitor_threshold) { \
    var = mstime(); } else { var = 0; }                     /* latency.h:50 */
#define latencyAddSampleIfNeeded(event,var) \
    if (server.latency_monitor_threshold && \
        (var) >= server.latency_monitor_threshold) \
          latencyAddSample((event),(var));                  /* latency.h:63 */
```

When `latency-monitor-threshold` is 0 — the default, set in
`src/config.c:3271` — both macros collapse to a single compare on a
server global: no `mstime()` call, no sample. This is
zero-cost-when-off as macro discipline; you can sprinkle these pairs
through the codebase without budgeting for them. When armed,
`latencyAddSample` (`src/latency.c:63`) fetches the event's ring
from a dict, updates the max, coalesces same-second samples (keeping
the worse latency), and advances `idx` modulo 160. Why it matters:
the armed tier's cost is proportional to how *sick* the server is,
not how busy — a healthy server pays the compare and nothing else.

### Step 5 — tier 3a: the doctors are expert systems over the rings

`createLatencyReport` (`src/latency.c:182`) is LATENCY DOCTOR: it
walks every event's 160-sample ring, computes stats (min/max/avg/
mean-absolute-deviation), then runs rule-based checks — slow fork?
expire-cycle spikes? appendfsync misconfigured? — accumulating an
`advices` counter (from around `:200`) and emitting human-readable
paragraphs. `getMemoryDoctorReport` (`src/object.c:1421`) is the
same pattern for memory: fragmentation ratio, allocator stats,
eviction policy, printed as advice sentences.

```
 tier-2 rings (cheap, always bounded)      tier-3 doctor (expensive, on demand)
 [fork:        160 samples] ──┐
 [expire-cycle:160 samples] ──┼──► walk + stats + IF/THEN rules ──► prose
 [command:     160 samples] ──┘        (runs only when you type LATENCY DOCTOR)
```

Why it matters: the doctors do string formatting, allocation, and
O(events × 160) analysis — costs the fast path could never absorb —
but they read only bounded evidence tiers 1–2 already deposited, so
asking the question is always safe on a struggling server.

### Step 6 — tier 3b: the watchdog, when the loop can't confess

All previous tiers assume the event loop is running. When it isn't —
a command stuck in a loop, a module (FalkorDB!) blocking the main
thread — redis can't log anything, so the last tier interrupts from
outside the loop. `watchdogScheduleSignal` (`src/debug.c:2673`) arms
a one-shot `setitimer(ITIMER_REAL, ...)` for `watchdog-period`
milliseconds; the serverCron re-arms it each tick, so the SIGALRM
only actually fires if cron *stops running*. The handler,
`sigalrmSignalHandler` (`src/debug.c:2643`), logs
`--- WATCHDOG TIMER EXPIRED ---` and calls `logStackTrace`
(`src/debug.c:2115`) to dump where the main thread is stuck — from
inside a signal handler, using only async-signal-safe raw logging.
Disabled by default (`watchdog-period 0`): this tier's price is a
signal handler racing your code, so it is paid only when a human has
already decided the server is sick. For FalkorDB: when a graph query
wedges the main thread, this stack trace is the first artifact you
will ever see — and it points into your module.

## Where each step lives in the code

All paths relative to `~/repos/redis`. FalkorDB's existing C
counterpart is `~/repos/FalkorDB/src/slow_log/slow_log.c`
(`SlowLog_Add`) — hold it against Step 2 while reading.

| Step | Anchor | What to see |
|---|---|---|
| 2 | `src/slowlog.c:103` | `slowlogPushEntryIfNeeded` — `:104` negative disables, `:105` `>=` logs, then trim to max-len |
| 3 | `src/slowlog.c:28` | `slowlogCreateEntry` — arg-count cap, string truncation, dup-not-share |
| 4 | `src/latency.h:17` | `LATENCY_TS_LEN 160` — fixed ring per event |
| 4 | `src/latency.h:50`, `:63` | `latencyStartMonitor` / `latencyAddSampleIfNeeded` — one compare when off (default 0 at `src/config.c:3271`) |
| 4 | `src/latency.c:63` | `latencyAddSample` — ring insert, max update, same-second coalescing |
| 5 | `src/latency.c:182` | `createLatencyReport` — LATENCY DOCTOR's rule engine (advice counter from ~`:200`) |
| 5 | `src/object.c:1421` | `getMemoryDoctorReport` — MEMORY DOCTOR, same pattern |
| 6 | `src/debug.c:2673`, `:2643` | `watchdogScheduleSignal` + `sigalrmSignalHandler` → `logStackTrace` (`:2115`) |

Read order: slowlogPushEntryIfNeeded → slowlogCreateEntry → the two
latency.h macros → latencyAddSample → skim createLatencyReport for
its shape (don't read every rule) → the watchdog pair. Six anchors
carry the design; the doctors' rule bodies are trivia.

## Questions to answer in notes.md

1. Enumerate the exact SLOWLOG contract the topic-34 Rust stub's
   tests encode (threshold comparison, disable value, eviction
   order, id behavior across reset) and point to the line in
   `slowlogPushEntryIfNeeded` / `slowlogCreateEntry` implementing
   each clause. Which would you have gotten wrong from memory?
2. FalkorDB's `SlowLog_Add` runs per query, per graph, from
   concurrent threads — redis's slowlog is single-threaded main-loop
   code. What may the always-on tier cost under contention, and what
   does that imply for the M34 Rust port (lock, sharded ring,
   per-thread buffers)?
3. The latency monitor coalesces samples landing in the same second,
   keeping only the max (`latency.c:82`). What information does this
   deliberately throw away, and why is that the right trade for a
   160-slot ring whose consumer is a rule engine rather than a
   percentile dashboard?
4. Design GRAPH.DOCTOR: a `createLatencyReport`-style advice engine
   for a graph database. List at least four rules and the cheap
   evidence each needs deposited in advance (e.g., hot label scanned
   without an index, result serialization dominating execution time,
   matrix resize storms, BFS frontier repeatedly spilling).
5. The watchdog fires SIGALRM and walks the stack of whatever the
   main thread is doing — including FalkorDB module code mid-
   GraphBLAS-call. What must be true of the handler's code for this
   to be safe, and what could a module-aware watchdog additionally
   report (query text? graph key?) within those constraints?

## Done when

- [ ] You can state the three-tier cost theorem and assign each of
      the five surfaces (SLOWLOG, latency rings, two doctors,
      watchdog) to its tier with its per-command cost.
- [ ] You can recite the SLOWLOG contract precisely — `>=` logs,
      negative disables, oldest evicted at max-len, ids monotonic
      across RESET — and your Rust stub passes its contract tests.
- [ ] You can explain why `slowlogCreateEntry` truncates and
      duplicates arguments, including the FLUSHALL ASYNC race the
      `:58` comment describes.
- [ ] You can trace what happens, function by function, when a
      FalkorDB query blocks the main thread for 2× watchdog-period.

## References

**Code**
- [redis](https://github.com/redis/redis) — cloned at
  `~/repos/redis`; the anchors above are the read
- FalkorDB's existing surface:
  `~/repos/FalkorDB/src/slow_log/slow_log.c` (`SlowLog_Add`) — the C
  implementation M34 ports to Rust
- This topic's `SlowLog` Rust stub and its contract tests — Step 2
  reshaped

**Docs**
- [SLOWLOG GET](https://redis.io/docs/latest/commands/slowlog-get/)
  — the observable contract (entry fields, ids, reset semantics)
- [Latency monitor](https://redis.io/docs/latest/operate/oss_and_stack/management/optimization/latency-monitor/)
  — events, threshold, LATENCY DOCTOR usage
