# Redis AOF & RDB: the command stream is the log

Redis logs the *commands themselves* (AOF) and checkpoints by *forking* (RDB) —
and since a graph module's data lives inside redis's keyspace, this is the
durability FalkorDB actually has today. Before the code, this chapter builds the
design step by step: what a command log is, what the fsync policy knob really
promises, why a command log must be rewritten, and how fork + copy-on-write
turns the OS into a snapshot engine. Read it as the incumbent your M5 design
competes with.

Every line number below was read at **redis/redis@a176d1225**
(`tools/pinned-source.py show redis <path> -r A:B`). Every timing comes from
this topic's provided lane, `cargo run --release --bin fsync_ladder`, on the
Apple M3 Pro / APFS machine recorded in `notes.md`.

**Vocabulary, once, before it is used.** A *WAL* (write-ahead log) is a
sequential file a change is written to, and made durable in, before the state it
describes may be considered committed; an *AOF* is redis's WAL, and it logs
commands rather than pages or deltas. A *checkpoint* is a complete rendering of
current state that lets the log before it be discarded; RDB is redis's. *Group
commit* is letting one durability call serve many operations. And the three
rungs of the durability ladder, which this chapter names every time it says
"fsync":

| call | what it guarantees | measured p50 here |
|---|---|---|
| `write()` alone | bytes in the OS page cache; survives `kill -9`, not power loss | **1.17 µs** |
| `fdatasync()` / macOS `fsync()` | bytes handed to the drive; its volatile cache may still hold them | **22.67 µs** |
| macOS `fcntl(fd, F_FULLFSYNC)` | the drive flushed its cache to stable media | **2.97 ms** |

**19.4×** from the first to the second, a further **131×** to the third — 856
898 → 44 109 → 337 implied durable operations per second. The middle row was
measured on macOS as `fsync(2)`; there is no `fdatasync` on this machine
(`fsync_ladder.rs` compiles that lane out), so it is named only because it
occupies the same rung on Linux. Redis picks a different rung *on different
platforms for the same config value*, which is the single most surprising fact
in this chapter (Step 2).

## The problem in one sentence

Redis serves ~100K+ commands/s from one thread, so it can afford neither a
durability call on the command path — on this machine the top rung is 2.97 ms,
which would put a 2.97 ms floor under every write's latency — nor any pause to
write a snapshot; its durability design is entirely shaped by "the main thread
must never wait for the disk," and the price is a stated, configurable window of
acknowledged-but-lost writes.

## The concepts, step by step

### Step 1 — the command log: log what was *said*, not what changed

> **In:** a write command that has just executed against the in-memory
> keyspace.
> **Out:** the same command, re-serialised as RESP protocol text, appended to
> an in-memory buffer — with no file I/O on the command path at all.

An AOF ("append-only file") is a **command log**: instead of logging page images
(turso, `reading-turso-wal.md`) or record deltas (postgres,
`reading-postgres-xlog.md`), redis appends the write commands themselves,
literally as RESP protocol text — the same bytes a client would send. `SET
user:42 "avi"` goes into the log as `SET user:42 "avi"`. Recovery is replay:
start an empty server and feed it the file as if a very fast client were
retyping history.

The append itself is one `sdscatlen`, and the comment above it states the whole
contract:

```c
// src/aof.c — feedAppendOnlyFile's tail, 1438-1445
  1438      /* Append to the AOF buffer. This will be flushed on disk just before
  1439       * of re-entering the event loop, so before the client will get a
  1440       * positive reply about the operation performed. */
  1441      if (server.aof_state == AOF_ON ||
  1442          (server.aof_state == AOF_WAIT_REWRITE && server.child_type == CHILD_TYPE_AOF))
  1443      {
  1444          server.aof_buf = sdscatlen(server.aof_buf, buf, sdslen(buf));
  1445      }
```

The trade against the other designs is volume-vs-CPU, flipped. A command is
usually tiny — tens of bytes, the cheapest possible log record — but replay must
re-execute *full command processing*: parsing, dispatch, data-structure updates.
Recovery time therefore scales with **total command count**, not with final data
size. A key written a million times costs a million replays; in turso it would
cost one page image, in postgres one delta plus whatever survives checkpointing.

*Why it matters:* every other property in this chapter follows from putting the
log record on the *command* axis. Rewrite (Step 3) exists only because command
logs grow with history; fork-based snapshotting (Step 5) exists only because
that rewrite must not pause the server.

### Step 2 — the fsync policy: durability as a config knob

> **In:** an `aof_buf` holding one event loop's worth of commands, and a
> configured `appendfsync` value.
> **Out:** bytes in the page cache, plus zero, one, or a deferred durability
> call — and a client reply that is sent *after* whichever of those happened.

Once per event-loop iteration the buffer is `write()`n to the AOF file — which
only reaches the kernel's page cache, not the drive. *When to fsync* is a
user-facing policy, and this is the design's signature: redis makes the
durability window a **config choice** the other systems in this topic don't
offer.

```c
// src/aof.c — flushAppendOnlyFile's policy tail, 1329-1354 (logging elided)
  1329      /* Perform the fsync if needed. */
  1330      if (server.aof_fsync == AOF_FSYNC_ALWAYS) {
  1331          /* redis_fsync is defined as fdatasync() for Linux in order to avoid
  1332           * flushing metadata. */
  1337          if (redis_fsync(server.aof_fd) == -1) {
  1340              exit(1);
  1341          }
  1344          server.aof_last_incr_fsync_offset = server.aof_last_incr_size;
  1345          server.aof_last_fsync = server.mstime;
  1347      } else if (server.aof_fsync == AOF_FSYNC_EVERYSEC &&
  1348                 server.mstime - server.aof_last_fsync >= 1000) {
  1349          if (!sync_in_progress) {
  1350              aof_background_fsync(server.aof_fd);
  1351              server.aof_last_incr_fsync_offset = server.aof_last_incr_size;
  1352          }
  1353          server.aof_last_fsync = server.mstime;
  1354      }
```

Three policies (`AOF_FSYNC_NO 0`, `AOF_FSYNC_ALWAYS 1`, `AOF_FSYNC_EVERYSEC 2`
— `server.h:634–636`), and note that `no` has no branch at all: redis simply
never issues a durability call and the kernel's writeback timer decides. On
Linux that default is `vm.dirty_expire_centisecs = 3000`, i.e. **30 seconds**.

**Which rung is `always`?** This is the fact worth carrying away. `redis_fsync`
is a macro, and it is not the same call everywhere:

```c
// src/config.h — 128-135
   128  /* Define redis_fsync to fdatasync() in Linux and fsync() for all the rest */
   129  #if defined(__linux__)
   130  #define redis_fsync(fd) fdatasync(fd)
   131  #elif defined(__APPLE__)
   132  #define redis_fsync(fd) fcntl(fd, F_FULLFSYNC)
   133  #else
   134  #define redis_fsync(fd) fsync(fd)
   135  #endif
```

On Linux, `appendfsync always` sits on the **middle** rung (`fdatasync`). On
macOS the identical config line sits on the **top** rung (`F_FULLFSYNC`). The
measurement is only available on one of those platforms at a time — this
machine is a Mac, so `notes.md` records macOS `fsync` (22.67 µs) as the middle
rung and `F_FULLFSYNC` (2.97 ms) as the top, a **131×** gap. The Linux
`fdatasync` rung is not measured here; the safe statement is that redis on
macOS pays the top rung for the same config line that buys a middle rung on
Linux, and that on this hardware the two rungs are 131× apart. Redis is unusual
in reaching for the top rung at all, and honest about it: postgres ships
`F_FULLFSYNC` only behind a non-default `wal_sync_method`, and turso only behind
`PRAGMA fullfsync`.

**Work the arithmetic.** Redis does not fsync per command — it fsyncs at most
once per event-loop iteration, so the AOF is *already* group-committed by the
loop. At an offered load of 100 000 write commands/s:

```
appendfsync always, macOS (F_FULLFSYNC, T = 2.967 ms)
    flushes/s ceiling      = 1 / 0.002967      =    337
    commands per flush     = 100 000 × 0.002967 =    297
    added latency floor per command             ≈ 2.97 ms

appendfsync always, middle rung (macOS fsync measured here at T = 22.67 µs;
                    Linux fdatasync is the same rung but is NOT measured here)
    flushes/s ceiling      = 1 / 0.00002267    = 44 109
    commands per flush     = 100 000 × 0.00002267 =  2.27
    added latency floor per command             ≈ 23 µs

appendfsync everysec (one bio fsync per second, off the command path)
    commands per fsync     = 100 000
    added latency floor per command             ≈ 0
```

So the old claim in this file — "an fsync per command, ~1 ms each, would cap it
at ~1K/s" — is wrong twice: redis never fsyncs per command, and "1 ms" is from
nowhere. What `always` actually costs is a **latency floor**, not a throughput
cap: 297 commands still ride each 2.97 ms flush.

**The reply ordering is the contract, and it is not what the old pseudocode in
this file said.** `beforeSleep` flushes the AOF *before* it writes replies, and
says why:

```c
// src/server.c — beforeSleep, 1958-1962
  1958      /* Write the AOF buffer on disk,
  1959       * must be done before handleClientsWithPendingWrites and
  1960       * sendPendingClientsToIOThreads, in case of appendfsync=always. */
  1961      if (server.aof_state == AOF_ON || server.aof_state == AOF_WAIT_REWRITE)
  1962          flushAppendOnlyFile(0);
```

`handleClientsWithPendingWrites()` runs at `server.c:1998`. So under `always`
the durability call completes *before* the client is told "OK" — a genuine
durable-before-ack, the same contract as postgres group commit. Under
`everysec` and `no`, only the `write()` completes first, and the ack is on the
page cache.

**The window under `everysec` is not exactly one second.** Two constants set
it. The fsync is issued when at least 1000 ms have passed (`aof.c:1348`). But
if a background fsync is still running, redis postpones the *write* too, and
keeps postponing for up to 2000 ms (`aof.c:1196`) before giving up:

```c
// src/aof.c — flushAppendOnlyFile's postponement, 1186-1204 (logging elided)
  1186      if (server.aof_fsync == AOF_FSYNC_EVERYSEC && !force) {
  1190          if (sync_in_progress) {
  1191              if (server.aof_flush_postponed_start == 0) {
  1194                  server.aof_flush_postponed_start = server.mstime;
  1195                  return;
  1196              } else if (server.mstime - server.aof_flush_postponed_start < 2000) {
  1199                  return;
  1200              }
  1203              server.aof_delayed_fsync++;
  1204              serverLog(LL_NOTICE,"Asynchronous AOF fsync is taking too long (disk is busy?). ...");
```

So the honest statement of the `everysec` window is: normally up to ~1 s of
acknowledged writes, stretching toward ~2 s when the disk cannot keep up — and
redis tells you when that happens, both in the log and in the
`aof_delayed_fsync` counter.

**One more silent rung change.** With `no-appendfsync-on-rewrite yes`, the fsync
is skipped entirely while any child process is doing I/O (`aof.c:1326–1327`) —
during a rewrite or a BGSAVE, `always` quietly becomes `no`, i.e. the bottom
rung, for the duration.

**And an opt-in durable ack.** Redis tracks `fsynced_reploff`
(`server.c:1970–1978`) so a client can issue `WAITAOF` and block until its write
is genuinely fsynced. That is the escape hatch: per-connection postgres
semantics without paying them server-wide.

Sharpen the comparison with postgres: group commit *batches the flush but never
acks early* — the client waits until its LSN is durable. Redis's `always` is the
same contract at the granularity of an event loop. `everysec` is a genuinely
different contract: a *time*-based batch with the **ack before the flush**.

*Why it matters:* this is the only design in the topic that lets an operator
choose the durability window, and the choice is legible only if you know which
rung the chosen policy lands on — which, for `always`, depends on the operating
system.

### Step 3 — the rewrite problem: command logs grow with history, not with data

> **In:** an AOF whose length is proportional to everything ever said.
> **Out:** a fresh BASE file holding the shortest command sequence that
> reconstructs current state, produced without pausing the server.

A command log's size is proportional to *everything ever said*, not to the data:
1M `INCR counter` commands is 1M log records describing one 8-byte value. So the
AOF must periodically be **rewritten** — replaced by the shortest command
sequence that reconstructs the *current* state (one `SET counter 1000000`).

Redis does this without pausing, by forking:

```c
// src/aof.c — rewriteAppendOnlyFileBackground, 2664-2696 (error paths elided)
  2664      /* We set aof_selected_db to -1 in order to force the next call to the
  2665       * feedAppendOnlyFile() to issue a SELECT command. */
  2666      server.aof_selected_db = -1;
  2667      flushAppendOnlyFile(1);
  2668      if (openNewIncrAofForAppend() != C_OK) {
  2689      if ((childpid = redisFork(CHILD_TYPE_AOF)) == 0) {
  2692          /* Child */
  2693          redisSetProcTitle("redis-aof-rewrite");
  2695          snprintf(tmpfile,256,"temp-rewriteaof-bg-%d.aof", (int) getpid());
  2696          if (rewriteAppendOnlyFile(tmpfile) == C_OK) {
```

Read the ordering: a **forced** flush first (`flushAppendOnlyFile(1)` at
`:2667`, the `force` argument that bypasses the everysec postponement of Step
2), then a *new* INCR file is opened (`:2668`), and only then the fork
(`:2689`). The parent keeps serving and appends new commands to the new INCR
file; the child serialises current state into a fresh BASE at its leisure. When
the child finishes, BASE + INCR replace the old log.

*Why it matters:* the ordering is the correctness argument. If the new INCR
were opened after the fork, commands executed in the gap would be in neither
file.

### Step 4 — multi-part AOF: an LSM in disguise

> **In:** one BASE file, N INCR files, and a manifest naming them.
> **Out:** at recovery, the current keyspace — by loading the BASE and then
> replaying each INCR in sequence order.

The modern (7.0+) AOF is not one file but a set, described by redis's own
header comment:

```c
// src/aof.c — the AOF Manifest file implementation, 48-70
    48   * Append-only files consist of three types:
    50   * BASE: Represents a Redis snapshot from the time of last AOF rewrite. The manifest
    51   * file contains at most a single BASE file, which will always be the first file in the
    52   * list.
    54   * INCR: Represents all write commands executed by Redis following the last successful
    55   * AOF rewrite. In some cases it is possible to have several ordered INCR files.
    60   * HISTORY: After a successful rewrite, the previous BASE and INCR become HISTORY files.
    61   * They will be automatically removed unless garbage collection is disabled.
    63   * The following is a possible AOF manifest file content:
    65   * file appendonly.aof.2.base.rdb seq 2 type b
    66   * file appendonly.aof.1.incr.aof seq 1 type h
    69   * file appendonly.aof.4.incr.aof seq 4 type i
    70   * file appendonly.aof.5.incr.aof seq 5 type i
```

Squint and this is topic 4 wholesale:

| redis | LSM equivalent |
|---|---|
| BASE (`type b`) | the bottom level — a compacted rendering of all history |
| INCR (`type i`) | L0 — recent appends, replayed in `seq` order |
| rewrite | full compaction |
| manifest | the MANIFEST |
| HISTORY (`type h`) | obsolete files awaiting GC |

Note the BASE file's extension in redis's own example: `appendonly.aof.2.base.rdb`.
The BASE of an AOF is an **RDB** file (Step 5) when `aof-use-rdb-preamble` is on
— so a "modern AOF" is literally a checkpoint plus a tail of commands, which is
the structure of every WAL system in this topic.

Even the write-amplification question transfers: a rewrite's cost is (entire
dataset serialised) per (INCR data absorbed) — exactly a full-compaction WA.
Topic 4's vocabulary was never LSM-specific; it is the vocabulary of *any* log
that must be compacted.

*Why it matters:* it tells you what to measure. If you can express redis's
durability in topic-4 terms, you can price it with topic-4 arithmetic instead of
inventing new intuitions.

### Step 5 — RDB: checkpoint by fork, priced in COW

> **In:** a live keyspace under write load.
> **Out:** a consistent point-in-time binary snapshot with a CRC64 trailer,
> written by a child process, paid for in copied memory pages.

An RDB snapshot is durability by checkpoint alone: fork, and let the child walk
the entire keyspace writing a compact binary snapshot, while the parent serves
traffic.

```c
// src/rdb.c — rdbSaveBackground, 1859-1878 (bookkeeping elided)
  1859  int rdbSaveBackground(int req, char *filename, rdbSaveInfo *rsi, int rdbflags) {
  1862      if (hasActiveChildProcess()) return C_ERR;
  1868      if ((childpid = redisFork(CHILD_TYPE_RDB)) == 0) {
  1871          /* Child */
  1872          redisSetProcTitle("redis-rdb-bgsave");
  1874          retval = rdbSave(req, filename,rsi,rdbflags);
  1878          exitFromChild((retval == C_OK) ? 0 : 1, 0);
```

Correctness is delegated to the OS: **copy-on-write** (COW) means parent and
child share all memory pages until the parent *writes* one, at which point the
kernel copies that page — so the child sees a frozen instant of the keyspace for
free.

The file ends with a **CRC64 trailer** (a 64-bit checksum over the whole file,
written at `rdb.c:1702–1706`) and it is verified on load
(`rdb.c:4025–4038`, compare at `:4034`). That makes an RDB **all-or-nothing**:
a truncated or corrupt snapshot is rejected wholesale, where a truncated AOF is
merely replayed up to the last complete command. Two log formats, two different
answers to "what does a torn tail mean" — and both are defensible, because a
snapshot has no useful prefix while a command log does.

The price of the fork is paid in page copies under write load: a write-hot
parent duplicates its working set, and worst case a multi-GB dataset approaches
2× RAM during the snapshot. This cost reaches all the way down into
data-structure design. Topic 2's dict does **not** simply "disable rehashing
during BGSAVE" — it raises the threshold:

```c
// src/server.c — updateDictResizePolicy, 772-785
   772  /* This function is called once a background process of some kind terminates,
   773   * as we want to avoid resizing the hash tables when there is a child in order
   774   * to play well with copy-on-write (otherwise when a resize happens lots of
   775   * memory pages are copied). ... */
   778  void updateDictResizePolicy(void) {
   779      if (server.in_fork_child != CHILD_TYPE_NONE)
   780          dictSetResizeEnabled(DICT_RESIZE_FORBID);
   781      else if (hasActiveChildProcess())
   782          dictSetResizeEnabled(DICT_RESIZE_AVOID);
   783      else
   784          dictSetResizeEnabled(DICT_RESIZE_ENABLE);
   785  }
```

Three states, not two. Inside the child, resizing is **forbidden** outright. In
the parent while a child lives, it is **avoided**: `dictExpandIfNeeded`
(`dict.c:1648–1660`) then only expands once used/buckets reaches
`dict_force_resize_ratio`, which is **4** (`dict.c:45`), instead of the normal
1:1. So a rehash during BGSAVE is not impossible, just four times less likely —
because a rehash touches every bucket and would COW-copy the whole table.

Durability window with RDB alone: everything since the last snapshot — minutes.

*Why it matters:* this is the topic's clearest case of a durability mechanism
whose real cost shows up somewhere else entirely — in RSS, and in a hash table's
load factor.

### Step 6 — the FalkorDB angle: this is the incumbent

> **In:** a graph whose adjacency matrices live in redis's keyspace as module
> data.
> **Out:** exactly the two mechanisms above, applied to a data structure
> neither was designed for — and the baseline your M5 design must beat.

A graph module's data lives inside redis's keyspace, so its durability *is*
this file: RDB serialises the matrices via module callbacks, and AOF logs the
`GRAPH.QUERY` commands themselves. Two consequences to quantify in `notes.md`,
because they are the M5 comparison baseline:

1. **Replay re-executes parsing and planning.** A `GRAPH.QUERY` is not a data
   mutation, it is a query to be compiled. Estimate recovery time for 10M
   mutations replayed as `GRAPH.QUERY` text versus replayed as logical records,
   using the per-command cost you can measure.
2. **A snapshot forks and COWs the whole matrix set.** Under write load, a
   multi-GB graph approaches 2× RSS during BGSAVE, and the matrices are exactly
   the kind of large contiguous allocation that COW handles worst
   (one write dirties a page of a structure you then copy in full).

*Why it matters:* M5 is not a greenfield design. It is an argument that a
purpose-built log beats a command log plus a fork, and that argument needs both
of the numbers above.

## Where each step lives in the code

Anchors verified at redis/redis@a176d1225.

- **Step 1 — `aof.c:1409–1448`**: `feedAppendOnlyFile`; the RESP serialisation
  at `:1436` (`catAppendOnlyGenericCommand`, defined `:1357`); the buffer append
  and its ordering comment `:1438–1445`.
- **Step 2 — `aof.c:1147–1355`**: `flushAppendOnlyFile`. Empty-buffer fast path
  and the everysec catch-up `:1152–1181`; the write postponement `:1186–1205`
  (2000 ms cap at `:1196`); the write `:1218`; `no-appendfsync-on-rewrite`
  `:1326–1327`; the policy tail `:1329–1354` — `AOF_FSYNC_ALWAYS` `:1330–1345`
  (`redis_fsync` `:1337`, `exit(1)` on failure `:1340`), `AOF_FSYNC_EVERYSEC`
  `:1347–1353` (1000 ms interval `:1348`, `aof_background_fsync` `:1350`,
  defined `:983` → `bioCreateFsyncJob`). Policy constants `server.h:634–636`.
  `redis_fsync` `config.h:128–135`. Reply ordering `server.c:1958–1962` and
  `:1998`; `WAITAOF` bookkeeping `server.c:1970–1978`.
- **Steps 3–4 — `aof.c`**: `rewriteAppendOnlyFileBackground` `:2652–2720` —
  forced flush `:2667`, new INCR opened `:2668`, fork `:2689`, child writes a
  temp BASE `:2695–2696`. Multi-part AOF manifest documentation `:42–71`;
  naming constants `:73–75`.
- **Step 5 — `rdb.c`**: `rdbSaveBackground` `:1859–1892`, fork `:1868`; CRC64
  trailer written `:1702–1706`, verified on load `:4025–4038` (compare
  `:4034`). COW-driven resize policy `server.c:772–785`; the threshold it
  selects `dict.c:1648–1660` with `dict_force_resize_ratio = 4` at `dict.c:45`.

## Questions to answer in notes.md

1. `everysec` acks before durability. State the exact loss window from the two
   constants in the code (`aof.c:1348` and `aof.c:1196`), and explain why redis
   postpones the *write* rather than the ack when the bio fsync falls behind.
2. `appendfsync always` is `fdatasync` on Linux and `F_FULLFSYNC` on macOS
   (`config.h:128–135`). Using the ladder in `notes.md`, compute the added
   per-command latency floor on each, at an offered load of 100 000 write
   commands/s. Which platform's `always` would you be willing to run in
   production, and what would you tell a user who benchmarked on the other one?
3. AOF-as-LSM: map BASE / INCR / rewrite / manifest onto topic-4 terms. What is
   the "write amp" of an AOF rewrite, and how does `aof-use-rdb-preamble` change
   the answer?
4. Command-log vs page-image vs logical-record WAL: rank recovery speed and log
   volume for a graph-mutation workload; justify your M5 choice with the
   numbers, not the vibe.

## Done when

Answer each before unfolding it.

- [ ] State each `appendfsync` policy's durability window, and say which one
      acks the client before the data is durable.

  <details><summary>Answer</summary>

  `always` — zero window; the durability call completes inside
  `flushAppendOnlyFile`, which `beforeSleep` runs *before*
  `handleClientsWithPendingWrites` precisely for this reason
  (`server.c:1958–1962`, `:1998`). `everysec` — normally up to ~1 s
  (`aof.c:1348` issues the fsync when 1000 ms have elapsed), stretching toward
  ~2 s when a bio fsync is behind and writes are postponed (`aof.c:1196`); the
  client is acked after the `write()`, so **this is the one that acks before
  durability**. `no` — unbounded; redis issues no durability call and the
  kernel's writeback timer decides (Linux default 30 s).

  </details>

- [ ] `appendfsync always` costs the same on Linux and macOS. True or false?

  <details><summary>Answer</summary>

  False, and the gap is huge. `redis_fsync` is `fdatasync(fd)` on Linux and
  `fcntl(fd, F_FULLFSYNC)` on macOS (`config.h:128–135`) — the middle and top
  rungs of the ladder. On the machine in `notes.md` those two rungs measure
  **22.67 µs** and **2.97 ms**, a **131×** gap (the middle rung there is macOS
  `fsync`; `fdatasync` is compiled out on this platform, so the Linux figure is
  not measured here). Only the macOS call actually flushes the drive's write
  cache, so the macOS build is strictly more durable and strictly slower for the
  identical config line.

  </details>

- [ ] Explain the AOF rewrite as compaction, and name the ordering constraint
      that makes it correct.

  <details><summary>Answer</summary>

  A command log grows with history, not with data, so it is periodically
  replaced by the shortest command sequence that reproduces current state — a
  full compaction, with BASE as the bottom level and INCR files as L0. The
  ordering constraint is at `aof.c:2667–2689`: force-flush the current buffer,
  **open the new INCR file, and only then fork**. Any command executed between
  opening the INCR and forking is captured by both the new INCR and the child's
  BASE (harmlessly, since replay is BASE-then-INCR); a command in the gap the
  other way round would be in neither.

  </details>

- [ ] A truncated AOF and a truncated RDB behave differently at load time. Say
      how, and why the difference is right.

  <details><summary>Answer</summary>

  An RDB carries a CRC64 over the whole file (`rdb.c:1702–1706`), checked on
  load (`rdb.c:4025–4038`); a truncated or corrupt one is rejected **wholesale**.
  A truncated AOF is replayed up to the last complete command and the tail is
  discarded. The difference is right because a snapshot has no useful prefix —
  half a keyspace dump is not half a keyspace — while a command log's prefix is
  exactly a valid earlier state.

  </details>

- [ ] Why does redis change a hash-table tuning parameter while a BGSAVE is
      running?

  <details><summary>Answer</summary>

  Copy-on-write. A rehash touches every bucket, so it would COW-copy the entire
  table into the parent's private memory during the snapshot.
  `updateDictResizePolicy` (`server.c:772–785`) therefore sets `DICT_RESIZE_AVOID`
  in the parent while any child lives — and `DICT_RESIZE_FORBID` inside the child
  itself. Under AVOID, `dictExpandIfNeeded` (`dict.c:1648–1660`) only expands at
  `dict_force_resize_ratio = 4` (`dict.c:45`) instead of a 1:1 load factor. It is
  not disabled, it is made four times less likely — a durability mechanism
  reaching down and retuning a data structure two topics away.

  </details>

## References

**Code** — all anchors read at `redis/redis@a176d1225`; local clone at
`~/repos/redis`, pin recorded in `resources/codebases.md`.

| file | what this chapter took from it |
|---|---|
| `src/aof.c` | the command log (Step 1), fsync policies and postponement (Step 2), rewrite (Step 3), the multi-part manifest (Step 4) |
| `src/config.h` | `redis_fsync` — which rung `appendfsync always` lands on, per platform (Step 2) |
| `src/server.c` | `beforeSleep`'s flush-before-reply ordering and `WAITAOF` bookkeeping (Step 2); `updateDictResizePolicy` (Step 5) |
| `src/server.h` | the three `AOF_FSYNC_*` constants (Step 2) |
| `src/rdb.c` | fork-based snapshot and the CRC64 trailer (Step 5) |
| `src/dict.c` | `dict_force_resize_ratio`, the COW-driven threshold change (Step 5) |

**Measurements** — `topics/05-durability-wal/notes.md`, "Baseline (provided
lane, Apple M3 Pro / APFS, measured 2026-07-28)", produced by
`experiments/src/bin/fsync_ladder.rs`. `FINDINGS.md` row 5 carries the
headline.

**Manual pages** — macOS `fsync(2)` for why `F_FULLFSYNC` exists; Linux
`proc(5)` / `vm.dirty_expire_centisecs` for the 30 s default behind
`appendfsync no`.
