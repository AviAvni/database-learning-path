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
#   ./verify.sh 40 41        only these topics
#
# Requires: a Rust toolchain (rustup.rs). First run compiles from scratch and
# takes a few minutes; later runs are cached.
#
# A note on what you will see: each benchmark has a first "lane" that is
# implemented and runs today — those are the numbers quoted in the guides. The
# later lanes are the reader's exercises and print "[stub — implement the
# todo!()s ...]" until you do them. That is intended, not a broken build.

set -uo pipefail
cd "$(dirname "$0")"

SUMMARY_ONLY=0
FILTER=()
for arg in "$@"; do
  case "$arg" in
    --summary) SUMMARY_ONLY=1 ;;
    -h|--help) sed -n '3,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) FILTER+=("$arg") ;;
  esac
done

# topic dir : bench binary : what it measures
BENCHES=(
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
)

bold() { printf '\033[1m%s\033[0m\n' "$1"; }
green() { printf '\033[32m%s\033[0m' "$1"; }
red() { printf '\033[31m%s\033[0m' "$1"; }

command -v cargo >/dev/null || { red "cargo not found"; echo " — install Rust from https://rustup.rs"; exit 1; }

declare -a RESULTS=()
FAILED=0

for entry in "${BENCHES[@]}"; do
  IFS=: read -r dir bin what <<<"$entry"

  if [ ${#FILTER[@]} -gt 0 ]; then
    match=0
    for f in "${FILTER[@]}"; do [[ "$dir" == *"$f"* ]] && match=1; done
    [ $match -eq 1 ] || continue
  fi

  crate="topics/$dir/experiments"
  [ -d "$crate" ] || { RESULTS+=("SKIP|$dir|missing"); continue; }

  [ $SUMMARY_ONLY -eq 1 ] || { echo; bold "═══ topics/$dir — $what"; echo; }

  start=$(date +%s)
  out=$( cd "$crate" && cargo run --release --quiet --bin "$bin" 2>&1 )
  rc=$?
  dur=$(( $(date +%s) - start ))

  if [ $rc -ne 0 ]; then
    FAILED=1
    RESULTS+=("FAIL|$dir|${dur}s")
    [ $SUMMARY_ONLY -eq 1 ] || { echo "$out" | tail -20; }
    continue
  fi

  stubs=$(printf '%s\n' "$out" | grep -c 'stub —' || true)
  RESULTS+=("PASS|$dir|${dur}s, $stubs exercise lane(s) unimplemented")
  [ $SUMMARY_ONLY -eq 1 ] || printf '%s\n' "$out"
done

echo
bold "═══ summary"
echo
printf '   %-8s %-28s %s\n' "result" "topic" "notes"
printf '   %-8s %-28s %s\n' "------" "-----" "-----"
for r in "${RESULTS[@]}"; do
  IFS='|' read -r status dir note <<<"$r"
  case "$status" in
    PASS) printf '   %-17s %-28s %s\n' "$(green PASS)" "$dir" "$note" ;;
    FAIL) printf '   %-17s %-28s %s\n' "$(red FAIL)" "$dir" "$note" ;;
    *)    printf '   %-8s %-28s %s\n' "$status" "$dir" "$note" ;;
  esac
done
echo
if [ $FAILED -eq 0 ]; then
  echo "   Every measured lane ran. The numbers above are the ones quoted in the"
  echo "   guides; timings will differ from the recorded ones on other hardware."
else
  echo "   Something failed to run — please open an issue with the output above."
fi
echo
exit $FAILED
