# solvers

The CoW Protocol solver engine. Serves route-finding over on-chain liquidity.

```
cargo run -p solvers -- baseline --config config/example.baseline.toml
```

Two request models are served by the same binary:

| Endpoint | Unit of work | Consumer |
| --- | --- | --- |
| `POST /solve` | a CoW batch auction | CoW driver |
| `POST /mandate/solve` | one Mandate `LimitIntent` | Mandate driver-shim |
| `POST /mandate/quote` | pre-sign quote for one intent | Mandate intent construction |

## Mandate mode

Mandate settles one user-signed intent at a time rather than a batch. The
routing algorithms and liquidity model are shared with `/solve`; only the input
shape and the objective differ. `/solve` is untouched by Mandate mode.

An intent is exact-input sell with a hard floor:

```jsonc
{
  "intent": {
    "signer": "0x...",                 // Foundation intent signer
    "nonce": "42",                     // Foundation replay-protection nonce
    "sellToken": "0x...",
    "buyToken": "0x...",
    "sellAmount": "133700000000000000",   // exact
    "minBuyAmount": "6000000000000000000000",  // hard floor
    "maxSlippageBps": 50,
    "deadline": 1767225600,             // Unix seconds
    "allowedVenues": ["0x..."]
  },
  "liquidity": [ /* same liquidity objects as /solve */ ],
  "liquiditySource": { "name": "my-indexer", "block": 21000000 }
}
```

How it differs from the batch objective:

- **Per-intent, independent pricing.** No uniform clearing price across orders,
  no ring trades, no order-to-order matching. Each intent is routed on its own
  against liquidity, maximizing output.
- **No solver fee.** The batch path charges limit orders a gas-priced surplus
  fee and internalizes eligible interactions against settlement-contract
  buffers. Mandate has no such custody and reports **gross** route output, so
  `expectedOut` is reproducible from pool math alone.
- **Allowlisted venues.** Liquidity is intersected with `allowedVenues` (pool
  address or router) *before* path finding, so a disallowed venue cannot appear
  in a route by construction. If no allowed venue can fill at or above the
  floor, the response is `{"solution": null}` — never a route that would revert
  on-chain.
- **Block-pinned liquidity.** `liquiditySource` is required; there is no
  solving against untagged liquidity. Every solution echoes `liquiditySource`
  and `block` so a route is replayable from the same chain state.
- **Preflight rejection.** The solver rejects expired intents and zero
  signer/token/venue addresses, equal sell and buy tokens, zero amounts,
  slippage above 10,000 bps, and empty venue allowlists. These checks avoid
  producing routes Foundation would reject. Foundation remains authoritative
  for signature validation, nonce consumption, token movement, and settlement.

`/mandate/quote` takes `sellToken`, `buyToken`, `sellAmount`, `allowedVenues`
plus the same snapshot, and returns `{ expectedOut, block, liquiditySource }`.
It is implemented as a floorless solve, so it is by construction the same route
`/mandate/solve` picks against the same snapshot. Mandate signs `expectedOut`
into the intent; slippage is then enforced on-chain against that signed
reference. The engine never weakens the signed `minBuyAmount` floor or invents
a separate output guarantee.

### Running the focused checks

```sh
cargo +nightly fmt --check
cargo test -p solvers mandate
```

The Mandate suite covers deterministic best-route selection, allowlisted venue
filtering, hard-floor enforcement, block-pinned liquidity, and preflight
rejection of structurally invalid intents.

## Engine / driver boundary

The engine is a **pure route optimizer**. `Solution` and `Interaction` describe
an *abstract* route: liquidity ids and input/output assets. The engine knows
nothing about `MandateSettlement`, EIP-712 hashing, calldata, recipients,
approvals or token custody.

Turning an abstract route into an executable call — encoding calldata,
parameterizing the executor, setting approvals, moving tokens — is the Mandate
driver-shim's job. A change that requires the engine to know the executor's
address or emit `target.call(data)` belongs on the other side of this line.
