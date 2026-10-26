# Redis doctors: a database that confesses at a price it can afford

Redis ships with its own diagnosticians: SLOWLOG remembers every
command that took too long, the latency monitor keeps a fixed-size
history of bad events, LATENCY DOCTOR and MEMORY DOCTOR turn that
history into prose advice, and the software watchdog interrupts a
stuck server to log where it is standing. For a FalkorDB developer
this is not analogy — this code IS the host process your module runs
inside, and `slowlogPushEntryIfNeeded` fires after every
`GRAPH.QUERY`. The repo is cloned at `~/repos/redis`, pinned at
`redis@e87dda2a4` (`unstable`, 2026-10-26); this is a code-read, ~1.5h, across four small
files. The anchor table below maps each step to an exact file:line,
and every snippet is quoted from that pinned SHA with real gutters.

## The problem in one sentence

A production database must diagnose itself — record slow commands,
spot latency spikes, explain memory bloat, unstick a hung event loop
— while adding effectively zero overhead to the fast path, because
any instrumentation that costs more than the anomaly it detects will
be turned off.

## The concepts, step by step

### Step 1 — the theorem: tier the diagnosis surface by cost

> **In:** the problem statement above — "zero overhead on the fast
> path, useful answers on demand."
> **Out:** a three-tier cost model that the next five steps each fill
> in with one concrete Redis surface.

Redis solves the **observer-cost problem** (the act of measuring must
cost less than what it measures, or it gets disabled) by splitting
diagnosis into three tiers, each with a hard budget it can always
afford:

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

> **In:** the "always-on" tier named in Step 1.
> **Out:** the exact fast-path contract (`>=` logs, negative disables,
> oldest evicted) that Step 3 then shows must also bound the *record*
> it creates.

`slowlogPushEntryIfNeeded` is called after EVERY command completes,
with the measured `duration` in microseconds. The entire always-on
tier is the two comparisons at `:106`–`:107`:

```c
// src/slowlog.c:105
105  int slowlogPushEntryIfNeeded(client *c, robj **argv, int argc, long long duration) {
106      if (server.slowlog_log_slower_than < 0 || server.slowlog_max_len == 0) return 0;
107      if (duration >= server.slowlog_log_slower_than) {
108          listAddNodeHead(server.slowlog,
109                          slowlogCreateEntry(c,argv,argc,duration));
110  
111          /* Remove old entries if needed. */
112          while (listLength(server.slowlog) > server.slowlog_max_len)
113              listDelNode(server.slowlog,listLast(server.slowlog));
114          return 1;
115      }
116      return 0;
117  }
```

The line to focus on is 107, the one comparison every command pays.
The semantics are load-bearing and easy to get subtly wrong. `>=` not
`>`: a command *equal* to the threshold logs (`:107`). A *negative*
threshold disables the log entirely (`:106`), and
`slowlog-log-slower-than` defaults to `10000` µs = 10 ms
(`src/config.c:3686`); setting it to `0` logs everything. The list is a
head-insert (`:108` `listAddNodeHead`) and trims from the *tail*
(`:113` `listDelNode(..., listLast(...))` — `listLast` is the oldest),
so the **oldest** entry is evicted at `slowlog-max-len`. Entry ids
(`se->id = server.slowlog_entry_id++` in `slowlogCreateEntry`,
`src/slowlog.c:71`) increase monotonically and are NOT reset by
`SLOWLOG RESET`, so a poller never confuses a new entry with one it
already saw.

The return value (`:114`, `:116`) says whether an entry was added, and
the caller turns it into counters. `slowlogPushCurrentCommand`
(`src/server.c:3950`) bumps a server-wide count, sum and max of logged
durations and the same three per command (`:3959`–`:3967`), which INFO
reports as `slowlog_commands_count` / `_time_ms_sum` / `_time_ms_max`
(`:7143`–`:7145`) and as `slowlog_count=…` fields in `cmdstat_<name>`
(`:6509`–`:6518`). Those adds sit inside the `if` — they run only for a
command that was already slow — so the fast path is still one compare,
and the log, which forgets old entries at `slowlog-max-len`, gains a
running total that does not.

Topic 34's `SlowLog` Rust stub is exactly this function reshaped: its
contract tests encode `>=`-logs, negative-disables,
fixed-ring-oldest-evicted, ids-monotonic-across-reset — pass them and
you have reimplemented `:105`–`:117`. Why it matters: this is the one
tier that runs unconditionally, so its cost — one branch on a server
global — is the floor of the whole design.

### Step 3 — the evidence must not become the problem

> **In:** the entry that Step 2's `:108`–`:109` creates via
> `slowlogCreateEntry`.
> **Out:** the two caps (argc, string length) and the dup-not-share
> rule that keep one logged command from pinning megabytes of keyspace.

`slowlogCreateEntry` (`src/slowlog.c:28`) shows the second-order
discipline: the diagnostic record itself is bounded, because a slow
command is often slow *because* its arguments are huge. Two caps, both
runtime configs (`CONFIG SET`-able) read from server globals:

- `slowlog-entry-max-argc`, default `32` (`src/config.c:3653`, stored in
  `server.slowlog_max_argc`): argument slots beyond 31 are collapsed
  into a single `"... (%d more arguments)"` marker
  (`src/slowlog.c:32`, `:40`–`:43`). The entry still records the
  command's true argument count in `se->cmd_argc` (`:34`), which
  `SLOWLOG GET` returns as the seventh field of each entry (`:178`,
  `:187`).
- `slowlog-entry-max-string-len`, default `128` (`src/config.c:3676`,
  stored in `server.slowlog_max_string_len`): any argument longer than
  that is truncated and a `"... (%lu more bytes)"` suffix records how
  many bytes were dropped (`src/slowlog.c:46`–`:55`).

Worked on a concrete command. `SET giant-key <10 MB value>` has a value
of `10 × 1024 × 1024 = 10,485,760` bytes. At the default cap the entry
keeps the first `128` bytes, and the suffix reports `10,485,760 − 128 = 10,485,632`
more bytes:

```
 SET giant-key <10 MB value>        slowlog entry (bounded):
 ──────────────────────────►   [SET][giant-key][first 128 B + "... (10485632 more bytes)"]
```

Non-shared argument objects are *duplicated* (`dupStringObject`,
`src/slowlog.c:65`), not refcounted — the comment at `:59`–`:64` explains
why: sharing an `robj` between the slowlog and the keyspace means
`FLUSHALL ASYNC` could free the object on a background thread while the
log still points at it. Why it matters: the log observes values, it
must never own them, and it must never let a pathological command turn
a diagnostic into an OOM.

### Step 4 — tier 2: latency rings, zero-cost when disarmed

> **In:** the "armed" tier from Step 1 — the middle budget.
> **Out:** the fixed-ring data structure and the macro pair whose cost
> is one compare when disarmed, which Step 5's doctors then read.

The latency monitor tracks named *events* (fork, expire-cycle,
command, aof-write…), each in a fixed ring of 160 one-second samples —
`#define LATENCY_TS_LEN 160` (`src/latency.h:17`). A sample is a
`{time, latency}` pair, so per-event memory is `160 × sizeof(sample)`,
bounded forever regardless of load. Instrumentation is two macros
whose whole body is guarded by a single server global:

```c
// src/latency.h:50
50  #define latencyStartMonitor(var) if (server.latency_monitor_threshold) { \
51      var = mstime(); \
52  } else { \
53      var = 0; \
54  }
     // ... :58 latencyEndMonitor computes (mstime() - var) under the same guard ...
63  #define latencyAddSampleIfNeeded(event,var) \
64      if (server.latency_monitor_threshold && \
65          (var) >= server.latency_monitor_threshold) \
66            latencyAddSample((event),(var));
```

When `latency-monitor-threshold` is `0` — the default, set in
`src/config.c:3687` — every one of these macros collapses to a single
`if` on a server global: no `mstime()` syscall, no sample write. This
is **zero-cost-when-off** as macro discipline; you can sprinkle the
`Start`/`End`/`AddSampleIfNeeded` triple through the codebase without
budgeting for it. When armed, `latencyAddSample` (`src/latency.c:63`)
does the ring bookkeeping:

- fetch the event's ring from a dict, creating it on first sight
  (`:64`, `:69`–`:75`);
- update the all-time max (`:77`);
- **coalesce same-second samples**: if the previous slot's timestamp
  equals `now`, keep only the worse latency and return (`:81`–`:86`);
- otherwise write the new slot and advance `idx` modulo `LATENCY_TS_LEN`
  (`:88`–`:92`).

Why it matters: the armed tier's cost is proportional to how *sick* the
server is, not how *busy* — a healthy server pays the compare and
nothing else, and even a spiking server writes at most one ring slot
per event per second.

### Step 5 — tier 3a: the doctors are expert systems over the rings

> **In:** the bounded rings Step 4 deposits (and the memory-overhead
> data Redis already tracks).
> **Out:** two on-demand text reports whose *only* inputs are those
> cheap tiers, so asking is always safe on a struggling server.

`createLatencyReport` (`src/latency.c:182`) is LATENCY DOCTOR: it walks
every event's 160-sample ring, computes stats (min/max/avg/mean-
absolute-deviation), then runs rule-based checks — slow fork?
expire-cycle spikes? `appendfsync` misconfigured? — accumulating an
`advices` counter (`int advices = 0;`, `src/latency.c:200`) and
emitting human-readable paragraphs. Its output strings are *literal
source*, and are the signature you will grep for; quoted verbatim:

```c
// src/latency.c:207  — monitoring disabled branch
207          report = sdscat(report,"I'm sorry, Dave, I can't do that. Latency monitoring is disabled in this Redis instance. You may use \"CONFIG SET latency-monitor-threshold <milliseconds>.\" in order to enable it. If we weren't in a deep space mission I'd suggest to take a look at https://redis.io/docs/latest/operate/oss_and_stack/management/optimization/latency-monitor.\n");
     // ... :226 first spike seen ...
226  report = sdscat(report,"Dave, I have observed latency spikes in this Redis instance. You don't mind talking about it, do you Dave?\n\n");
     // ... :355 nothing wrong ...
355  report = sdscat(report,"Dave, no latency spike was observed during the lifetime of this Redis instance, not in the slightest bit. I honestly think you ought to sit down calmly, take a stress pill, and think things over.\n");
     // ... :362 advice header ...
362  report = sdscat(report,"\nI have a few advices for you:\n\n");
```

`getMemoryDoctorReport` (`src/object.c:1625`) is the same pattern for
memory. Its thresholds are explicit: "empty" if
`total_allocated < 5 MB` (`:1638`, `1024*1024*5`); big-peak if
`peak_allocated / total_allocated > 1.5` (`:1643`); high-frag if
`total_frag > 1.4 && total_frag_bytes > 10 MB` (`:1649`, `10<<20`).
Its literal strings, verbatim:

```c
// src/object.c:1694  — which report: none found / empty / issues
1694      if (num_reports == 0) {
1695          s = sdsnew(
1696          "Hi Sam, I can't find any memory issue in your instance. "
1697          "I can only account for what occurs on this base.\n");
1698      } else if (empty == 1) {
1699          s = sdsnew(
1700          "Hi Sam, this instance is empty or is using very little memory, "
1701          "my issues detector can't be used in these conditions. "
     // ... 1702–1704: the rest of the "empty" paragraph ...
1705      } else {
1706          s = sdsnew("Sam, I detected a few issues in this Redis instance memory implants:\n\n");
```

```
 tier-2 rings (cheap, always bounded)      tier-3 doctor (expensive, on demand)
 [fork:        160 samples] ──┐
 [expire-cycle:160 samples] ──┼──► walk + stats + IF/THEN rules ──► "Dave, ..."
 [command:     160 samples] ──┘        (runs only when you type LATENCY DOCTOR)
```

Why it matters: the doctors do string formatting, allocation, and
O(events × 160) analysis — costs the fast path could never absorb — but
they read only the bounded evidence tiers 1–2 already deposited, so
asking the question is always safe on a server that is already in
trouble.

### Step 6 — tier 3b: the watchdog, when the loop can't confess

> **In:** the failure case *all* prior tiers assume away — an event
> loop that has stopped running, so no tier can log anything.
> **Out:** an out-of-band SIGALRM stack dump, the last-resort artifact
> that for FalkorDB points straight into your wedged module.

All previous tiers assume the event loop is running. When it isn't — a
command stuck in a loop, a module (FalkorDB!) blocking the main thread
— Redis can't log anything, so the last tier interrupts from *outside*
the loop. `watchdogScheduleSignal` (`src/debug.c:2990`) arms a one-shot
`setitimer(ITIMER_REAL, ...)` for `watchdog-period` milliseconds (a
one-shot: `it_interval` is zero). `serverCron` re-arms it every tick
(`src/server.c:1612`), so the SIGALRM only actually fires if cron
*stops running* — i.e. if the loop is genuinely wedged. The handler,
`sigalrmSignalHandler` (`src/debug.c:2960`), logs
`--- WATCHDOG TIMER EXPIRED ---` (`:2974`) and calls `logStackTrace`
(`src/debug.c:2423`) to dump where the main thread is standing — from
inside a signal handler, using only async-signal-safe raw logging.

Disabled by default (`watchdog-period 0`): this tier's price is a
signal handler racing your code, so it is paid only once a human has
already decided the server is sick. For FalkorDB: when a graph query
wedges the main thread, this stack trace is the first artifact you will
ever see — and it points into your module. Why it matters: it closes
the coverage gap — every other tier needs a working loop to file its
report; this one is the report the loop cannot make itself.

## Where each step lives in the code

All paths relative to `~/repos/redis` at `redis@e87dda2a4`. FalkorDB's
existing C counterpart is `~/repos/FalkorDB/src/slow_log/slow_log.c`
(`SlowLog_Add`, `:190`; note the per-log `pthread_mutex_t lock` at
`:37` and the `pthread_mutex_lock` at `:222`) — hold it against Step 2
while reading, because FalkorDB pays a mutex per query where
single-threaded Redis pays none.

| Step | Anchor | What to see |
|---|---|---|
| 2 | `src/slowlog.c:105` | `slowlogPushEntryIfNeeded` — `:106` negative disables, `:107` `>=` logs, `:112`–`:113` trim to max-len (oldest evicted), `:114` returns 1 when logged |
| 2 | `src/slowlog.c:71` | `se->id = server.slowlog_entry_id++` — monotonic ids, survive RESET |
| 2 | `src/server.c:3950`, `:7143` | `slowlogPushCurrentCommand` — the caller; slowlog counters, reported by INFO |
| 3 | `src/slowlog.c:28` | `slowlogCreateEntry` — argc cap (`:32`, config at `src/config.c:3653`), string truncation (`:46`–`:55`, config at `src/config.c:3676`), `cmd_argc` (`:34`), `dupStringObject` (`:65`), FLUSHALL race (`:59`–`:64`) |
| 4 | `src/latency.h:17` | `LATENCY_TS_LEN 160` — fixed ring per event |
| 4 | `src/latency.h:50`, `:63` | `latencyStartMonitor` / `latencyAddSampleIfNeeded` — one compare when off (default 0 at `src/config.c:3687`) |
| 4 | `src/latency.c:63` | `latencyAddSample` — ring insert (`:88`), max update (`:77`), same-second coalescing (`:82`–`:84`) |
| 5 | `src/latency.c:182`, `:200`, `:207` | `createLatencyReport` — advice counter and literal "Dave" strings |
| 5 | `src/object.c:1625`, `:1694` | `getMemoryDoctorReport` — thresholds (`:1638`/`:1643`/`:1649`), literal "Sam" strings |
| 6 | `src/debug.c:2990`, `:2960` | `watchdogScheduleSignal` (re-armed by `server.c:1612`) + `sigalrmSignalHandler` → `logStackTrace` (`:2423`) |

Read order: `slowlogPushEntryIfNeeded` → `slowlogCreateEntry` → the two
`latency.h` macros → `latencyAddSample` → skim `createLatencyReport`
for its shape (don't read every rule) → the watchdog pair. The anchors
carry the design; the doctors' individual rule bodies are trivia.

## Questions to answer in notes.md

1. Enumerate the exact SLOWLOG contract the topic-34 Rust stub's tests
   encode (threshold comparison, disable value, eviction order, id
   behavior across reset) and point to the line in
   `slowlogPushEntryIfNeeded` / `slowlogCreateEntry` implementing each
   clause. Which would you have gotten wrong from memory?
2. FalkorDB's `SlowLog_Add` runs per query, per graph, from concurrent
   threads (`slow_log.c:222` takes a mutex) — Redis's slowlog is
   single-threaded main-loop code with no lock. What may the always-on
   tier cost under contention, and what does that imply for the M34
   Rust port (lock, sharded ring, per-thread buffers)?
3. The latency monitor coalesces samples landing in the same second,
   keeping only the max (`latency.c:82`–`:84`). What information does
   this deliberately throw away, and why is that the right trade for a
   160-slot ring whose consumer is a rule engine rather than a
   percentile dashboard?
4. Design GRAPH.DOCTOR: a `createLatencyReport`-style advice engine for
   a graph database. List at least four rules and the cheap evidence
   each needs deposited in advance (e.g., hot label scanned without an
   index, result serialization dominating execution time, matrix resize
   storms, BFS frontier repeatedly spilling).
5. The watchdog fires SIGALRM and walks the stack of whatever the main
   thread is doing — including FalkorDB module code mid-GraphBLAS-call.
   What must be true of the handler's code for this to be safe
   (async-signal-safety), and what could a module-aware watchdog
   additionally report (query text? graph key?) within those
   constraints?

## Done when

Answer each before unfolding it.

- [ ] You can state the three-tier cost theorem and assign each of the five surfaces (SLOWLOG, latency rings, two doctors, watchdog) to its tier with its per-command cost.

  <details><summary>Answer</summary>

  Tier 1, always-on: **SLOWLOG**, ~one integer compare on every command
  (`slowlog.c:106`–`:107`). Tier 2, armed: the **latency monitor**
  rings — one compare when `latency-monitor-threshold == 0` (the
  default), and at most one ring-slot write per event per second when
  armed (`latency.h:50`/`:63`, `latency.c:63`). Tier 3, on-demand:
  **LATENCY DOCTOR** and **MEMORY DOCTOR** (walk bounded evidence, build
  text — `latency.c:182`, `object.c:1625`) and the **software
  watchdog** (a SIGALRM stack dump — `debug.c:2990`/`:2960`), all paid
  only when a human asks. The invariant: expensive analysis never rides
  the hot path; the hot path only deposits cheap, bounded evidence.

  </details>

- [ ] You can recite the SLOWLOG contract precisely — `>=` logs, negative disables, oldest evicted at max-len, ids monotonic across RESET — and your Rust stub passes its contract tests.

  <details><summary>Answer</summary>

  From `slowlogPushEntryIfNeeded`: a negative `slowlog-log-slower-than`
  (or `slowlog-max-len == 0`) disables logging and returns (`:106`); a
  command whose `duration >= threshold` is logged, so equality logs, not
  just strictly-greater (`:107`); entries are head-inserted (`:108`) and
  trimmed from the tail (`:112`–`:113`), so the **oldest** is evicted
  once `slowlog-max-len` is exceeded. Ids come from
  `server.slowlog_entry_id++` in `slowlogCreateEntry` (`:71`): strictly
  increasing and never reset by `SLOWLOG RESET` (which only clears the
  list), so a poller can dedupe by id.

  </details>

- [ ] You can explain why `slowlogCreateEntry` truncates and duplicates arguments, including the FLUSHALL ASYNC race the `:59`–`:64` comment describes.

  <details><summary>Answer</summary>

  A slow command is frequently slow *because* its arguments are huge, so
  logging them verbatim would let one entry pin megabytes. The entry
  caps argument count at `slowlog-entry-max-argc` (default 32,
  `slowlog.c:32`) with a `"... (N more arguments)"` marker — keeping the
  true count in `cmd_argc` (`:34`) — and truncates any argument over
  `slowlog-entry-max-string-len` (default 128 bytes, `:46`–`:55`) with a
  `"... (N more bytes)"` suffix — a 10 MB value keeps 128 bytes and
  reports `10,485,632` more. It `dupStringObject`s non-shared args
  (`:65`) rather than bumping a refcount because sharing an `robj` with
  the keyspace means `FLUSHALL ASYNC` could free it on a background
  thread while the log still references it (the `:59`–`:64` comment).

  </details>

- [ ] You can trace what happens, function by function, when a FalkorDB query blocks the main thread for 2× watchdog-period.

  <details><summary>Answer</summary>

  With `watchdog-period` set (non-zero), `watchdogScheduleSignal`
  (`debug.c:2990`) has armed a one-shot `setitimer(ITIMER_REAL, ...)`.
  Normally `serverCron` re-arms it every tick (`server.c:1612`), so it
  never fires. When a FalkorDB query wedges the main thread, `serverCron`
  stops running, the timer is not re-armed, and after `watchdog-period`
  ms the kernel delivers SIGALRM to `sigalrmSignalHandler`
  (`debug.c:2960`). The handler logs `--- WATCHDOG TIMER EXPIRED ---`
  (`:2974`) and calls `logStackTrace` (`debug.c:2423`), dumping the
  stack of the wedged main thread — which is inside your module's
  GraphBLAS call — using only async-signal-safe logging.

  </details>

## References

**Code** (pinned at `redis@e87dda2a4`)
- [redis](https://github.com/redis/redis) — cloned at `~/repos/redis`;
  the anchors above are the read.
- FalkorDB's existing surface:
  `~/repos/FalkorDB/src/slow_log/slow_log.c` (`SlowLog_Add`, `:190`) —
  the C implementation M34 ports to Rust; note its per-log mutex.
- This topic's `SlowLog` Rust stub and its contract tests — Step 2
  reshaped.

**Docs**
- [SLOWLOG GET](https://redis.io/docs/latest/commands/slowlog-get/) —
  the observable contract (entry fields, ids, reset semantics).
- [Latency monitor](https://redis.io/docs/latest/operate/oss_and_stack/management/optimization/latency-monitor/)
  — events, threshold, LATENCY DOCTOR usage.
