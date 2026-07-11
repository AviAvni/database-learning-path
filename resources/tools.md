# Tools

## Benchmarking

| Tool | Use |
|------|-----|
| criterion.rs | Rust microbenchmarks (statistical, fights noise) |
| divan | faster-iteration Rust benches |
| hyperfine | CLI-level benchmarks |
| redis-benchmark / memtier_benchmark | RESP server load testing |
| YCSB (or rust port) | standard KV workloads A–F |
| BenchBase (CMU) | TPC-C, TPC-H, and 20+ workloads against SQL DBs |
| ClickBench | analytics benchmark (topic 12) |
| ann-benchmarks | recall/QPS curves for vector indexes (topic 14) |
| LDBC SNB | graph benchmark standard (topic 13) |
| pgbench | postgres load gen |

## Profiling & observation

| Tool | Use |
|------|-----|
| cargo flamegraph / samply | CPU flamegraphs on macOS/Linux |
| perf (Linux) + `perf stat -d` | hardware counters: cache misses, branch misses, IPC |
| perf c2c | false sharing detection (topic 9) |
| Instruments (macOS) | time profiler, allocations, syscalls |
| heaptrack / dhat-rs | allocation profiling |
| bpftrace / eBPF | fsync latency, block IO tracing (topic 5) |
| iostat / fio | raw disk characterization — know your hardware baseline |
| tokio-console | async runtime introspection (topic 7) |

## Correctness

| Tool | Use |
|------|-----|
| proptest / quickcheck | property-based testing vs model oracle |
| cargo-fuzz (libFuzzer) | fuzz parsers, SST/page decoders |
| Miri | UB detection in unsafe Rust |
| loom | exhaustive interleaving checks for lock-free code (topic 9) |
| ThreadSanitizer / ASan | C/C++ and FFI sanitizing |
| SQLancer | logic-bug finding in SQL engines |
| Jepsen + elle | distributed consistency checking (topic 15) |
| TLA+ / PlusCal | model checking protocols (optional, topic 15/16) |
| Z3 (via `z3` crate) | SMT solving: prove query rewrites equivalent, check invariants (topic 16) |
| strace / dtruss | verify what syscalls actually happen (fsync lies) |

## Rust crates that recur

`tokio`, `crossbeam` (epoch, skiplist), `hashbrown`, `parking_lot`, `memmap2`,
`io-uring`, `arrow`/`parquet`, `sqlparser`, `openraft`, `rand`/`zipf` (workload gen)
