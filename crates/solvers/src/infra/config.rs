use {
    crate::{
        domain::{eth, solver},
        infra::contracts,
    },
    chain::Chain,
    reqwest::Url,
    serde::Deserialize,
    shared::price_estimation::gas::SETTLEMENT_OVERHEAD,
    std::{fmt::Debug, path::Path},
    tokio::fs,
};

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct Config {
    /// Optional chain ID. This is used to automatically determine the address
    /// of the WETH contract.
    chain_id: Option<Chain>,

    /// Optional WETH contract address. This can be used to specify a manual
    /// value **instead** of using the canonical WETH contract for the
    /// configured chain.
    weth: Option<eth::Address>,

    /// List of base tokens to use when path finding. This defines the tokens
    /// that can appear as intermediate "hops" within a trading route. Note that
    /// WETH is always considered as a base token.
    base_tokens: Vec<eth::Address>,

    /// The maximum number of hops to consider when finding the optimal trading
    /// path.
    max_hops: usize,

    /// The maximum number of pieces to divide partially fillable limit orders
    /// when trying to solve it against baseline liquidity.
    max_partial_attempts: usize,

    /// Units of gas that get added to the gas estimate for executing a
    /// computed trade route to arrive at a gas estimate for a whole settlement.
    #[serde(default = "default_gas_offset")]
    solution_gas_offset: i64,

    /// The amount of the native token to use to estimate native price of a
    /// token
    native_token_price_estimation_amount: eth::U256,

    /// If this is configured the solver will also use the Uniswap V3 liquidity
    /// sources that rely on RPC request.
    uni_v3_node_url: Option<Url>,

    /// The Mandate deployment this engine serves. When set, a `/mandate/*`
    /// request must name the same chain, so an interface pointed at the wrong
    /// deployment is rejected instead of being handed a route it cannot settle.
    mandate_chain_id: Option<u64>,

    /// The `MandateSettlement` address this engine serves. Same handling as
    /// `mandate_chain_id`.
    mandate_settlement: Option<eth::Address>,
}

/// The most base tokens a config may declare. Path finding runs inline on the
/// request's runtime worker and cannot be preempted once it starts, so the size
/// of the candidate set is an availability concern, not just a latency one. See
/// `docs/MANDATE-BENCH.md`: at ~250 base tokens a solve already costs ~16ms
/// under load, and at ~2500 it starves unrelated endpoints for seconds. This cap
/// is an order of magnitude above every config in this repository.
const MAX_BASE_TOKENS: usize = 128;

/// The most intermediary hops a config may allow. Deployed configs use 0 or 2.
const MAX_HOPS: usize = 4;

/// The most candidate paths a config may be able to produce. Bounds the
/// combination the individual caps above miss: a modest base token set with
/// several hops enumerates a number of paths neither limit would catch.
const MAX_PATH_CANDIDATES: usize = 4096;

/// Upper bound on the paths [`shared::baseline_solver`] enumerates: it extends
/// each prefix by every base token not already on it, for `max_hops + 1` rounds.
fn path_candidate_bound(base_tokens: usize, max_hops: usize) -> usize {
    // WETH is always a base token, whether or not the config lists it.
    let tokens = base_tokens.saturating_add(1);
    (0..=max_hops)
        .map(|hop| tokens.saturating_pow(hop as u32))
        .fold(0, usize::saturating_add)
}

/// Rejects routing parameters whose candidate set would be large enough to hold
/// a runtime worker for a long, uninterruptible stretch.
fn validate_routing(base_tokens: usize, max_hops: usize) -> Result<(), String> {
    if base_tokens > MAX_BASE_TOKENS {
        return Err(format!(
            "`base-tokens` has {base_tokens} entries, more than the supported maximum of \
             {MAX_BASE_TOKENS}"
        ));
    }
    if max_hops > MAX_HOPS {
        return Err(format!(
            "`max-hops` is {max_hops}, more than the supported maximum of {MAX_HOPS}"
        ));
    }
    let candidates = path_candidate_bound(base_tokens, max_hops);
    if candidates > MAX_PATH_CANDIDATES {
        return Err(format!(
            "`base-tokens` ({base_tokens}) and `max-hops` ({max_hops}) allow up to {candidates} \
             candidate paths per request, more than the supported maximum of \
             {MAX_PATH_CANDIDATES}"
        ));
    }
    Ok(())
}

/// Load the driver configuration from a TOML file.
///
/// # Panics
///
/// This method panics if the config is invalid or on I/O errors.
pub async fn load(path: &Path) -> solver::Config {
    let data = fs::read_to_string(path)
        .await
        .unwrap_or_else(|e| panic!("I/O error while reading {path:?}: {e:?}"));
    // Not printing detailed error because it could potentially leak secrets.
    let config = unwrap_or_log(toml::de::from_str::<Config>(&data), &path);
    if let Err(err) = validate_routing(config.base_tokens.len(), config.max_hops) {
        panic!("invalid configuration: {err}");
    }
    let weth = match (config.chain_id, config.weth) {
        (Some(chain_id), None) => contracts::Contracts::for_chain(chain_id).weth,
        (None, Some(weth)) => eth::WethAddress(weth),
        (Some(_), Some(_)) => panic!(
            "invalid configuration: cannot specify both `chain-id` and `weth` configuration \
             options",
        ),
        (None, None) => panic!(
            "invalid configuration: must specify either `chain-id` or `weth` configuration options",
        ),
    };

    solver::Config {
        weth,
        base_tokens: config
            .base_tokens
            .into_iter()
            .map(eth::TokenAddress)
            .collect(),
        max_hops: config.max_hops,
        max_partial_attempts: config.max_partial_attempts,
        solution_gas_offset: config.solution_gas_offset.into(),
        native_token_price_estimation_amount: config.native_token_price_estimation_amount,
        uni_v3_node_url: config.uni_v3_node_url,
        mandate: solver::MandateDeployment {
            chain_id: config.mandate_chain_id,
            settlement: config.mandate_settlement,
        },
    }
}

/// Unwraps result or logs a `TOML` parsing error.
fn unwrap_or_log<T, E, P>(result: Result<T, E>, path: &P) -> T
where
    E: Debug,
    P: Debug,
{
    result.unwrap_or_else(|err| {
        if std::env::var("TOML_TRACE_ERROR").is_ok_and(|v| v == "1") {
            panic!("failed to parse TOML config at {path:?}: {err:#?}")
        } else {
            panic!(
                "failed to parse TOML config at: {path:?}. Set TOML_TRACE_ERROR=1 to print \
                 parsing error but this may leak secrets."
            )
        }
    })
}

/// Returns minimum gas used for settling a single order.
/// (not accounting for the cost of additional interactions)
fn default_gas_offset() -> i64 {
    SETTLEMENT_OVERHEAD.try_into().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The configs this repository ships must keep loading.
    #[tokio::test]
    async fn deployed_configs_are_accepted() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for config in [
            "configs/local/baseline.toml",
            "configs/local/mandate-base-sepolia.toml",
        ] {
            load(&root.join(config)).await;
        }
    }

    #[test]
    fn typical_routing_configs_are_accepted() {
        // Mandate on Base Sepolia: direct swaps only.
        assert!(validate_routing(0, 0).is_ok());
        // The mainnet baseline config in this repository.
        assert!(validate_routing(8, 2).is_ok());
        // Headroom for a much larger base token set than anything deployed.
        assert!(validate_routing(30, 2).is_ok());
        assert!(validate_routing(MAX_BASE_TOKENS, 1).is_ok());
    }

    #[test]
    fn pathological_base_token_counts_are_rejected() {
        assert!(validate_routing(MAX_BASE_TOKENS + 1, 0).is_err());
        // The size that starved unrelated endpoints in docs/MANDATE-BENCH.md.
        assert!(validate_routing(2499, 1).is_err());
    }

    #[test]
    fn pathological_max_hops_are_rejected() {
        assert!(validate_routing(8, MAX_HOPS + 1).is_err());
    }

    /// Values each within their own limit, but explosive together.
    #[test]
    fn candidate_path_explosion_is_rejected() {
        assert!(validate_routing(MAX_BASE_TOKENS, 3).is_err());
        assert!(validate_routing(64, 2).is_err());
    }

    #[test]
    fn candidate_bound_covers_the_solver() {
        // No base tokens and no hops is the single direct sell -> buy path.
        assert_eq!(path_candidate_bound(0, 0), 1);
        // Then sell -> {weth, a, b} -> buy, and their two-hop extensions.
        assert_eq!(path_candidate_bound(2, 1), 4);
        assert_eq!(path_candidate_bound(2, 2), 13);
    }
}
