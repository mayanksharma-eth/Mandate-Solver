//! Test cases for the Mandate per-intent endpoints.

use {crate::tests, serde_json::json};

const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
const COW: &str = "0xDEf1CA1fb7FBcDC777520aa7f396b4E015F497aB";

/// The better of the two pools, and its router.
const POOL_A: &str = "0x97b744df0b59d93A866304f97431D8EfAd29a08d";
const ROUTER_A: &str = "0x7a250d5630b4cf539739df2c5dacb4c659f2488d";
/// The worse pool: identical shape, half the reserves.
const POOL_B: &str = "0x3041cbd36888becc7bbcbc0045e3b1f144466f5f";
const ROUTER_B: &str = "0xd9e1ce17f2641f24ae83637ab66a2cca9c378b9f";

const SELL_AMOUNT: &str = "133700000000000000";
const SIGNER: &str = "0x1111111111111111111111111111111111111111";
const DEADLINE: u64 = 4_102_444_800;
/// Constant-product output of `SELL_AMOUNT` against pool A, wei-exact:
/// `997 * x * Y / (1000 * X + 997 * x)`.
const OUT_A: &str = "6043910341261930467761";
/// Same, against pool B.
const OUT_B: &str = "5847160920332621778484";

async fn engine() -> tests::SolverEngine {
    tests::SolverEngine::new(
        "baseline",
        tests::Config::String(
            r#"
chain-id = "1"
base-tokens = []
max-hops = 0
max-partial-attempts = 5
native-token-price-estimation-amount = "100000000000000000"
            "#
            .to_owned(),
        ),
    )
    .await
}

fn pool(id: &str, address: &str, router: &str, weth: &str, cow: &str) -> serde_json::Value {
    json!({
        "kind": "constantProduct",
        "id": id,
        "address": address,
        "router": router,
        "gasEstimate": "110000",
        "fee": "0.003",
        "tokens": {
            WETH: { "balance": weth },
            COW: { "balance": cow },
        },
    })
}

fn pools() -> serde_json::Value {
    json!([
        pool(
            "a",
            POOL_A,
            ROUTER_A,
            "3828187314911751990",
            "179617892578796375604692"
        ),
        pool(
            "b",
            POOL_B,
            ROUTER_B,
            "1914093657455875995",
            "89808946289398187802346"
        ),
    ])
}

fn liquidity_source() -> serde_json::Value {
    json!({ "name": "test-indexer", "block": 21000000 })
}

fn solve_request(min_buy_amount: &str, allowed_venues: serde_json::Value) -> serde_json::Value {
    json!({
        "intent": {
            "signer": SIGNER,
            "nonce": "42",
            "sellToken": WETH,
            "buyToken": COW,
            "sellAmount": SELL_AMOUNT,
            "minBuyAmount": min_buy_amount,
            "maxSlippageBps": 50,
            "deadline": DEADLINE,
            "allowedVenues": allowed_venues,
        },
        "liquidity": pools(),
        "liquiditySource": liquidity_source(),
    })
}

/// Same snapshot and intent must produce a byte-identical route, and the
/// output must be the wei-exact constant-product result so a downstream test
/// can reproduce it on-chain.
#[tokio::test]
async fn replay_is_deterministic() {
    let engine = engine().await;
    let request = solve_request("1", json!([ROUTER_A, ROUTER_B]));

    let first = engine.post("mandate/solve", request.clone()).await;
    let second = engine.post("mandate/solve", request).await;

    assert_eq!(first, second);
    assert_eq!(
        first,
        json!({
            "solution": {
                "expectedOut": OUT_A,
                "liquiditySource": "test-indexer",
                "block": 21000000,
                "gas": 60000,
                "route": [{
                    "liquidity": "a",
                    "inputToken": WETH.to_lowercase(),
                    "inputAmount": SELL_AMOUNT,
                    "outputToken": COW.to_lowercase(),
                    "outputAmount": OUT_A,
                }],
            }
        })
    );
}

/// A venue that is not on the intent's allowlist must never appear in a route,
/// even when it offers strictly better execution.
#[tokio::test]
async fn allowlist_excludes_better_venue() {
    let engine = engine().await;

    let solution = engine
        .post("mandate/solve", solve_request("1", json!([ROUTER_B])))
        .await;
    let solution = &solution["solution"];

    assert_eq!(solution["expectedOut"], OUT_B);
    assert_eq!(solution["route"].as_array().unwrap().len(), 1);
    assert_eq!(solution["route"][0]["liquidity"], "b");
}

/// Matching is on the pool address as well as the router.
#[tokio::test]
async fn allowlist_matches_pool_address() {
    let engine = engine().await;

    let solution = engine
        .post("mandate/solve", solve_request("1", json!([POOL_B])))
        .await;

    assert_eq!(solution["solution"]["route"][0]["liquidity"], "b");
}

/// When no allowed venue can fill the intent, there is no solution — not a
/// route through a disallowed one.
#[tokio::test]
async fn no_allowed_venue_yields_no_solution() {
    let engine = engine().await;

    let solution = engine
        .post(
            "mandate/solve",
            solve_request("1", json!(["0x1111111111111111111111111111111111111111"])),
        )
        .await;

    assert_eq!(solution, json!({ "solution": null }));
}

/// An empty allowlist is malformed: it could never produce a valid Foundation
/// settlement, so reject it instead of doing route work.
#[tokio::test]
async fn empty_allowlist_is_rejected() {
    let engine = engine().await;

    let status = engine
        .post_status("mandate/solve", solve_request("1", json!([])))
        .await;

    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
}

/// A route below the intent's floor is not a fill.
#[tokio::test]
async fn floor_is_enforced() {
    let engine = engine().await;

    // One wei above the best achievable output.
    let above_best = "6043910341261930467762";
    let solution = engine
        .post(
            "mandate/solve",
            solve_request(above_best, json!([ROUTER_A, ROUTER_B])),
        )
        .await;
    assert_eq!(solution, json!({ "solution": null }));

    // Exactly at the best achievable output still fills.
    let solution = engine
        .post(
            "mandate/solve",
            solve_request(OUT_A, json!([ROUTER_A, ROUTER_B])),
        )
        .await;
    assert_eq!(solution["solution"]["expectedOut"], OUT_A);
}

/// The pre-sign quote is the output of the route `/mandate/solve` picks for
/// the same intent against the same snapshot.
#[tokio::test]
async fn quote_matches_solve() {
    let engine = engine().await;

    for venues in [json!([ROUTER_A, ROUTER_B]), json!([ROUTER_B])] {
        let quote = engine
            .post(
                "mandate/quote",
                json!({
                    "sellToken": WETH,
                    "signer": SIGNER,
                    "nonce": "42",
                    "buyToken": COW,
                    "sellAmount": SELL_AMOUNT,
                    "allowedVenues": venues,
                    "liquidity": pools(),
                    "liquiditySource": liquidity_source(),
                }),
            )
            .await;
        let solution = engine
            .post("mandate/solve", solve_request("1", venues.clone()))
            .await;

        assert_eq!(
            quote["quote"]["expectedOut"],
            solution["solution"]["expectedOut"]
        );
        assert_eq!(quote["quote"]["block"], solution["solution"]["block"]);
        assert_eq!(
            quote["quote"]["liquiditySource"],
            solution["solution"]["liquiditySource"]
        );
    }
}

/// Liquidity without a source and block is not solvable.
#[tokio::test]
async fn untagged_liquidity_is_rejected() {
    let engine = engine().await;

    let status = engine
        .post_status(
            "mandate/solve",
            json!({
                "intent": {
                    "sellToken": WETH,
                    "buyToken": COW,
                    "sellAmount": SELL_AMOUNT,
                    "minBuyAmount": "1",
                    "maxSlippageBps": 50,
                    "deadline": DEADLINE,
                    "allowedVenues": [ROUTER_A],
                },
                "liquidity": pools(),
            }),
        )
        .await;

    assert!(!status.is_success(), "untagged liquidity was accepted");
}

/// A route for an expired or structurally invalid Foundation intent would be
/// unusable, so the solver rejects it before spending routing effort.
#[tokio::test]
async fn rejects_unsettleable_intents_before_routing() {
    let engine = engine().await;

    let expired = engine
        .post_status(
            "mandate/solve",
            json!({
                "intent": {
                    "signer": SIGNER,
                    "nonce": "42",
                    "sellToken": WETH,
                    "buyToken": COW,
                    "sellAmount": SELL_AMOUNT,
                    "minBuyAmount": "1",
                    "maxSlippageBps": 50,
                    "deadline": 1,
                    "allowedVenues": [ROUTER_A],
                },
                "liquidity": pools(),
                "liquiditySource": liquidity_source(),
            }),
        )
        .await;
    assert_eq!(expired, axum::http::StatusCode::BAD_REQUEST);

    let zero_signer = engine
        .post_status(
            "mandate/solve",
            json!({
                "intent": {
                    "signer": "0x0000000000000000000000000000000000000000",
                    "nonce": "42",
                    "sellToken": WETH,
                    "buyToken": COW,
                    "sellAmount": SELL_AMOUNT,
                    "minBuyAmount": "1",
                    "maxSlippageBps": 50,
                    "deadline": DEADLINE,
                    "allowedVenues": [ROUTER_A],
                },
                "liquidity": pools(),
                "liquiditySource": liquidity_source(),
            }),
        )
        .await;
    assert_eq!(zero_signer, axum::http::StatusCode::BAD_REQUEST);
}
