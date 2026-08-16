#!/usr/bin/env bash
# Load/latency benchmark for POST /mandate/solve.
#
# Starts a release `solvers` binary per liquidity size (base tokens have to
# match the fixture), runs the scenario matrix, and writes one JSON object per
# scenario to results.jsonl. Also records process CPU time and the Mandate
# Prometheus counters around each scenario.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT=${PORT:-8099}
BASE="http://127.0.0.1:$PORT"
OUT=${OUT:-/tmp/mandate-bench}
BENCH=./target/release/examples/mandate_bench
SERVER=./target/release/solvers
# Concurrency limit under test. The production default is 32; scenarios that
# need a different value pass it explicitly.
LIMIT=${LIMIT:-32}

ulimit -n 4096 || true
mkdir -p "$OUT"
: > "$OUT/results.jsonl"

cargo build --release -p solvers --bin solvers --example mandate_bench

server_pid=""
stop_server() {
  [ -n "$server_pid" ] || return 0
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
  server_pid=""
}
trap stop_server EXIT

start_server() { # $1 entries, $2... extra server args
  local entries=$1; shift
  stop_server
  # BASE_TOKENS pins the config's base-token set instead of letting it grow with
  # the fixture, which models a production config.
  $BENCH --entries "$entries" ${BASE_TOKENS:+--base-tokens "$BASE_TOKENS"} --print-config \
    > "$OUT/config-$entries.toml"
  $SERVER --addr "127.0.0.1:$PORT" --log=warn --max-concurrent-requests "$LIMIT" "$@" \
    baseline --config "$OUT/config-$entries.toml" > "$OUT/server-$entries.log" 2>&1 &
  server_pid=$!
  for _ in $(seq 100); do
    curl -sf "$BASE/healthz" >/dev/null && return 0
    sleep 0.2
  done
  echo "server did not come up" >&2
  exit 1
}

# Cumulative process CPU seconds, from ps TIME (e.g. "1:23.45").
cpu_seconds() {
  ps -p "$server_pid" -o time= | awk -F'[:.]' '{print $1*60 + $2 + $3/100}'
}

mandate_counters() {
  curl -s "$BASE/metrics" | grep '^solver_engine_mandate_requests_total' || true
}

scenario() { # $1 label, $2 entries, $3 concurrency, $4 requests, $5... extra bench args
  local label=$1 entries=$2 concurrency=$3 requests=$4; shift 4
  local cpu_before cpu_after wall_before wall_after
  mandate_counters > "$OUT/metrics-$label.before"
  cpu_before=$(cpu_seconds); wall_before=$(date +%s.%N)
  $BENCH --url "$BASE" --entries "$entries" --concurrency "$concurrency" \
    --requests "$requests" --label "$label" "$@" > "$OUT/raw-$label.json"
  cpu_after=$(cpu_seconds); wall_after=$(date +%s.%N)
  mandate_counters > "$OUT/metrics-$label.after"
  python3 - "$OUT/raw-$label.json" "$cpu_before" "$cpu_after" "$wall_before" "$wall_after" \
    >> "$OUT/results.jsonl" <<'PY'
import json, sys
run = json.load(open(sys.argv[1]))
cpu = float(sys.argv[3]) - float(sys.argv[2])
wall = float(sys.argv[5]) - float(sys.argv[4])
run["server_cpu_secs"] = round(cpu, 2)
run["server_cpu_cores"] = round(cpu / wall, 2) if wall > 0 else 0
print(json.dumps(run))
PY
  python3 -c "import json,sys; r=json.load(open(sys.argv[1])); print(f\"{r['label']:<22} entries={r['entries']:<5} c={r['concurrency']:<4} n={r['requests']:<5} rps={r['rps']:.1f} p50={r['latency']['p50_ms']:.1f}ms p99={r['latency']['p99_ms']:.1f}ms healthz_p99={r['healthz']['p99_ms']:.2f}ms statuses={ {k: v['count'] for k,v in r['by_status'].items()} }\")" "$OUT/raw-$label.json"
}

# --- idle baseline -----------------------------------------------------------
start_server 10
scenario idle 10 1 0

# --- matrix ------------------------------------------------------------------
scenario e10-c1    10 1  2000
scenario e10-c32   10 32 4000

start_server 100
scenario e100-c1   100 1  1000
scenario e100-c32  100 32 2000

start_server 500
scenario e500-c32  500 32 600
# Same payload, allowlist matching nothing: everything but route finding.
scenario e500-c32-noroute 500 32 600 --no-route
# Client concurrency above the solver's limit of 32.
scenario e500-c128 500 128 600

start_server 1000
scenario e1000-c32 1000 32 300

start_server 5000
scenario e5000-c32 5000 32 100
scenario e5000-c32-noroute 5000 32 100 --no-route

# --- timeout experiment ------------------------------------------------------
# Expensive payload, deliberately short deadline. Does the 504 arrive near the
# deadline, and does the server keep burning CPU after it?
start_server 5000 --request-timeout 200ms
scenario e5000-timeout 5000 8 48
cpu_idle_start=$(cpu_seconds)
sleep 3
cpu_idle_end=$(cpu_seconds)
echo "{\"label\":\"post-timeout-idle\",\"cpu_secs_over_3s_idle\":$(python3 -c "print(round($cpu_idle_end - $cpu_idle_start, 2))")}" \
  >> "$OUT/results.jsonl"
# Lightweight probe after the timed-out burst.
$BENCH --url "$BASE" --entries 10 --concurrency 1 --requests 50 --label after-timeout \
  > "$OUT/raw-after-timeout.json"
cat "$OUT/raw-after-timeout.json" >> "$OUT/results.jsonl"

echo "results: $OUT/results.jsonl"
