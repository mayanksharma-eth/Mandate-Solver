# Mandate solver

Per-intent route selection for `MandateSettlement` limit intents, served by the
baseline solver engine. It picks a route against a block-pinned liquidity
snapshot and returns it. It holds no key, verifies no signature, moves no funds
and submits no transaction — `MandateSettlement` is authoritative for all of
that, and the Foundation driver owns calldata encoding.

## Run

```sh
cargo run -p solvers -- --addr 0.0.0.0:8080 baseline \
  --config configs/local/mandate-base-sepolia.toml
```

Endpoints: `GET /healthz`, `POST /mandate/quote`, `POST /mandate/solve`.

## Frontend flow

Two calls, in this order:

1. `POST /mandate/quote` — before the user signs. Returns `expectedOut` for the
   sell amount; the signed `minBuyAmount` is derived from it. Implemented as a
   floorless solve, so the quote and the fill pick the same route against the
   same snapshot.
2. `POST /mandate/solve` — with the signed intent. Returns the route, or
   `{"solution": null}` when no allowed venue can fill at or above the floor.

Both take `chainId` and `settlementContract` at the top level, checked against
the engine's configured deployment. Both are **required** when the config sets
`mandate-chain-id` / `mandate-settlement`, so an interface pointed at another
deployment gets a `400` rather than a route it could never settle.

```jsonc
// POST /mandate/solve
{
  "chainId": 84532,
  "settlementContract": "0xBcc2C99AE31477bc15309ba34126e3cb607E4117",
  "intent": {
    "signer": "0x…", "nonce": "42",
    "sellToken": "0x…", "buyToken": "0x…",
    "sellAmount": "1000000000000000000", "minBuyAmount": "…",
    "maxSlippageBps": 50, "deadline": 1760000000,
    "allowedVenues": ["0x…"]          // pool or router addresses
  },
  "liquidity": [{
    "kind": "constantProduct",         // also: weightedProduct, stable,
    "id": "tweth-tusd",                //       concentrated, limitOrder
    "address": "0x…", "router": "0x…",
    "gasEstimate": "110000", "fee": "0.003",
    "tokens": { "0xSELL": { "balance": "…" }, "0xBUY": { "balance": "…" } }
  }],
  "liquiditySource": { "name": "my-indexer", "block": 1 }
}
```

`allowedVenues` matches a pool address **or** a router address. Liquidity is
filtered by it before path finding, so no returned route can contain a venue the
signer did not allow. Untagged liquidity (no `liquiditySource`) is rejected: a
route must be replayable from the block it was computed against.

## Configuration

`max-hops` counts **intermediary tokens**, not pools: `0` is direct swaps only,
`1` allows `A -> base -> B` and needs `base-tokens` set. Keep it at `0` until the
Foundation driver can encode multi-hop calldata.

Base Sepolia (84532) is not in the `Chain` enum, so the config sets `weth`
directly instead of `chain-id`.

Set `uni-v3-node-url` to add Uniswap V3 pools, which are priced through the
on-chain QuoterV2 rather than local math.

### Routing guardrails

The engine refuses to start on a config whose routing parameters could produce a
pathological candidate set:

| limit | value | deployed today |
|---|---|---|
| `base-tokens` entries | 128 | 0 (Mandate), 8 (mainnet baseline) |
| `max-hops` | 4 | 0 (Mandate), 2 (mainnet baseline) |
| candidate paths, bounded as `(base-tokens + 1) ^ max-hops` summed over hops | 4096 | 1 (Mandate), 91 (mainnet baseline) |

These are ceilings against obviously broken configuration, not targets. The
measured comfortable regime is far below them — see
[the benchmark](MANDATE-BENCH.md).

## Availability

Route finding runs inline on the Tokio worker handling the request, and for
local (non-RPC) liquidity it completes in a single uninterrupted poll.

**What `--request-timeout` does and does not do.** It bounds request work that
yields: reading the request body, and any liquidity source that goes over the
network. It is **not** a CPU execution deadline. `tower::timeout` only checks its
timer when the inner future yields or completes, so a solve that never awaits
runs to completion and its response is returned late rather than as a 504. This
is measured, not theoretical: with a 200 ms deadline and a deliberately
expensive config, 48 of 48 requests returned 200 at a p50 of 1409 ms, and none
returned 504.

The work does not continue past the response — CPU measured over the 3 s after
such a burst was 0.00 s — so the failure mode is a late answer, not a runaway.

Because the deadline cannot preempt it, the guardrails on `base-tokens` and
`max-hops` above, plus the cost of path finding itself, are what actually
protect availability. `--max-concurrent-requests` bounds how many solves can
hold workers at once, and requests beyond it are shed with a 503 rather than
queued.

The deployed Base Sepolia config (`base-tokens = []`, `max-hops = 0`) sits at the
cheapest point measured: one candidate path per request. Under a
production-shaped config, 5000 liquidity entries at 32-way concurrency solved at
a p50 of 27 ms with `/healthz` p99 of 17 ms.

**Revisit the decision to keep path finding on the runtime** (rather than
isolating it) if any of these change:

- `base-tokens` grows substantially — the cost tracks candidate count, which
  tracks this, not the size of the liquidity payload.
- multi-hop routing is enabled or `max-hops` is raised, which multiplies
  candidates rather than adding to them.
- `/healthz` tail latency degrades materially under normal load. It sits outside
  the Mandate concurrency limiter, so it is the cheapest signal that solves are
  holding workers: at 2500 candidate paths, a **single** in-flight solve pushed
  its p99 to 516 ms on an otherwise idle 14-core machine.

## Tests

```sh
cargo test -p solvers mandate     # engine-level, in-process HTTP
./scripts/mandate-e2e.sh          # real binary, real config, real requests
./scripts/mandate-bench.sh        # load/latency matrix, see docs/MANDATE-BENCH.md
```
