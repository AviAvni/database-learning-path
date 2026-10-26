# Topic 45 notes — async I/O & modern SSDs

## Baseline (provided lane, Apple M3 Pro, measured 2026-10-26)

`cargo run --release --bin io_bench`. Internal SSD (APPLE SSD AP0512Z, Apple
Fabric), 1 GiB test file in the system temp directory, written and read with
`F_NOCACHE` + `F_RDAHEAD 0`. Offsets and file contents are seeded and
reproduce exactly; every timing is one run on this machine.

### Lane 1a — random 4 KiB reads, queue depth bought with threads

| QD | IOPS | MiB/s | p50 µs | p99 µs | mean µs | Little's-law prediction | achieved |
|---|---|---|---|---|---|---|---|
| 1 | 13,810 | 54 | 68.8 | 80.6 | 72.2 | 13,843 | 100% |
| 2 | 27,155 | 106 | 69.5 | 110.6 | 73.5 | 27,686 | 98% |
| 4 | 52,450 | 205 | 72.0 | 115.9 | 76.1 | 55,373 | 95% |
| 8 | 96,907 | 379 | 74.0 | 155.5 | 82.4 | 110,746 | 88% |
| 16 | 155,741 | 608 | 91.5 | 199.4 | 102.4 | 221,491 | 70% |
| 32 | 244,300 | 954 | 112.5 | 316.5 | 130.3 | 442,983 | 55% |
| 64 | 248,363 | 970 | 203.0 | 583.9 | 254.3 | 885,965 | 28% |

- **Little's law is exact; the prediction uses the wrong latency.** The
  predicted column holds latency at its QD-1 value. Feed each row its own mean
  instead and the law closes: QD 8 → 8 / 82.4 µs = 97,087 vs 96,907 measured
  (0.2%); QD 64 → 64 / 254.3 µs = 251,671 vs 248,363 (1.3%). Past QD 16 the
  device queues internally and every extra in-flight read buys latency, not
  throughput.
- **Sanity checks, all passed in this run:** QD-1 p50 68.8 µs is above the
  8 µs cold-read floor (`file.rs`, `COLD_FLOOR_US`); the best random row,
  970 MiB/s, is 16% of the measured sequential ceiling of 5,967 MiB/s (1 MiB
  reads, 8 threads); and `pread`, 8 threads and `lio_listio` return the same
  checksum `9786a6be5036cf04` over 2,000 shared offsets.
- Run-to-run spread on this machine across the four full runs whose output
  was kept (three while building the lane, one from `./verify.sh 45`):
  QD-1 13,330–14,499 IOPS; QD-64 237,589–248,363; the sequential ceiling
  5,967–6,340 MiB/s. In the `verify.sh` run QD 64 came out *below* QD 32
  (242,384 vs 250,497) — past saturation the extra threads only add queueing.
  Compare shapes and ratios, not the third digit.
- Lane 1b's prediction column is the fragile one: the mean of the max of 16
  QD-1 draws is decided by the QD-1 sample's tail, and it ranged 82.9–149.5 µs
  across the three runs that printed it while the measured batch p50 stayed at
  187.7–189.2 µs. The conclusion (the batch waits for its slowest read, and
  concurrent reads slow each other) holds in every run; the predicted IOPS
  figure should not be quoted to more than one significant digit.

### Lane 1b — POSIX AIO `lio_listio(LIO_WAIT)`

| batch | IOPS | syscalls/IO | batch p50 µs | predicted batch µs (max of b QD-1 draws) | predicted IOPS |
|---|---|---|---|---|---|
| 1 | 13,385 | 2.000 | 71.8 | 72.3 | 13,822 |
| 4 | 39,273 | 1.250 | 89.0 | 85.6 | 46,706 |
| 16 | 79,200 | 1.062 | 189.2 | 124.8 | 128,245 |

Batch 16 reaches 79,200 IOPS, **0.51× the 155,741 of sixteen blocking
threads**, with 1/16 of the submission syscalls. The barrier is the cost: a
batch ends when its slowest read does. syscalls/IO = 1/b + 1, because every
result needs its own `aio_return` — POSIX AIO batches submission only.

An early draft of this lane claimed macOS caps POSIX AIO at its 4 kernel
worker threads (`sysctl kern.aiothreads` = 4). The measurement refutes it:
79,200 IOPS is above the 52,450 that QD-4 threads achieve, so more than four
of the sixteen reads are in flight. The claim was removed rather than
explained away; how XNU schedules AIO is not something this topic read.

### Lane 1c — warm page cache, ns per 4 KiB page (best of 3)

| pages per call | 1 | 2 | 4 | 8 | 16 | 32 | 64 |
|---|---|---|---|---|---|---|---|
| ns / page | 612 | 435 | 271 | 225 | 217 | 196 | 195 |
| vs 1 per call | 1.00× | 1.41× | 2.26× | 2.72× | 2.82× | 3.13× | 3.14× |

Two-term fit from the 1 and 64 columns: ≈ 424 ns per call + 188 ns per page.
The io_uring paper's "roughly 5–6×" at batch 16 (§2.1) is for io_uring
submissions, not cached `preadv`; this lane's per-page floor includes a 4 KiB
copy no batching removes.

### Lane 2 — no reference numbers, and why

macOS has no io_uring. A Linux container on the same machine (Docker Desktop,
kernel `7.0.14-linuxkit`, `rust:1-bookworm`) builds the crate with
`-D warnings` and runs lanes 1 and 3, but:

- `io_uring_setup` fails with **EPERM** under Docker's default seccomp
  profile — the bench prints `[stub — io_uring_setup refused here (Operation
  not permitted (os error 1)); …]`. Running with seccomp disabled was not
  done.
- The container's own numbers fail the sanity check: QD-1 p50 35.8 µs (half the
  bare-metal SSD's 68.8), and a sequential "device ceiling" of **16,872 MiB/s**,
  2.8× the physical drive's 5,967. The VM's disk image is cached by the host,
  so `O_DIRECT` inside the guest does not reach flash. Recorded only as the
  example of a benchmark lying in a way its own sanity line catches.

A reference implementation of `uring_reads` was written and compiled for
Linux against `io-uring` 0.7 in that container; it was not run. So lane 2's
tests are a contract with no measured reference — the prediction to beat is
lane 1a's thread row at the same queue depth, plus the paper's §2.1 and
Table 1 cycle counts.

### Lane 3 — in-place baseline on the simulated FTL

Device model: 1,000 addressable erase blocks × 256 pages (4 KiB) + 100
over-provisioned; greedy GC; one write stream. Database: 230,144 pages (90% of
the addressable space minus a 256-page doublewrite buffer); 768,000 user page
writes, the first half uncounted.

| workload | DB WAF | SSD WAF | total |
|---|---|---|---|
| uniform, in-place + doublewrite | 2.00 | 4.13 | 8.25 |
| Zipf(0.8), in-place + doublewrite | 2.00 | 4.46 | 8.91 |

The simulator is deterministic, so these reproduce to the digit on any
machine (the Linux container printed the same 8.25 and 8.91).

### Lane 3 — reference numbers for the out-of-place stub

From a reference implementation of `oop::run_out_of_place` (greedy DB GC,
round-robin open zones, no TRIM), run against the same streams before the
function was stubbed. These are what lane 3 should print once you implement it:

| workload | configuration | DB WAF | SSD WAF | total | mean victim valid ratio |
|---|---|---|---|---|---|
| uniform | zone = erase block, 1 open | 5.13 | 1.00 | 5.13 | 0.805 |
| uniform | zone = ½ erase block, 1 open | 5.00 | 2.00 | 10.00 | 0.800 |
| uniform | zone = erase block, 2 open | 5.18 | 2.02 | 10.45 | 0.807 |
| uniform | zone = ¼ erase block, 4 open | 4.83 | 4.43 | 21.39 | 0.793 |
| Zipf(0.8) | zone = erase block, 1 open | 6.36 | 1.00 | 6.36 | 0.843 |
| Zipf(0.8) | zone = ½ erase block, 1 open | 6.18 | 2.13 | 13.16 | 0.838 |
| Zipf(0.8) | zone = erase block, 2 open | 6.42 | 2.97 | 19.07 | 0.844 |
| Zipf(0.8) | zone = ¼ erase block, 4 open | 5.97 | 4.45 | 26.54 | 0.832 |

- 1 / (1 − 0.805) = 5.13: the greedy-GC identity holds to the digit (§4.1's
  "1/(1 − 0.75) = 4"). The stub test allows 3%.
- **Aligned + single stream is the only configuration that beats in-place**
  (5.13 vs 8.25 uniform, 6.36 vs 8.91 skewed). Every misaligned or multiplexed
  configuration is worse than in-place, up to 2.6× worse at ¼-block zones with
  4 open (21.39 vs 8.25) — the model's version of the paper's §7.2 finding that
  switching LeanStore out of place first *raised* total WAF 1.66× (Table 1:
  4.06 × 1.94 = 7.88 against 2.00 × 2.36 = 4.72).
- Skew hurts the out-of-place engine more than the in-place one (DB WAF 5.13 →
  6.36), because greedy GC with one stream mixes hot and cold pages in every
  zone. That is §4.1's motivation for deathtime grouping (exercise 4).
- Test-sized device (64-page blocks, 200 + 20 blocks) from the same reference:
  in-place uniform 2.00/3.78/7.57; aligned 4.44/1.00/4.44 (victims 0.775 valid);
  half-block 4.01/2.51/10.09. These are what the four stub tests check against.

## Predictions (fill BEFORE implementing the stubs)

| question | prediction | actual |
|---|---|---|
| lane 2, QD 32 batch 1: IOPS vs lane 1a's QD-32 thread row | | |
| lane 2, QD 32 batch 32: io_uring_enter calls per read | | |
| lane 2, QD 32 batch 32: p99 vs batch 1 — higher or lower, and by how much? | | |
| CPU cycles per read, threads vs ring, at QD 32 (`perf stat`) | | |
| lane 3, aligned zone: SSD WAF | | |
| lane 3, aligned zone: DB WAF at uniform, 90% fill | | |
| lane 3, half-block zones: SSD WAF | | |
| which out-of-place configuration first beats in-place on total WAF? | | |
| with GDT (exercise 4): DB WAF at Zipf(0.8) | | |

## Paper numbers worth keeping

- **io_uring (Jasny et al., VLDB'26).** Naive swap: 1.06× (buffer manager,
  libaio 173.0 → io_uring 183.5 k tx/s) and 1.10× (shuffle, epoll 12.0 → 13.2
  GiB/s); redesigned: 2.05× and 2.31× (§1, Figure 1). Synchronous Posix =
  synchronous io_uring = 16.5 k tx/s (§3.3). Feature ladder 16.5 → 19.1 →
  183.5 → 216.6 → 237.8 → 300.5 → 376.4 → 546.5 k tx/s (Figure 5). Table 1:
  read 70 µs, write 12 µs; tx 8,264 cycles; single read 10,200; batched read
  5,400; batched write 5,700. Batching latency (Table 2, 8 workers, 1.5 MIOPS):
  11.51 / 24.22 / 60.62 / 116.40 / 200.85 / 317.51 µs at batch 1 / 8 / 32 / 64 /
  128 / 256. SQPoll wake ~30 µs; io-wq NOP +7.3 µs (§2.2). PostgreSQL: fixed
  buffers 4–6%, +IOPOLL up to 7.5%, +SQPOLL 11–15% (§5.2, Figure 18); the
  abstract's "14%" is the top of that range.
- **How to Write to SSDs (Lee, Ziegler & Leis, VLDB'26).** In-place LeanStore
  writes 18.85 KiB of flash per 4 KiB page write, 4.7× (§1, Figure 1); DB WAF
  2.00 × SSD WAF 2.36 (§2). Table 1 (800 GB, PM9A3): 229 → 535 K OPS; DB WAF
  2.00 → 4.06 (out-of-place) → 0.60; SSD WAF 2.36 → 1.94 → 1.00. Across six
  SSDs, flash writes per op down 6.2× (Solidigm D7-P5520) to 9.76× (Kioxia
  CM7-R) (§7.4). TPC-C 15,000 warehouses: 2.45× transactions, 7.2× fewer flash
  writes (§7.6).
- **xNVMe (Houlborg, Tietgen et al., CIDR'26).** DuckDB with `nvmefs`: "up to
  50% lower query times" on I/O-intensive queries (abstract); most TPC-H queries
  within ~10% of the baseline because the OS page cache absorbs the I/O (§5.1);
  ~20% average reduction on the wide aggregation variants (§5.2); SPDK backend
  4–30%, io_uring passthrough "almost halves" (§5.3).

## Cross-topic threads

- Topic 6's buffer pool is the M45 integration point; its CLOCK victim search is
  where eviction writes would be batched (+BatchEvict, the paper's first step).
- Topic 5's fsync ladder is the QD-1 write path; the AIO README's FUA argument
  is the same latency arithmetic.
- Topic 4's compaction is DB GC under another name; the WAF identity
  1/(1 − v) applies to a compaction that rewrites live data too.

## Open questions

- How many of lane 1b's sixteen `lio_listio` reads are really concurrent on
  macOS, and what bounds it? (Read XNU's `bsd/kern/kern_aio.c`.)
- Does the PostgreSQL heuristic the io_uring paper describes ("enabling
  IO_ASYNC after four outstanding I/Os", §5.2) still apply to direct I/O? In
  PostgreSQL `701f021` `pgaio_uring_should_use_async` returns false for
  non-buffered I/O before it looks at the in-flight count
  (`method_io_uring.c:722-723`) — see reading-postgres-aio.md.
- Lane 3 has no TRIM. With TRIM on DB GC, how much of the misaligned
  configurations' SSD WAF disappears?

## Implementation log

- [ ] `uring::uring_reads` — both tests green on Linux bare metal
- [ ] `oop::run_out_of_place` — all four tests green; lane 3 rows match the
      reference table
- [ ] prediction table reconciled
- [ ] stretch: adaptive batching (exercise 3)
- [ ] stretch: GDT placement (exercise 4) and NoWA (exercise 5)
