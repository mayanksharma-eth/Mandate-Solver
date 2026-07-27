//! Per-intent solving for Mandate.
//!
//! Mandate settles one user-signed intent at a time rather than a batch. An
//! intent is exact-input sell with a hard floor, routed only through venues the
//! signer allowed. There is no cross-intent matching, no uniform clearing price
//! and no solver fee: the engine picks the best route against a block-pinned
//! liquidity snapshot and reports it. Everything about *executing* that route
//! (calldata, approvals, custody) belongs to the driver, not here.

use {
    crate::{
        boundary,
        domain::{eth, liquidity, order, solver},
    },
    std::collections::HashSet,
};

/// A user-signed Mandate limit intent, reduced to what routing needs.
///
/// `maxSlippageBps` and `deadline` are part of the signed intent but are
/// deliberately absent: slippage is enforced on-chain against the signed
/// `expectedOut`, and expiry is checked at settlement. Neither is a routing
/// input.
#[derive(Debug)]
pub struct Intent {
    /// The exact amount of `sell.token` the signer provides.
    pub sell: eth::Asset,
    /// The token to buy, and the floor (`minBuyAmount`) below which no fill is
    /// acceptable.
    pub buy: eth::Asset,
    /// Pool or router addresses this intent permits routing through.
    pub allowed_venues: HashSet<eth::Address>,
}

/// A block-pinned liquidity snapshot. Routes are only ever computed against
/// tagged liquidity, so a route can be replayed from the same chain state.
#[derive(Debug)]
pub struct Snapshot {
    pub source: String,
    pub block: u64,
    pub liquidity: Vec<liquidity::Liquidity>,
}

/// An abstract route fulfilling an intent, tagged with the snapshot it was
/// computed against. Carries no calldata, executor address or approvals.
#[derive(Debug)]
pub struct Fill {
    pub segments: Vec<Segment>,
    pub expected_out: eth::U256,
    pub gas: eth::Gas,
    pub source: String,
    pub block: u64,
}

/// One hop of a route.
#[derive(Debug)]
pub struct Segment {
    pub liquidity: liquidity::Id,
    pub input: eth::Asset,
    pub output: eth::Asset,
}

/// The pre-sign quote an intent is constructed around. The signer commits
/// `expected_out` into the intent, so it is tagged with the state it was
/// derived from.
#[derive(Debug)]
pub struct Quote {
    pub expected_out: eth::U256,
    pub source: String,
    pub block: u64,
}

/// Routes a single intent against the snapshot, or `None` if no allowed venue
/// can fill it at or above the floor.
pub async fn solve(
    routing: &solver::Routing<'_>,
    intent: &Intent,
    snapshot: &Snapshot,
) -> Option<Fill> {
    // Restricting the liquidity set up front is what makes the allowlist a
    // structural guarantee rather than a post-hoc check: path finding never
    // sees a disallowed venue, so no solution can contain one.
    let liquidity = snapshot
        .liquidity
        .iter()
        .filter(|liquidity| liquidity.is_venue(&intent.allowed_venues))
        .cloned()
        .collect::<Vec<_>>();

    let solver = boundary::baseline::Solver::new(
        routing.weth,
        routing.base_tokens,
        &liquidity,
        routing.uni_v3_quoter_v2.clone(),
    );

    let request = solver::Request {
        sell: intent.sell,
        buy: intent.buy,
        side: order::Side::Sell,
        wrappers: Vec::new(),
    };
    let route = solver.route(request, routing.max_hops).await?;

    let expected_out = route.output().amount;
    if expected_out < intent.buy.amount {
        return None;
    }

    Some(Fill {
        segments: route
            .segments
            .iter()
            .map(|segment| Segment {
                liquidity: segment.liquidity.id.clone(),
                input: segment.input,
                output: segment.output,
            })
            .collect(),
        expected_out,
        gas: route.gas(),
        source: snapshot.source.clone(),
        block: snapshot.block,
    })
}

/// Quotes the output an intent for these parameters would receive.
///
/// Implemented as a floorless [`solve`] so the quote is by construction the
/// same route `/mandate/solve` would pick against the same snapshot.
pub async fn quote(
    routing: &solver::Routing<'_>,
    sell: eth::Asset,
    buy_token: eth::TokenAddress,
    allowed_venues: HashSet<eth::Address>,
    snapshot: &Snapshot,
) -> Option<Quote> {
    let intent = Intent {
        sell,
        buy: eth::Asset {
            token: buy_token,
            amount: eth::U256::ZERO,
        },
        allowed_venues,
    };
    let fill = solve(routing, &intent, snapshot).await?;

    Some(Quote {
        expected_out: fill.expected_out,
        source: fill.source,
        block: fill.block,
    })
}
