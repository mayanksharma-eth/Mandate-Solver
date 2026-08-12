//! A deliberately small, non-custodial solver for Mandate Foundation.
//!
//! It validates a signed-intent *payload*, filters liquidity by the user's
//! allowed venues, compares deterministic constant-product routes, and returns
//! an abstract execution plan. It never holds a key, verifies a signature,
//! moves funds, or submits a transaction. `MandateSettlement` is authoritative
//! for those operations.

use {
    alloy_primitives::{Address, U256, address},
    serde::{Deserialize, Serialize},
    std::collections::{BTreeSet, HashSet},
    thiserror::Error,
};

pub const BASE_SEPOLIA_CHAIN_ID: u64 = 84_532;
pub const BASE_SEPOLIA_SETTLEMENT_PROXY: Address =
    address!("Bcc2C99AE31477bc15309ba34126e3cb607E4117");
pub const BASE_SEPOLIA_MOCK_UNIV2_ROUTER: Address =
    address!("7893A11C3572129BBdDaCE266b87A7A9b23871d2");
pub const BASE_SEPOLIA_TWETH: Address = address!("40ca30bee6Ca16E67BDc8f39F83e6637f5B623F2");
pub const BASE_SEPOLIA_TUSD: Address = address!("d88828Fa434B791324856c302d8Ab856E2b6d57C");

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundationIntent {
    pub signer: Address,
    #[serde(with = "uint")]
    pub nonce: U256,
    pub deadline: u64,
    pub max_slippage_bps: u16,
    pub allowed_venues: Vec<Address>,
    pub sell_token: Address,
    pub buy_token: Address,
    #[serde(with = "uint")]
    pub sell_amount: U256,
    #[serde(with = "uint")]
    pub min_buy_amount: U256,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SolverConfig {
    pub chain_id: u64,
    pub settlement_contract: Address,
    pub enabled_routers: HashSet<Address>,
    pub max_hops: usize,
}

impl SolverConfig {
    /// Base Sepolia alpha deployment from Mandate Foundation PR #9. One hop is
    /// deliberate until the Foundation driver can encode and simulate multi-hop
    /// calldata.
    pub fn base_sepolia_alpha() -> Self {
        Self {
            chain_id: BASE_SEPOLIA_CHAIN_ID,
            settlement_contract: BASE_SEPOLIA_SETTLEMENT_PROXY,
            enabled_routers: HashSet::from([BASE_SEPOLIA_MOCK_UNIV2_ROUTER]),
            max_hops: 1,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub source: String,
    pub block: u64,
    pub pools: Vec<ConstantProductPool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstantProductPool {
    pub id: String,
    pub address: Address,
    pub router: Address,
    pub token0: Address,
    pub token1: Address,
    #[serde(with = "uint")]
    pub reserve0: U256,
    #[serde(with = "uint")]
    pub reserve1: U256,
    /// Fee in basis points, e.g. 30 for a 0.3% pool.
    pub fee_bps: u16,
    pub gas: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SolveRequest {
    pub chain_id: u64,
    pub settlement_contract: Address,
    pub now: u64,
    pub intent: FoundationIntent,
    pub liquidity: Snapshot,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPlan {
    #[serde(with = "uint")]
    pub expected_out: U256,
    pub router: Address,
    pub block: u64,
    pub liquidity_source: String,
    pub gas: u64,
    pub route: Vec<RouteHop>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RouteHop {
    pub pool_id: String,
    pub router: Address,
    pub token_in: Address,
    pub token_out: Address,
    #[serde(with = "uint")]
    pub amount_in: U256,
    #[serde(with = "uint")]
    pub amount_out: U256,
    pub gas: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SolveError {
    #[error("request chain does not match solver configuration")]
    ChainMismatch,
    #[error("request settlement contract does not match solver configuration")]
    SettlementMismatch,
    #[error("intent is invalid: {0}")]
    InvalidIntent(&'static str),
}

pub fn solve(
    config: &SolverConfig,
    request: SolveRequest,
) -> Result<Option<ExecutionPlan>, SolveError> {
    if request.chain_id != config.chain_id {
        return Err(SolveError::ChainMismatch);
    }
    if request.settlement_contract != config.settlement_contract {
        return Err(SolveError::SettlementMismatch);
    }
    validate(&request.intent, request.now)?;

    let allowed = request
        .intent
        .allowed_venues
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let pools = request
        .liquidity
        .pools
        .into_iter()
        .filter(|pool| {
            config.enabled_routers.contains(&pool.router)
                && (allowed.contains(&pool.address) || allowed.contains(&pool.router))
        })
        .collect::<Vec<_>>();

    let mut candidates = Vec::new();
    visit(
        request.intent.sell_token,
        request.intent.sell_amount,
        request.intent.buy_token,
        config.max_hops.max(1),
        &pools,
        &mut BTreeSet::new(),
        Vec::new(),
        &mut candidates,
    );
    let best = candidates
        .into_iter()
        .filter(|plan| plan.expected_out >= request.intent.min_buy_amount)
        .max_by(|a, b| {
            a.expected_out
                .cmp(&b.expected_out)
                .then_with(|| b.route_key().cmp(&a.route_key()))
        });
    Ok(best.map(|candidate| ExecutionPlan {
        expected_out: candidate.expected_out,
        router: candidate.route[0].router,
        block: request.liquidity.block,
        liquidity_source: request.liquidity.source,
        gas: candidate.gas,
        route: candidate.route,
    }))
}

fn validate(intent: &FoundationIntent, now: u64) -> Result<(), SolveError> {
    if intent.signer.is_zero() {
        return Err(SolveError::InvalidIntent("signer is zero"));
    }
    if intent.sell_token.is_zero() || intent.buy_token.is_zero() {
        return Err(SolveError::InvalidIntent("token is zero"));
    }
    if intent.sell_token == intent.buy_token {
        return Err(SolveError::InvalidIntent("tokens are equal"));
    }
    if intent.sell_amount.is_zero() || intent.min_buy_amount.is_zero() {
        return Err(SolveError::InvalidIntent("amount is zero"));
    }
    if intent.deadline <= now {
        return Err(SolveError::InvalidIntent("deadline expired"));
    }
    if intent.max_slippage_bps > 10_000 {
        return Err(SolveError::InvalidIntent("slippage exceeds 10000 bps"));
    }
    if intent.allowed_venues.is_empty() || intent.allowed_venues.iter().any(|venue| venue.is_zero())
    {
        return Err(SolveError::InvalidIntent(
            "allowed venues are empty or zero",
        ));
    }
    Ok(())
}

#[derive(Clone)]
struct Candidate {
    expected_out: U256,
    gas: u64,
    route: Vec<RouteHop>,
}
impl Candidate {
    fn route_key(&self) -> String {
        self.route
            .iter()
            .map(|hop| hop.pool_id.as_str())
            .collect::<Vec<_>>()
            .join("/")
    }
}

#[allow(clippy::too_many_arguments)] // Recursive route search state is explicit for deterministic traversal.
fn visit(
    token: Address,
    amount: U256,
    target: Address,
    remaining: usize,
    pools: &[ConstantProductPool],
    used: &mut BTreeSet<String>,
    route: Vec<RouteHop>,
    candidates: &mut Vec<Candidate>,
) {
    if token == target && !route.is_empty() {
        candidates.push(Candidate {
            expected_out: amount,
            gas: route.iter().map(|hop| hop.gas()).sum(),
            route,
        });
        return;
    }
    if remaining == 0 {
        return;
    }
    for pool in pools {
        if used.contains(&pool.id) {
            continue;
        }
        if let Some(previous) = route.last()
            && previous.router != pool.router
        {
            continue;
        }
        let Some((out_token, reserve_in, reserve_out)) = pool.direction(token) else {
            continue;
        };
        let amount_out = amount_out(amount, reserve_in, reserve_out, pool.fee_bps);
        if amount_out.is_zero() {
            continue;
        }
        used.insert(pool.id.clone());
        let mut next = route.clone();
        next.push(RouteHop {
            pool_id: pool.id.clone(),
            router: pool.router,
            token_in: token,
            token_out: out_token,
            amount_in: amount,
            amount_out,
            gas: pool.gas,
        });
        visit(
            out_token,
            amount_out,
            target,
            remaining - 1,
            pools,
            used,
            next,
            candidates,
        );
        used.remove(&pool.id);
    }
}

impl RouteHop {
    fn gas(&self) -> u64 {
        self.gas
    }
}
impl ConstantProductPool {
    fn direction(&self, token: Address) -> Option<(Address, U256, U256)> {
        if token == self.token0 {
            Some((self.token1, self.reserve0, self.reserve1))
        } else if token == self.token1 {
            Some((self.token0, self.reserve1, self.reserve0))
        } else {
            None
        }
    }
}

fn amount_out(amount_in: U256, reserve_in: U256, reserve_out: U256, fee_bps: u16) -> U256 {
    if reserve_in.is_zero() || reserve_out.is_zero() || fee_bps >= 10_000 {
        return U256::ZERO;
    }
    let fee = U256::from(10_000_u64 - fee_bps as u64);
    let numerator = amount_in * fee * reserve_out;
    let denominator = reserve_in * U256::from(10_000) + amount_in * fee;
    numerator / denominator
}

mod uint {
    use {
        alloy_primitives::U256,
        serde::{Deserialize, Deserializer, Serializer},
        std::str::FromStr,
    };
    pub fn serialize<S: Serializer>(value: &U256, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<U256, D::Error> {
        let value = String::deserialize(deserializer)?;
        U256::from_str(&value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: Address = Address::repeat_byte(1);
    const B: Address = Address::repeat_byte(2);
    const C: Address = Address::repeat_byte(3);
    const ROUTER: Address = Address::repeat_byte(4);
    const SETTLEMENT: Address = Address::repeat_byte(5);
    fn config() -> SolverConfig {
        SolverConfig {
            chain_id: 11155111,
            settlement_contract: SETTLEMENT,
            enabled_routers: HashSet::from([ROUTER]),
            max_hops: 2,
        }
    }
    fn intent() -> FoundationIntent {
        FoundationIntent {
            signer: A,
            nonce: U256::from(1),
            deadline: 2_000_000_000,
            max_slippage_bps: 50,
            allowed_venues: vec![ROUTER],
            sell_token: A,
            buy_token: B,
            sell_amount: U256::from(1000),
            min_buy_amount: U256::from(800),
        }
    }
    fn pool(
        id: &str,
        x: Address,
        y: Address,
        x_reserve: u64,
        y_reserve: u64,
    ) -> ConstantProductPool {
        ConstantProductPool {
            id: id.into(),
            address: Address::with_last_byte(id.as_bytes()[0]),
            router: ROUTER,
            token0: x,
            token1: y,
            reserve0: U256::from(x_reserve),
            reserve1: U256::from(y_reserve),
            fee_bps: 30,
            gas: 100_000,
        }
    }
    fn request(intent: FoundationIntent, pools: Vec<ConstantProductPool>) -> SolveRequest {
        SolveRequest {
            chain_id: 11155111,
            settlement_contract: SETTLEMENT,
            now: 1_000,
            intent,
            liquidity: Snapshot {
                source: "test".into(),
                block: 42,
                pools,
            },
        }
    }
    #[test]
    fn chooses_the_best_allowed_route() {
        let result = solve(
            &config(),
            request(
                intent(),
                vec![
                    pool("a", A, B, 10_000, 20_000),
                    pool("b", A, B, 10_000, 15_000),
                ],
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.route[0].pool_id, "a");
        assert!(result.expected_out >= U256::from(800));
    }
    #[test]
    fn enforces_floor_and_expiry() {
        let mut too_high = intent();
        too_high.min_buy_amount = U256::from(2_000);
        assert!(
            solve(
                &config(),
                request(too_high, vec![pool("a", A, B, 10_000, 20_000)])
            )
            .unwrap()
            .is_none()
        );
        let mut expired = intent();
        expired.deadline = 1_000;
        assert_eq!(
            solve(&config(), request(expired, vec![])).unwrap_err(),
            SolveError::InvalidIntent("deadline expired")
        );
    }
    #[test]
    fn rejects_wrong_foundation_target() {
        let mut request = request(intent(), vec![]);
        request.chain_id = 84532;
        assert_eq!(
            solve(&config(), request).unwrap_err(),
            SolveError::ChainMismatch
        );
    }
    #[test]
    fn rejects_a_router_not_enabled_for_foundation() {
        let mut blocked = pool("a", A, B, 10_000, 20_000);
        blocked.router = Address::repeat_byte(9);
        assert!(
            solve(&config(), request(intent(), vec![blocked]))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn supports_a_two_hop_route() {
        let mut multi = intent();
        multi.min_buy_amount = U256::from(1000);
        multi.allowed_venues = vec![ROUTER];
        let result = solve(
            &config(),
            request(
                multi,
                vec![
                    pool("a", A, C, 10_000, 20_000),
                    pool("b", C, B, 10_000, 20_000),
                ],
            ),
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.route.len(), 2);
    }
}
