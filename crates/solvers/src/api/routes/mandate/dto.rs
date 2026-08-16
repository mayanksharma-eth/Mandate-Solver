//! Wire types for the Mandate per-intent endpoints.

use {
    crate::{
        api::routes::{Error, solve::dto::auction::liquidity_to_domain},
        domain::{eth, mandate, solver},
    },
    alloy::primitives::{Address, U256},
    itertools::Itertools,
    number::serialization::HexOrDecimalU256,
    serde::{Deserialize, Serialize},
    serde_with::serde_as,
    solvers_dto::auction::Liquidity,
};

/// A block-pinned liquidity snapshot. Solving against untagged liquidity is
/// rejected, so both fields are required.
#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiquiditySource {
    /// Where the snapshot came from, e.g. an indexer or node identifier.
    pub name: String,
    /// The block the snapshot reflects.
    pub block: u64,
}

/// A Mandate limit intent: exact-input sell with a hard floor.
#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitIntent {
    /// Wallet that signed the Foundation `LimitIntent`.
    pub signer: Address,
    /// The Foundation replay-protection nonce. The solver preserves it for the
    /// driver handoff but never assigns or consumes nonces itself.
    #[serde_as(as = "HexOrDecimalU256")]
    pub nonce: U256,
    pub sell_token: Address,
    pub buy_token: Address,
    #[serde_as(as = "HexOrDecimalU256")]
    pub sell_amount: U256,
    #[serde_as(as = "HexOrDecimalU256")]
    pub min_buy_amount: U256,
    /// Accepted as part of the signed intent but unused by the engine:
    /// slippage is enforced on-chain against the signed `expectedOut`.
    pub max_slippage_bps: u16,
    /// Unix timestamp, matching `MandateSettlement.LimitIntent.deadline`.
    pub deadline: u64,
    /// Pool or router addresses this intent permits routing through.
    pub allowed_venues: Vec<Address>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolveRequest {
    pub intent: LimitIntent,
    pub liquidity: Vec<Liquidity>,
    pub liquidity_source: LiquiditySource,
    #[serde(flatten)]
    pub deployment: Deployment,
}

/// Which Mandate deployment the caller believes it is talking to. Checked
/// against the engine's configured deployment, when it has one.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub chain_id: Option<u64>,
    pub settlement_contract: Option<Address>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteRequest {
    pub sell_token: Address,
    pub buy_token: Address,
    #[serde_as(as = "HexOrDecimalU256")]
    pub sell_amount: U256,
    pub allowed_venues: Vec<Address>,
    pub liquidity: Vec<Liquidity>,
    pub liquidity_source: LiquiditySource,
    #[serde(flatten)]
    pub deployment: Deployment,
}

/// An abstract route. No calldata, no executor address: turning this into an
/// executable call is the Mandate driver's job.
#[serde_as]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Solution {
    #[serde_as(as = "HexOrDecimalU256")]
    pub expected_out: U256,
    /// Echo of the snapshot this route was computed against.
    pub liquidity_source: String,
    pub block: u64,
    pub gas: u64,
    pub route: Vec<Segment>,
}

#[serde_as]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub liquidity: String,
    pub input_token: Address,
    #[serde_as(as = "HexOrDecimalU256")]
    pub input_amount: U256,
    pub output_token: Address,
    #[serde_as(as = "HexOrDecimalU256")]
    pub output_amount: U256,
}

/// `null` when no allowed venue can fill the intent at or above its floor.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolveResponse {
    pub solution: Option<Solution>,
}

#[serde_as]
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Quote {
    #[serde_as(as = "HexOrDecimalU256")]
    pub expected_out: U256,
    pub block: u64,
    pub liquidity_source: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteResponse {
    pub quote: Option<Quote>,
}

pub fn snapshot_to_domain(
    liquidity: &[Liquidity],
    source: LiquiditySource,
) -> Result<mandate::Snapshot, Error> {
    Ok(mandate::Snapshot {
        source: source.name,
        block: source.block,
        liquidity: liquidity.iter().map(liquidity_to_domain).try_collect()?,
    })
}

pub fn intent_to_domain(intent: &LimitIntent) -> mandate::Intent {
    mandate::Intent {
        sell: eth::Asset {
            token: eth::TokenAddress(intent.sell_token),
            amount: intent.sell_amount,
        },
        buy: eth::Asset {
            token: eth::TokenAddress(intent.buy_token),
            amount: intent.min_buy_amount,
        },
        allowed_venues: intent.allowed_venues.iter().copied().collect(),
    }
}

/// Reject malformed or already-expired data before route finding. The
/// settlement contract remains authoritative for signature, nonce and balance
/// checks; these checks avoid returning a route the contract must reject.
pub fn validate_intent(intent: &LimitIntent) -> Result<(), Error> {
    if intent.signer.is_zero() {
        return Err("signer must not be zero".into());
    }
    if intent.sell_token.is_zero() || intent.buy_token.is_zero() {
        return Err("token must not be zero".into());
    }
    if intent.sell_token == intent.buy_token {
        return Err("sell and buy token must differ".into());
    }
    if intent.sell_amount.is_zero() || intent.min_buy_amount.is_zero() {
        return Err("amounts must be nonzero".into());
    }
    if intent.deadline <= chrono::Utc::now().timestamp().unsigned_abs() {
        return Err("intent deadline has expired".into());
    }
    if intent.max_slippage_bps > 10_000 {
        return Err("max slippage must not exceed 10000 bps".into());
    }
    if intent.allowed_venues.is_empty() || intent.allowed_venues.contains(&Address::ZERO) {
        return Err("allowed venues must be nonempty and nonzero".into());
    }
    Ok(())
}

/// Reject a request aimed at a different Mandate deployment. A route computed
/// here could never settle there, and silently returning one lets an interface
/// pointed at the wrong contract look healthy.
pub fn validate_deployment(
    request: &Deployment,
    configured: &solver::MandateDeployment,
) -> Result<(), Error> {
    if let Some(chain_id) = configured.chain_id
        && request.chain_id != Some(chain_id)
    {
        return Err("request chain does not match the configured Mandate deployment".into());
    }
    if let Some(settlement) = configured.settlement
        && request.settlement_contract != Some(settlement)
    {
        return Err(
            "request settlement contract does not match the configured Mandate deployment".into(),
        );
    }
    Ok(())
}

pub fn validate_quote(request: &QuoteRequest) -> Result<(), Error> {
    if request.sell_token.is_zero() || request.buy_token.is_zero() {
        return Err("token must not be zero".into());
    }
    if request.sell_token == request.buy_token {
        return Err("sell and buy token must differ".into());
    }
    if request.sell_amount.is_zero() {
        return Err("sell amount must be nonzero".into());
    }
    if request.allowed_venues.is_empty() || request.allowed_venues.contains(&Address::ZERO) {
        return Err("allowed venues must be nonempty and nonzero".into());
    }
    Ok(())
}

impl From<mandate::Fill> for Solution {
    fn from(fill: mandate::Fill) -> Self {
        Self {
            expected_out: fill.expected_out,
            liquidity_source: fill.source,
            block: fill.block,
            gas: u64::try_from(fill.gas.0).unwrap_or(u64::MAX),
            route: fill
                .segments
                .into_iter()
                .map(|segment| Segment {
                    liquidity: segment.liquidity.0,
                    input_token: segment.input.token.0,
                    input_amount: segment.input.amount,
                    output_token: segment.output.token.0,
                    output_amount: segment.output.amount,
                })
                .collect(),
        }
    }
}

impl From<mandate::Quote> for Quote {
    fn from(quote: mandate::Quote) -> Self {
        Self {
            expected_out: quote.expected_out,
            block: quote.block,
            liquidity_source: quote.source,
        }
    }
}
