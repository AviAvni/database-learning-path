# Redis AOF & RDB: the command stream is the log

Redis logs the *commands themselves* (AOF) and checkpoints by *forking* (RDB) —
and since a graph module's data lives inside redis's keyspace, this is the
durability FalkorDB actually has today. Before the code, this chapter builds the
design step by step: what a command log is, what the fsync policy knob really
promises, why a command log must be rewritten, and how fork + copy-on-write
turns the OS into a snapshot engine. Read it as the incumbent your M5 design
competes with.

Every line number below was read at **redis/redis@e87dda2a4** (`unstable`,
2026-10-26;
`tools/pinned-source.py show redis <path> -r A:B`). Every timing comes from
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
// src/aof.c — feedAppendOnlyFile's tail, 1700-1708
  1700      /* Append to the AOF buffer. This will be flushed on disk just before
  1701       * of re-entering the event loop, so before the client will get a
  1702       * positive reply about the operation performed. */
  1703      if (server.aof_state == AOF_ON ||
  1704          (server.aof_state == AOF_WAIT_REWRITE && server.child_type == CHILD_TYPE_AOF))
  1705      {
  1706          server.aof_buf = sdscatlen(server.aof_buf, buf, sdslen(buf));
  1707          server.aof_cmd_duration += duration;
  1708      }
```

The line to focus on is 1706, the only one that touches the log: an in-memory
string append. Line 1707 adds the command's execution time to a running total
redis keeps so it can estimate how long replaying the current INCR file would
take — a reminder that replay cost is command-shaped.

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
// src/aof.c — flushAppendOnlyFile's policy tail, 1591-1616
  1591      /* Perform the fsync if needed. */
  1592      if (server.aof_fsync == AOF_FSYNC_ALWAYS) {
  1593          /* redis_fsync is defined as fdatasync() for Linux in order to avoid
  1594           * flushing metadata. */
        // ... 1595-1598: latency monitor start and a comment ...
  1599          if (redis_fsync(server.aof_fd) == -1) {
        // ... 1600-1601: serverLog(LL_WARNING, "Can't persist AOF ...") ...
  1602              exit(1);
  1603          }
        // ... 1604-1605: latency sample "aof-fsync-always" ...
  1606          server.aof_last_incr_fsync_offset = server.aof_last_incr_size;
  1607          server.aof_last_fsync = server.mstime;
  1608          atomicSet(server.fsynced_reploff_pending, server.master_repl_offset);
  1609      } else if (server.aof_fsync == AOF_FSYNC_EVERYSEC &&
  1610                 server.mstime - server.aof_last_fsync >= 1000) {
  1611          if (!sync_in_progress) {
  1612              aof_background_fsync(server.aof_fd);
  1613              server.aof_last_incr_fsync_offset = server.aof_last_incr_size;
  1614          }
  1615          server.aof_last_fsync = server.mstime;
  1616      }
```

The two lines carrying the argument are 1599 — the durability call made inline,
on the main thread, with `exit(1)` at 1602 if it fails — and 1612, where the
`everysec` branch hands the same call to a background thread instead
(`aof_background_fsync`, `aof.c:1237`, which queues a `bioCreateFsyncJob`).

Three policies (`AOF_FSYNC_NO 0`, `AOF_FSYNC_ALWAYS 1`, `AOF_FSYNC_EVERYSEC 2`
— `server.h:698–700`), and note that `no` has no branch at all: redis simply
never issues a durability call and the kernel's writeback timer decides. On
Linux that default is `vm.dirty_expire_centisecs = 3000`, i.e. **30 seconds**.

**Which rung is `always`?** This is the fact worth carrying away. `redis_fsync`
is a macro, and it is not the same call everywhere:

```c
// src/config.h — 144-151
   144  /* Define redis_fsync to fdatasync() in Linux and fsync() for all the rest */
   145  #if defined(__linux__)
   146  #define redis_fsync(fd) fdatasync(fd)
   147  #elif defined(__APPLE__)
   148  #define redis_fsync(fd) fcntl(fd, F_FULLFSYNC)
   149  #else
   150  #define redis_fsync(fd) fsync(fd)
   151  #endif
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

So the common shorthand "an fsync per command caps redis at ~1K writes/s" does
not describe this code: redis never fsyncs per command. What `always` actually
costs is a **latency floor**, not a throughput cap: 297 commands still ride each
2.97 ms flush.

**The reply ordering is the contract.** `beforeSleep` flushes the AOF *before*
it writes replies, and says why:

```c
// src/server.c — beforeSleep, 2095-2099
  2095      /* Write the AOF buffer on disk,
  2096       * must be done before handleClientsWithPendingWrites and
  2097       * sendPendingClientsToIOThreads, in case of appendfsync=always. */
  2098      if (server.aof_state == AOF_ON || server.aof_state == AOF_WAIT_REWRITE)
  2099          flushAppendOnlyFile(0);
```

`handleClientsWithPendingWrites()` runs later in the same function, at
`server.c:2146`. So under `always`
the durability call completes *before* the client is told "OK" — a genuine
durable-before-ack, the same contract as postgres group commit. Under
`everysec` and `no`, only the `write()` completes first, and the ack is on the
page cache.

**The window under `everysec` is not exactly one second.** Two constants set
it. The fsync is issued when at least 1000 ms have passed (`aof.c:1610`). But
if a background fsync is still running, redis postpones the *write* too, and
keeps postponing for up to 2000 ms (`aof.c:1458`) before giving up:

```c
// src/aof.c — flushAppendOnlyFile's postponement, 1448-1466
  1448      if (server.aof_fsync == AOF_FSYNC_EVERYSEC && !force) {
        // ... 1449-1451: comment — "we can try to delay the write for a couple of seconds" ...
  1452          if (sync_in_progress) {
  1453              if (server.aof_flush_postponed_start == 0) {
        // ... 1454-1455: comment ...
  1456                  server.aof_flush_postponed_start = server.mstime;
  1457                  return;
  1458              } else if (server.mstime - server.aof_flush_postponed_start < 2000) {
        // ... 1459-1460: comment ...
  1461                  return;
  1462              }
        // ... 1463-1464: comment — "we can't wait over two seconds" ...
  1465              server.aof_delayed_fsync++;
  1466              serverLog(LL_NOTICE,"Asynchronous AOF fsync is taking too long (disk is busy?). Writing the AOF buffer without waiting for fsync to complete, this may slow down Redis.");
```

The line to focus on is 1458: the 2000 ms cap. Each `return` above it leaves
the commands in `aof_buf` — and, under `everysec`, their clients already
acknowledged.

So the honest statement of the `everysec` window is: normally up to ~1 s of
acknowledged writes, stretching toward ~2 s when the disk cannot keep up — and
redis tells you when that happens, both in the log and in the
`aof_delayed_fsync` counter.

**One more silent rung change.** With `no-appendfsync-on-rewrite yes`, the fsync
is skipped entirely while any child process is doing I/O (`aof.c:1588–1589`) —
during a rewrite or a BGSAVE, `always` quietly becomes `no`, i.e. the bottom
rung, for the duration.

**And an opt-in durable ack.** Redis tracks `fsynced_reploff`
(`server.c:2104–2116`) so a client can issue `WAITAOF` and block until its write
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
// src/aof.c — rewriteAppendOnlyFileBackground, 3276-3308
  3276      /* We set aof_selected_db to -1 in order to force the next call to the
  3277       * feedAppendOnlyFile() to issue a SELECT command. */
  3278      server.aof_selected_db = -1;
  3279      flushAppendOnlyFile(1);
  3280      if (openNewIncrAofForAppend() != C_OK) {
  3281          server.aof_lastbgrewrite_status = C_ERR;
  3282          return C_ERR;
  3283      }
        // ... 3285-3299: full-sync offset bookkeeping, stat_aof_rewrites++ ...
  3301      if ((childpid = redisFork(CHILD_TYPE_AOF)) == 0) {
  3302          char tmpfile[256];
  3303
  3304          /* Child */
  3305          redisSetProcTitle("redis-aof-rewrite");
  3306          redisSetCpuAffinity(server.aof_rewrite_cpulist);
  3307          snprintf(tmpfile,256,"temp-rewriteaof-bg-%d.aof", (int) getpid());
  3308          if (rewriteAppendOnlyFile(tmpfile) == C_OK) {
```

Read the ordering: a **forced** flush first (`flushAppendOnlyFile(1)` at
`:3279`, the `force` argument that bypasses the everysec postponement of Step
2), then a *new* INCR file is opened (`:3280`), and only then the fork
(`:3301`). The parent keeps serving and appends new commands to the new INCR
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
// src/aof.c — the AOF Manifest file implementation, 53-75
    53   * Append-only files consist of three types:
    54   *
    55   * BASE: Represents a Redis snapshot from the time of last AOF rewrite. The manifest
    56   * file contains at most a single BASE file, which will always be the first file in the
    57   * list.
    58   *
    59   * INCR: Represents all write commands executed by Redis following the last successful
    60   * AOF rewrite. In some cases it is possible to have several ordered INCR files. For
        // ... 61-63: the two cases — a rewrite in progress, or a failed one ...
    64   *
    65   * HISTORY: After a successful rewrite, the previous BASE and INCR become HISTORY files.
    66   * They will be automatically removed unless garbage collection is disabled.
    67   *
    68   * The following is a possible AOF manifest file content:
    69   *
    70   * file appendonly.aof.2.base.rdb seq 2 type b
    71   * file appendonly.aof.1.incr.aof seq 1 type h
        // ... 72-73: incr seq 2 and 3, also type h ...
    74   * file appendonly.aof.4.incr.aof seq 4 type i
    75   * file appendonly.aof.5.incr.aof seq 5 type i
```

The line to focus on is 70: the BASE file is named `.base.rdb`.

Squint and this is topic 4 wholesale:

| redis | LSM equivalent |
|---|---|
| BASE (`type b`) | the bottom level — a compacted rendering of all history |
| INCR (`type i`) | L0 — recent appends, replayed in `seq` order |
| rewrite | full compaction |
| manifest | the MANIFEST |
| HISTORY (`type h`) | obsolete files awaiting GC |

Note the BASE file's extension in that example: `appendonly.aof.2.base.rdb`.
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
// src/rdb.c — rdbSaveBackground, 2317-2336
  2317  int rdbSaveBackground(int req, char *filename, rdbSaveInfo *rsi, int rdbflags) {
        // ... 2318-2319: pid_t childpid; ...
  2320      if (hasActiveChildProcess()) return C_ERR;
        // ... 2321-2325: save statistics ...
  2326      if ((childpid = redisFork(CHILD_TYPE_RDB)) == 0) {
  2327          int retval;
  2328
  2329          /* Child */
  2330          redisSetProcTitle("redis-rdb-bgsave");
  2331          redisSetCpuAffinity(server.bgsave_cpulist);
  2332          retval = rdbSave(req, filename,rsi,rdbflags);
        // ... 2333-2335: report the child's copy-on-write size ...
  2336          exitFromChild((retval == C_OK) ? 0 : 1, 0);
```

The line to focus on is 2326: the snapshot *is* a `fork`; everything after it
runs in the child.

Correctness is delegated to the OS: **copy-on-write** (COW) means parent and
child share all memory pages until the parent *writes* one, at which point the
kernel copies that page — so the child sees a frozen instant of the keyspace for
free.

The file ends with a **CRC64 trailer** — a **CRC** (cyclic redundancy check) is
a checksum computed by polynomial division over the bytes, and CRC64 is the
64-bit variant, so any corruption of the file changes it with overwhelming
probability. The save side writes it as the last 8 bytes (`rdb.c:2160–2164`),
and the load side compares it once the whole file has been read
(`rdb.c:5192–5210`, the compare at `:5201`). That makes an RDB
**all-or-nothing**: a truncated or corrupt snapshot is rejected wholesale, where
a truncated AOF is merely replayed up to the last complete command. Two log
formats, two different answers to "what does a torn tail mean" — and both are
defensible, because a snapshot has no useful prefix while a command log does.

Verifying the trailer means computing a CRC64 over every byte of a possibly
multi-GB file during startup, so *where* that computation happens is a
performance decision. The RDB parser asks for data in tiny pieces — a type byte,
a length, a short string — and a table-driven CRC run on a 1-byte buffer pays
its per-call overhead for one byte of work. So the file loader does not checksum
what the parser asks for; it checksums what it reads from the disk:

```c
// src/rio.c — rioFileLoadRead and the checksum it folds in, 204-256
   204  #define RIO_FILE_LOAD_BUFLEN (4*1024)
        // ... 205-210: comment ...
   211  static void rioFileLoadChecksumChunk(rio *r, const void *buf, ssize_t n) {
   212      if (r->flags & RIO_FLAG_CKSUM_AT_SOURCE) {
   213          off_t cksum_len = r->io.file.content_end - r->io.file.read_offset;
   214          if (cksum_len > n) cksum_len = n;
   215          if (cksum_len > 0) r->cksum = crc64(r->cksum, buf, (size_t)cksum_len);
   216      }
   217      r->io.file.read_offset += n;
   218  }
        // ... 219-249: rioFileLoadRead serves from read_buf, or reads large requests straight into the caller ...
   250          ssize_t n = read(fileno(r->io.file.fp), r->io.file.read_buf, RIO_FILE_LOAD_BUFLEN);
   251          if (n <= 0) return 0;
   252          rioFileLoadChecksumChunk(r, r->io.file.read_buf, n);
```

The line to focus on is 215: one `crc64()` call per physical read of up to
4 KB, clipped at `content_end`. `rioInitWithFileLoad` sets that boundary to
`filesize - 8` (`rio.c:273`), so the 8-byte trailer itself never enters the sum
even when one read spans both the last content bytes and the trailer. A request
of 4 KB or more skips the buffer and is read straight into the caller's memory,
then checksummed the same way (`:238–246`). Because the bytes are now summed at
the source, the per-request progress callback must not sum them again; it checks
the flag first (`rdb.c:4735`, `RIO_FLAG_CKSUM_AT_SOURCE` defined at
`rio.h:31`). The loader uses this path whenever it knows the file size
(`rdb.c:5302–5305`).

Large buffers are what let the CRC itself be fast. `crc64()` dispatches on
length (`crc64.c:293–299`): with a CPU that has **PCLMULQDQ** — the x86-64
carry-less multiply instruction, which multiplies polynomials over GF(2), the
arithmetic a CRC is made of — any buffer of at least `CRC64_PCLMUL_CUTOFF = 64`
bytes (`crc64.c:168`) takes the folding path that consumes 64 bytes per loop
iteration (`crc64_pclmul`, `:192`); shorter buffers, and CPUs without the
instruction, use the 8-tables-of-256 lookup implementation (`crcspeed64native`).
Run it once on the two request shapes: a parser read of a 1-byte type tag is
below the 64-byte cutoff, so checksumming at the request would always take the
table path; a 4,096-byte chunk is 4096 / 64 = **64** blocks of 64 bytes — the
first loads the four 16-byte accumulators (`crc64.c:202–208`) and the other
**63** go through the folding loop (`:210–217`) — in a single call. The same load path decompresses LZF-compressed strings with
`lzf_decompress2` (`rdb.c:438`). None of this changes the contract — the file is
still rejected wholesale on a mismatch; it makes verifying each byte cheaper.

The price of the fork is paid in page copies under write load: a write-hot
parent duplicates its working set, and worst case a multi-GB dataset approaches
2× RAM during the snapshot. This cost reaches all the way down into
data-structure design. Topic 2's dict does **not** simply "disable rehashing
during BGSAVE" — it raises the threshold:

```c
// src/server.c — updateDictResizePolicy, 829-842
   829  /* This function is called once a background process of some kind terminates,
   830   * as we want to avoid resizing the hash tables when there is a child in order
   831   * to play well with copy-on-write (otherwise when a resize happens lots of
   832   * memory pages are copied). The goal of this function is to update the ability
        // ... 833-834: rest of the comment ...
   835  void updateDictResizePolicy(void) {
   836      if (server.in_fork_child != CHILD_TYPE_NONE)
   837          dictSetResizeEnabled(DICT_RESIZE_FORBID);
   838      else if (hasActiveChildProcess())
   839          dictSetResizeEnabled(DICT_RESIZE_AVOID);
   840      else
   841          dictSetResizeEnabled(DICT_RESIZE_ENABLE);
   842  }
```

Three states, not two. Inside the child, resizing is **forbidden** outright. In
the parent while a child lives, it is **avoided**: `dictExpandIfNeeded`
(`dict.c:1662–1688`, the condition at `:1678–1681`) then only expands once
used/buckets reaches `dict_force_resize_ratio`, which is **4** (`dict.c:46`), instead of the normal
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

Anchors verified at redis/redis@e87dda2a4.

- **Step 1 — `aof.c:1671–1711`**: `feedAppendOnlyFile`; the RESP serialisation
  at `:1698` (`catAppendOnlyGenericCommand`, defined `:1619`); the buffer append
  and its ordering comment `:1700–1708`.
- **Step 2 — `aof.c:1409–1617`**: `flushAppendOnlyFile`. Empty-buffer fast path
  and the everysec catch-up `:1414–1443`; the write postponement `:1448–1467`
  (2000 ms cap at `:1458`); the write `:1480`; `no-appendfsync-on-rewrite`
  `:1588–1589`; the policy tail `:1591–1616` — `AOF_FSYNC_ALWAYS` `:1592–1608`
  (`redis_fsync` `:1599`, `exit(1)` on failure `:1602`), `AOF_FSYNC_EVERYSEC`
  `:1609–1615` (1000 ms interval `:1610`, `aof_background_fsync` `:1612`,
  defined `:1237` → `bioCreateFsyncJob`). Policy constants `server.h:698–700`.
  `redis_fsync` `config.h:144–151`. Reply ordering `server.c:2095–2099` and
  `:2146`; `WAITAOF` bookkeeping `server.c:2104–2116`.
- **Steps 3–4 — `aof.c`**: `rewriteAppendOnlyFileBackground` `:3259–3336` —
  forced flush `:3279`, new INCR opened `:3280`, fork `:3301`, child writes a
  temp BASE `:3307–3308`. Multi-part AOF manifest documentation `:47–76`;
  naming constants `:78–84`.
- **Step 5 — `rdb.c`**: `rdbSaveBackground` `:2317–2351`, fork `:2326`; CRC64
  trailer written `:2160–2164`, verified on load `:5192–5210` (compare
  `:5201`); the progress callback that skips already-summed bytes `:4731–4736`;
  the loader choosing the chunked reader `:5302–5305`. Chunked reader
  `rio.c:204–257` (`rioFileLoadChecksumChunk` `:211–218`,
  `rioInitWithFileLoad` `:265–274`); `RIO_FLAG_CKSUM_AT_SOURCE` `rio.h:31`.
  `crc64()` dispatch `crc64.c:293–299`, `CRC64_PCLMUL_CUTOFF` `:168`,
  `crc64_pclmul` `:192–235`. COW-driven resize policy `server.c:829–842`; the
  threshold it selects `dict.c:1662–1688` with `dict_force_resize_ratio = 4` at
  `dict.c:46`.

## Questions to answer in notes.md

1. `everysec` acks before durability. State the exact loss window from the two
   constants in the code (`aof.c:1610` and `aof.c:1458`), and explain why redis
   postpones the *write* rather than the ack when the bio fsync falls behind.
2. `appendfsync always` is `fdatasync` on Linux and `F_FULLFSYNC` on macOS
   (`config.h:144–151`). Using the ladder in `notes.md`, compute the added
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
  (`server.c:2095–2099`, `:2146`). `everysec` — normally up to ~1 s
  (`aof.c:1610` issues the fsync when 1000 ms have elapsed), stretching toward
  ~2 s when a bio fsync is behind and writes are postponed (`aof.c:1458`); the
  client is acked after the `write()`, so **this is the one that acks before
  durability**. `no` — unbounded; redis issues no durability call and the
  kernel's writeback timer decides (Linux default 30 s).

  </details>

- [ ] `appendfsync always` costs the same on Linux and macOS. True or false?

  <details><summary>Answer</summary>

  False, and the gap is huge. `redis_fsync` is `fdatasync(fd)` on Linux and
  `fcntl(fd, F_FULLFSYNC)` on macOS (`config.h:144–151`) — the middle and top
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
  ordering constraint is at `aof.c:3279–3301`: force-flush the current buffer,
  **open the new INCR file, and only then fork**. Any command executed between
  opening the INCR and forking is captured by both the new INCR and the child's
  BASE (harmlessly, since replay is BASE-then-INCR); a command in the gap the
  other way round would be in neither.

  </details>

- [ ] A truncated AOF and a truncated RDB behave differently at load time. Say
      how, and why the difference is right.

  <details><summary>Answer</summary>

  An RDB carries a CRC64 over the whole file (`rdb.c:2160–2164`), checked on
  load (`rdb.c:5192–5210`) against a running CRC the loader accumulated in 4 KB
  chunks as it read (`rio.c:211–218`, the trailer's own 8 bytes clipped out); a
  truncated or corrupt one is rejected **wholesale**.
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
  `updateDictResizePolicy` (`server.c:829–842`) therefore sets `DICT_RESIZE_AVOID`
  in the parent while any child lives — and `DICT_RESIZE_FORBID` inside the child
  itself. Under AVOID, `dictExpandIfNeeded` (`dict.c:1662–1688`) only expands at
  `dict_force_resize_ratio = 4` (`dict.c:46`) instead of a 1:1 load factor. It is
  not disabled, it is made four times less likely — a durability mechanism
  reaching down and retuning a data structure two topics away.

  </details>

## References

**Code** — all anchors read at `redis/redis@e87dda2a4`; local clone at
`~/repos/redis`, pin recorded in `resources/codebases.md`.

| file | what this chapter took from it |
|---|---|
| `src/aof.c` | the command log (Step 1), fsync policies and postponement (Step 2), rewrite (Step 3), the multi-part manifest (Step 4) |
| `src/config.h` | `redis_fsync` — which rung `appendfsync always` lands on, per platform (Step 2) |
| `src/server.c` | `beforeSleep`'s flush-before-reply ordering and `WAITAOF` bookkeeping (Step 2); `updateDictResizePolicy` (Step 5) |
| `src/server.h` | the three `AOF_FSYNC_*` constants (Step 2) |
| `src/rdb.c` | fork-based snapshot and the CRC64 trailer (Step 5) |
| `src/rio.c`, `src/rio.h` | the chunked file reader that checksums at the source (Step 5) |
| `src/crc64.c` | the length-dispatched CRC64 — PCLMULQDQ folding at ≥ 64 bytes (Step 5) |
| `src/dict.c` | `dict_force_resize_ratio`, the COW-driven threshold change (Step 5) |

**Measurements** — `topics/05-durability-wal/notes.md`, "Baseline (provided
lane, Apple M3 Pro / APFS, measured 2026-07-28)", produced by
`experiments/src/bin/fsync_ladder.rs`. `FINDINGS.md` row 5 carries the
headline.

**Manual pages** — macOS `fsync(2)` for why `F_FULLFSYNC` exists; Linux
`proc(5)` / `vm.dirty_expire_centisecs` for the 30 s default behind
`appendfsync no`.
