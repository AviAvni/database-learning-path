#!/usr/bin/env bash
# Reproduce capstone/BASELINES.md. Usage:
#   C_MODULE=/path/falkordb.so RS_MODULE=/path/libfalkordb.dylib PY=/path/venv/bin/python ./run.sh
# The venv needs: FalkorDB==1.8.1 pytest==9.1.1 pytest-benchmark==5.3.0 redis==8.1.0
# Engines are run one after the other, never concurrently, on an otherwise idle machine.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PY="${PY:-python3}"
VENV_BIN="$(dirname "$PY")"
OUT="${OUT:-$HERE}"
LOG="${LOG:-$(mktemp -d)}"

bench() {  # $1 = label, $2 = module
  echo "$1 start $(date -u +%FT%TZ) loadavg $(sysctl -n vm.loadavg)" >> "$OUT/load.log"
  (cd "$LOG" && FALKORDB_MODULE="$2" BENCH_LOGDIR="$LOG" BENCH_ROUNDS=100 \
    "$VENV_BIN/pytest" -q -p no:cacheprovider "$HERE/test_bench.py" \
      --benchmark-min-rounds=100 --benchmark-save-data \
      --benchmark-json="$OUT/bench-$1.json")
  "$PY" "$HERE/mem.py" "$2" "$1" "$OUT/mem-$1.json"
  echo "$1 end   $(date -u +%FT%TZ) loadavg $(sysctl -n vm.loadavg)" >> "$OUT/load.log"
}

bench c-v4.22.2  "$C_MODULE"
bench rs-v6.0.3  "$RS_MODULE"
"$PY" "$HERE/summarize.py" "$OUT"/bench-c-v4.22.2.json "$OUT"/bench-rs-v6.0.3.json
