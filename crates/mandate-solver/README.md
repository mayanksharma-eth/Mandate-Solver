# Mandate Foundation Solver

A small, non-custodial solver for `MandateSettlement` limit intents. It is
inspired by CoW's route selection, but is not a CoW service: it has no
database, orderbook, autopilot, driver, wallet, private key, approval logic, or
transaction submission.

## What it does

- accepts the Foundation `LimitIntent` payload plus a block-pinned constant
  product liquidity snapshot;
- checks the configured chain ID and settlement contract address;
- rejects expired or malformed intents before routing;
- filters pools by the user-signed venue allowlist;
- evaluates direct and bounded multi-hop paths deterministically;
- returns the best route at or above `minBuyAmount` as an abstract execution
  plan.

Foundation remains authoritative for EIP-712 signature checks, nonce use,
global router allowlisting, token transfers, approvals, and atomic settlement.

## Run

```sh
export MANDATE_CHAIN_ID=11155111
export MANDATE_SETTLEMENT_ADDRESS=0xYourSepoliaSettlementAddress
cargo run -p mandate-solver
```

The HTTP surface is deliberately small:

- `GET /healthz`
- `POST /v1/solve`

`/v1/solve` returns an abstract execution plan. It does not execute a trade.

## Quality gate

```sh
cargo +nightly fmt --package mandate-solver -- --check
cargo +nightly clippy --package mandate-solver --all-targets -- -D warnings
cargo +nightly test --package mandate-solver
cargo +nightly build --package mandate-solver --release
```
