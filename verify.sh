#!/usr/bin/env bash
#
# verify.sh — re-derive the headline numbers in this repo on your own machine.
#
# Every measured claim in the guides comes from one of these benchmarks. This
# script builds and runs each one and prints its output, so you can check the
# numbers rather than take them on trust. Nothing is downloaded and nothing is
# mocked; the generators are seeded, so the figures should reproduce exactly
# apart from timings, which depend on your hardware.
#
#   ./verify.sh              run everything, print the measured lanes
#   ./verify.sh --summary    just the pass/fail table
#   ./verify.sh --list       show every lane and what it measures, run nothing
#   ./verify.sh 40 41        only these topics
#   ./verify.sh --criterion  also run the criterion lanes (slow, topic 0)
#
# Requires: a Rust toolchain (rustup.rs). First run compiles from scratch and
# takes a few minutes; later runs are cached. A full run is ~20 minutes on an
# M3 Pro, most of it in topics 6, 11, 14 and 25, which build big inputs.
#
# A note on what you will see: each benchmark has a first "lane" that is
# implemented and runs today — those are the numbers quoted in the guides. The
# later lanes are the reader's exercises and print "[stub — implement the
# todo!()s ...]" until you do them. That is intended, not a broken build.
#
# Seven binaries are deliberately NOT in this list, because they measure the
# reader's own implementation and so have nothing to report on a fresh clone:
#
#   03 disk_btree (bench)  05 crash_test    07 server       10 explain
#   04 write_amp           15 partition_test 16 dst_run
#
# Each of those prints one line saying which file to implement, and exits 0.
# Topics 3 and 7 have separate provided lanes here (btree_baseline,
# loopback_bench) that measure the same claims without the reader's code.

set -uo pipefail
cd "$(dirname "$0")"

SUMMARY_ONLY=0
LIST_ONLY=0
RUN_CRITERION=0
FILTER=()
for arg in "$@"; do
  case "$arg" in
    --summary) SUMMARY_ONLY=1 ;;
    --list) LIST_ONLY=1 ;;
    --criterion) RUN_CRITERION=1 ;;
    -h|--help) sed -n '3,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) FILTER+=("$arg") ;;
  esac
done

# topic dir : bench binary : what it measures
BENCHES=(
  "01-storage-engine-landscape:engine-shootout:the RUM conjecture priced — space amp, B-tree vs LSM"
  "02-in-memory-structures:rehash_spike:the doubling rehash you can see in the tail"
  "03-btree-internals:btree_baseline:height is one lever; cache residency is the other"
  "05-durability-wal:fsync_ladder:what a durable commit costs, per sync policy"
  "06-buffer-pool:pool_vs_mmap:mmap's tail — the page fault you cannot schedule"
  "07-networking-protocols:loopback_bench:pipelining, or the same work at 279x the rate"
  "08-transactions-mvcc:txn_bench:MVCC vs one big lock, across read/write mixes"
  "09-concurrency:false_sharing:the cost of sharing a cache line you never share"
  "09-concurrency:scaling:a global mutex scaling BACKWARDS with cores"
  "11-execution-models:exec_bench:tuple-at-a-time vs batch-at-a-time"
  "12-columnar-analytics:scan_bench:the memory-bandwidth floor a scan has to beat"
  "13-graph-engines:hop_bench:two-hop traversal, adjacency list vs CSR vs SpMV"
  "14-vector-search:ann_bench:brute-force recall and the QPS floor it sets"
  "15-replication-consensus:repl_lag:follower fsync policy vs ack latency"
  "16-testing-correctness:crash_matrix:which planted bugs seeded crash testing catches"
  "17-simd:simd_bench:autovectorization vs hand SIMD, per selectivity"
  "18-gpu:gpu_bench:the transfer tax, and where the GPU crossover is"
  "19-jit:jit_bench:interpreter vs vectorized, and the compile-time break-even"
  "20-graphblas:gb_bench:SpMV bandwidth, SpGEMM, and hypersparse index size"
  "21-formal:eqsat_bench:the rewrite-ordering trap that hand optimizers fall into"
  "22-benchmarks:bench_suite:TPC-H choke points and YCSB tails, measured"
  "23-fulltext:fts_bench:BM25 top-k and what exhaustive scoring costs"
  "24-graph-algorithms:algo_bench:PageRank, triangles and Dijkstra on RMAT vs uniform"
  "25-graph-ml:gnn_bench:the message-passing kernel is an SpMM"
  "26-probabilistic:filter_bench:what a point-miss costs before you add a filter"
  "27-streaming:ivm_bench:full recompute per batch — the bill incremental view maintenance pays"
  "28-cloud-native:tier_bench:local NVMe vs raw S3, at the tail"
  "29-distributed-txn:txn_bench:how much conflict the workload itself contains"
  "30-timeseries:tsdb_bench:delta+varint as the baseline Gorilla must beat"
  "31-crdts:crdt_bench:convergence and the metadata it costs"
  "32-htap:htap_bench:freshness vs analytical throughput"
  "33-temporal-graphs:temporal_bench:snapshot replay cost vs anchor+delta"
  "34-debugging:debug_bench:coordinated omission — a closed loop hides its own tail"
  "35-overload:overload_bench:metastable failure — the outage that outlives its trigger"
  "36-sharding:shard_bench:mod-N resharding moves almost everything"
  "37-distributed-query:distq_bench:fan-out turns rare slowness into common slowness"
  "38-graphrag-agent-memory:graphrag_bench:vector-RAG's multi-hop collapse"
  "39-fraud-identity-graphs:fraud_bench:camouflage defeats row-based fraud scores"
  "40-security-attack-graphs:attack_bench:the list view of privilege vs the graph view"
  "41-onchain-analytics:chain_bench:haircut tainting smears one theft over everyone"
  "42-recommendations-social:social_bench:the popularity trap"
  "43-ops-dependency-graphs:ops_bench:one gray failure, thirty-four alerts"
  "44-egraphs-egglog:ematch_bench:N matches, N^2 candidates — e-matching as a join"
)

# topic dir : criterion bench : what it measures   (--criterion only: minutes each)
CRITERION=(
  "00-performance-toolbox:cache_ladder:the memory hierarchy, one rung at a time"
  "00-performance-toolbox:lookup_shootout:where a HashMap lookup's time actually goes"
  "00-performance-toolbox:branch_misprediction:the branch you cannot predict, priced"
)

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
green() { printf '\033[32m%s\033[0m' "$1"; }
red() { printf '\033[31m%s\033[0m' "$1"; }

if [ $LIST_ONLY -eq 1 ]; then
  bold "═══ bin lanes (run by default)"
  for entry in "${BENCHES[@]}"; do
    IFS=: read -r dir bin what <<<"$entry"
    printf '   %-30s %-18s %s\n' "$dir" "$bin" "$what"
  done
  echo
  bold "═══ criterion lanes (--criterion)"
  for entry in "${CRITERION[@]}"; do
    IFS=: read -r dir bin what <<<"$entry"
    printf '   %-30s %-18s %s\n' "$dir" "$bin" "$what"
  done
  exit 0
fi

command -v cargo >/dev/null || { red "cargo not found"; echo " — install Rust from https://rustup.rs"; exit 1; }

declare -a RESULTS=()
FAILED=0

wanted() {
  [ ${#FILTER[@]} -eq 0 ] && return 0
  for f in "${FILTER[@]}"; do [[ "$1" == *"$f"* ]] && return 0; done
  return 1
}

run_lane() {
  local dir=$1 bin=$2 what=$3 kind=$4
  local crate="topics/$dir/experiments"
  [ -d "$crate" ] || { RESULTS+=("SKIP|$dir/$bin|missing"); return; }

  [ $SUMMARY_ONLY -eq 1 ] || { echo; bold "═══ topics/$dir — $what"; echo; }

  local start out rc dur stubs
  start=$(date +%s)
  if [ "$kind" = criterion ]; then
    out=$( cd "$crate" && cargo bench --quiet --bench "$bin" -- --quick --noplot 2>&1 )
  else
    out=$( cd "$crate" && cargo run --release --quiet --bin "$bin" 2>&1 )
  fi
  rc=$?
  dur=$(( $(date +%s) - start ))

  if [ $rc -ne 0 ]; then
    FAILED=1
    RESULTS+=("FAIL|$dir/$bin|${dur}s")
    # A failure is exactly when the output is wanted, so print it even under
    # --summary. (This script used to swallow it, which meant CI reported a red
    # build with no way to tell what broke.)
    echo
    red "── $dir/$bin failed (rc=$rc); last 25 lines:"; echo
    printf '%s\n' "$out" | tail -25
    return
  fi

  # A lane that needs hardware this machine does not have reports SKIP, not
  # PASS: a green tick for a lane that measured nothing is worse than a red one.
  if printf '%s\n' "$out" | grep -q 'skipped —'; then
    RESULTS+=("SKIP|$dir/$bin|${dur}s, needs hardware this machine lacks")
    [ $SUMMARY_ONLY -eq 1 ] || printf '%s\n' "$out"
    return
  fi

  stubs=$(printf '%s\n' "$out" | grep -c 'stub —' || true)
  if [ "$stubs" -gt 0 ]; then
    RESULTS+=("PASS|$dir/$bin|${dur}s, $stubs exercise lane(s) unimplemented")
  else
    RESULTS+=("PASS|$dir/$bin|${dur}s, all lanes implemented")
  fi
  [ $SUMMARY_ONLY -eq 1 ] || printf '%s\n' "$out"
}

for entry in "${BENCHES[@]}"; do
  IFS=: read -r dir bin what <<<"$entry"
  wanted "$dir" || continue
  run_lane "$dir" "$bin" "$what" bin
done

for entry in "${CRITERION[@]}"; do
  IFS=: read -r dir bin what <<<"$entry"
  # criterion lanes are slow: only on --criterion, or when named explicitly
  if [ $RUN_CRITERION -eq 0 ]; then
    wanted "$dir" && [ ${#FILTER[@]} -gt 0 ] || continue
  else
    wanted "$dir" || continue
  fi
  run_lane "$dir" "$bin" "$what" criterion
done

echo
bold "═══ summary"
echo
printf '   %-8s %-40s %s\n' "result" "lane" "notes"
printf '   %-8s %-40s %s\n' "------" "----" "-----"
for r in "${RESULTS[@]}"; do
  IFS='|' read -r status lane note <<<"$r"
  case "$status" in
    PASS) printf '   %-17s %-40s %s\n' "$(green PASS)" "$lane" "$note" ;;
    FAIL) printf '   %-17s %-40s %s\n' "$(red FAIL)" "$lane" "$note" ;;
    SKIP) printf '   %-8s %-40s %s\n' "SKIP" "$lane" "$note" ;;
    *)    printf '   %-8s %-40s %s\n' "$status" "$lane" "$note" ;;
  esac
done
echo
if [ ${#RESULTS[@]} -eq 0 ]; then
  echo "   Nothing matched. ./verify.sh --list shows every lane."
elif [ $FAILED -eq 0 ]; then
  echo "   Every measured lane ran. The numbers above are the ones quoted in the"
  echo "   guides; timings will differ from the recorded ones on other hardware."
  skipped=$(printf '%s\n' "${RESULTS[@]}" | grep -c '^SKIP' || true)
  if [ "$skipped" -gt 0 ]; then
    echo
    echo "   $skipped lane(s) reported SKIP: they need hardware this machine does not"
    echo "   have, so they measured nothing rather than failing. topics/18-gpu is the"
    echo "   only one that can do this, and its reference numbers are in its notes.md."
  fi
else
  echo "   Something failed to run — please open an issue with the output above."
fi
echo
exit $FAILED
