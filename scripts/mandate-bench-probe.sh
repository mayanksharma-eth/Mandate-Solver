#!/usr/bin/env bash
# Follow-up experiments for scripts/mandate-bench.sh.
#
# 1. Probes /healthz from a *separate process* (curl), so health-check latency
#    is not confounded by the load client's own runtime.
# 2. Separates the two costs inside a request: a big payload measures parsing,
#    a small payload against a large base-token config measures path finding.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT=${PORT:-8099}
BASE="http://127.0.0.1:$PORT"
OUT=${OUT:-/tmp/mandate-bench}
BENCH=./target/release/examples/mandate_bench
SERVER=./target/release/solvers

ulimit -n 4096 || true
mkdir -p "$OUT"
: > "$OUT/probe.jsonl"

server_pid=""
stop_server() {
  [ -n "$server_pid" ] || return 0
  kill "$server_pid" 2>/dev/null || true
  wait "$server_pid" 2>/dev/null || true
  server_pid=""
}
trap 'stop_server; kill %1 2>/dev/null || true' EXIT

start_server() { # $1 config entries, $2... extra args
  local entries=$1; shift
  stop_server
  $BENCH --entries "$entries" --print-config > "$OUT/config-$entries.toml"
  $SERVER --addr "127.0.0.1:$PORT" --log=warn "$@" \
    baseline --config "$OUT/config-$entries.toml" > "$OUT/server-probe-$entries.log" 2>&1 &
  server_pid=$!
  for _ in $(seq 100); do
    curl -sf "$BASE/healthz" >/dev/null && return 0
    sleep 0.2
  done
  echo "server did not come up" >&2; exit 1
}

# Out-of-process health probe: one sequential curl at a time, total time in ms.
probe() { # $1 output file
  : > "$1"
  while true; do
    curl -s -o /dev/null -w "%{time_total}\n" "$BASE/healthz" >> "$1" || true
    sleep 0.02
  done
}

scenario() { # $1 label, $2 config-entries, $3 payload-entries, $4 concurrency, $5 requests, $6... bench args
  local label=$1 cfg=$2 entries=$3 concurrency=$4 requests=$5; shift 5
  probe "$OUT/probe-$label.txt" &
  local probe_pid=$!
  $BENCH --url "$BASE" --entries "$entries" --concurrency "$concurrency" \
    --requests "$requests" --label "$label" --healthz-interval-ms 1000 "$@" \
    > "$OUT/raw-probe-$label.json"
  kill "$probe_pid" 2>/dev/null || true
  python3 - "$OUT/raw-probe-$label.json" "$OUT/probe-$label.txt" "$cfg" >> "$OUT/probe.jsonl" <<'PY'
import json, sys
run = json.load(open(sys.argv[1]))
probes = sorted(float(l) * 1e3 for l in open(sys.argv[2]) if l.strip())
def p(q):
    return round(probes[round(q / 100 * (len(probes) - 1))], 3) if probes else None
out = {
    "label": run["label"], "config_base_tokens": (int(sys.argv[3]) - 2) // 2,
    "payload_entries": run["entries"], "concurrency": run["concurrency"],
    "requests": run["requests"], "rps": round(run["rps"], 2),
    "latency": {k: round(v, 2) for k, v in run["latency"].items() if k.endswith("_ms")},
    "statuses": {k: v["count"] for k, v in run["by_status"].items()},
    "errors": run.get("errors", {}),
    "healthz_curl": {"count": len(probes), "p50_ms": p(50), "p95_ms": p(95), "p99_ms": p(99),
                     "max_ms": round(probes[-1], 3) if probes else None},
}
print(json.dumps(out))
PY
  python3 -c "import json,sys; r=json.loads(open(sys.argv[1]).read().strip().split(chr(10))[-1]); print(f\"{r['label']:<26} base_tokens={r['config_base_tokens']:<5} payload={r['payload_entries']:<5} c={r['concurrency']:<4} p50={r['latency']['p50_ms']:9.2f}ms healthz_curl p50={r['healthz_curl']['p50_ms']:7.2f} p99={r['healthz_curl']['p99_ms']:8.2f} max={r['healthz_curl']['max_ms']:8.2f} statuses={r['statuses']} errors={r['errors']}\")" "$OUT/probe.jsonl"
}

# Idle baseline for the out-of-process probe.
start_server 10
scenario idle-probe 10 10 1 0

# Parsing-dominated: huge payload, tiny base-token set (2 base tokens => the
# router only has the direct path to consider).
start_server 10
scenario parse-heavy-c1  10 5000 1  20
scenario parse-heavy-c32 10 5000 32 100

# Pathfinding-dominated: tiny payload, 2499 base tokens => 2500 path candidates.
start_server 5000
scenario route-heavy-c1  5000 10 1  40
scenario route-heavy-c32 5000 10 32 200

# Both, at concurrency 1, with and without route finding.
start_server 5000
scenario full-c1         5000 5000 1 20
scenario full-c1-noroute 5000 5000 1 20 --no-route

# Client concurrency above the limit, with error reasons this time.
start_server 500 --max-concurrent-requests 32
scenario overload-c128 500 500 128 600

echo "probe results: $OUT/probe.jsonl"
