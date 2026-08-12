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
- evaluates direct and bounded multi-hop paths deterministically; the Base
  Sepolia alpha is limited to one hop until the Foundation driver encodes and
  simulates multi-hop calldata;
- returns the best route at or above `minBuyAmount` as an abstract execution
  plan.

Foundation remains authoritative for EIP-712 signature checks, nonce use,
global router allowlisting, token transfers, approvals, and atomic settlement.

## Run

```sh
cargo run -p mandate-solver
```

By default it targets the Base Sepolia alpha deployment: chain `84532`, proxy
`0xBcc2C99AE31477bc15309ba34126e3cb607E4117`, and the configured mock UniV2
router. Override the chain and settlement address only for another reviewed
deployment.

The HTTP surface is deliberately small:

- `GET /healthz`
- `POST /v1/solve`

`/v1/solve` returns an abstract execution plan. It does not execute a trade.

## Quality gate

```sh
cargo fmt --package mandate-solver -- --check
cargo clippy --package mandate-solver --all-targets -- -D warnings
cargo test --package mandate-solver
cargo build --package mandate-solver --release
```
