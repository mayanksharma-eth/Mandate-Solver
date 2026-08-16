#!/usr/bin/env bash
# End-to-end check of the Mandate endpoints against the Base Sepolia config.
#
# Drives the same two-call flow the Mandate frontend uses: /mandate/quote before
# the user signs, then /mandate/solve with the signed intent.
set -euo pipefail

cd "$(dirname "$0")/.."

PORT=${PORT:-8090}
BASE="http://127.0.0.1:$PORT"
CONFIG=configs/local/mandate-base-sepolia.toml
ROUTER="0x7893A11C3572129BBdDaCE266b87A7A9b23871d2" # mock UniV2 router
POOL="0x0000000000000000000000000000000000000aaa"
TWETH="0x40ca30bee6Ca16E67BDc8f39F83e6637f5B623F2"
TUSD="0xd88828Fa434B791324856c302d8Ab856E2b6d57C"
CHAIN_ID=84532
SETTLEMENT="0xBcc2C99AE31477bc15309ba34126e3cb607E4117"
SELL_AMOUNT="1000000000000000000" # 1 tWETH
# Wei-exact constant-product output for SELL_AMOUNT against the pool below:
# 997 * x * Y / (1000 * X + 997 * x).
EXPECTED_OUT="2988020943119709649479"
DEADLINE=$(($(date +%s) + 600))
fail=0

command -v jq >/dev/null || { echo "jq required"; exit 1; }
! nc -z 127.0.0.1 "$PORT" 2>/dev/null || { echo "port $PORT already in use"; exit 1; }

cargo build -p solvers --bin solvers
./target/debug/solvers --addr "127.0.0.1:$PORT" baseline --config "$CONFIG" \
  >/tmp/mandate-e2e-server.log 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true' EXIT

for _ in $(seq 50); do
  curl -sf "$BASE/healthz" >/dev/null && break
  sleep 0.2
done

pool() { # $1 router
  cat <<JSON
[{
  "kind": "constantProduct",
  "id": "tweth-tusd",
  "address": "$POOL",
  "router": "$1",
  "gasEstimate": "110000",
  "fee": "0.003",
  "tokens": {
    "$TWETH": { "balance": "1000000000000000000000" },
    "$TUSD": { "balance": "3000000000000000000000000" }
  }
}]
JSON
}

source() { echo '{ "name": "e2e", "block": 1 }'; }

quote_request() { # $1 allowedVenues, $2 chainId (default CHAIN_ID)
  cat <<JSON
{
  "chainId": ${2:-$CHAIN_ID},
  "settlementContract": "$SETTLEMENT",
  "sellToken": "$TWETH",
  "buyToken": "$TUSD",
  "sellAmount": "$SELL_AMOUNT",
  "allowedVenues": $1,
  "liquidity": $(pool "$ROUTER"),
  "liquiditySource": $(source)
}
JSON
}

solve_request() { # $1 minBuyAmount, $2 allowedVenues, $3 pool router, $4 chainId
  cat <<JSON
{
  "chainId": ${4:-$CHAIN_ID},
  "settlementContract": "$SETTLEMENT",
  "intent": {
    "signer": "0x1111111111111111111111111111111111111111",
    "nonce": "42",
    "sellToken": "$TWETH",
    "buyToken": "$TUSD",
    "sellAmount": "$SELL_AMOUNT",
    "minBuyAmount": "$1",
    "maxSlippageBps": 50,
    "deadline": $DEADLINE,
    "allowedVenues": $2
  },
  "liquidity": $(pool "$3"),
  "liquiditySource": $(source)
}
JSON
}

# $1 name, $2 path, $3 body, $4 jq assertion or "status:<code>"
check() {
  local name=$1 status
  status=$(curl -s -o /tmp/mandate-e2e-body -w '%{http_code}' \
    -H 'content-type: application/json' -d "$3" "$BASE/$2")
  if [[ $4 == status:* ]]; then
    [[ $status == "${4#status:}" ]] && { echo "ok   $name"; return; }
  elif [[ $status == 200 ]] && jq -e "$4" </tmp/mandate-e2e-body >/dev/null; then
    echo "ok   $name"; return
  fi
  echo "FAIL $name (http $status): $(cat /tmp/mandate-e2e-body)"
  fail=1
}

curl -sf "$BASE/healthz" >/dev/null && echo "ok   healthz" || { echo "FAIL healthz"; fail=1; }

# The frontend quotes before the user signs, so the quote must be present and
# wei-exact: the signed minBuyAmount is derived from it.
check "quote returns a wei-exact expectedOut" mandate/quote \
  "$(quote_request "[\"$ROUTER\"]")" \
  ".quote.expectedOut == \"$EXPECTED_OUT\" and .quote.block == 1"

# The same snapshot must fill at the quoted price, or the quote was a lie.
check "solve fills at the quoted price" mandate/solve \
  "$(solve_request "$EXPECTED_OUT" "[\"$ROUTER\"]" "$ROUTER")" \
  ".solution.expectedOut == \"$EXPECTED_OUT\" and .solution.route[0].liquidity == \"tweth-tusd\""

check "floor above the best route is not filled" mandate/solve \
  "$(solve_request "9999999999999999999999999" "[\"$ROUTER\"]" "$ROUTER")" \
  '.solution == null'

check "venue outside the allowlist is never routed" mandate/solve \
  "$(solve_request 1 '["0x000000000000000000000000000000000000bad0"]' "$ROUTER")" \
  '.solution == null'

check "pool address matches the allowlist too" mandate/solve \
  "$(solve_request 1 "[\"$POOL\"]" "$ROUTER")" \
  '.solution.route[0].liquidity == "tweth-tusd"'

check "empty allowlist is rejected" mandate/solve \
  "$(solve_request 1 '[]' "$ROUTER")" 'status:400'

check "expired intent is rejected" mandate/solve \
  "$(solve_request 1 "[\"$ROUTER\"]" "$ROUTER" | sed "s/\"deadline\": $DEADLINE/\"deadline\": 1/")" \
  'status:400'

# An interface pointed at another deployment must not receive a route it could
# never settle.
check "wrong chain is rejected" mandate/solve \
  "$(solve_request 1 "[\"$ROUTER\"]" "$ROUTER" 1)" 'status:400'

check "wrong settlement contract is rejected" mandate/solve \
  "$(solve_request 1 "[\"$ROUTER\"]" "$ROUTER" | sed "s/$SETTLEMENT/0x000000000000000000000000000000000000dead/")" \
  'status:400'

check "quote checks the deployment too" mandate/quote \
  "$(quote_request "[\"$ROUTER\"]" 1)" 'status:400'

exit $fail
