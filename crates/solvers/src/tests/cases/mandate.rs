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
    engine_with(&[]).await
}

async fn engine_with(args: &[&str]) -> tests::SolverEngine {
    tests::SolverEngine::with_args(
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
        args,
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

/// A request whose liquidity source is gated, so it parks inside the handler
/// until the test lets it through.
fn gated_request(source: &str) -> serde_json::Value {
    let mut request = solve_request("1", json!([ROUTER_A]));
    request["liquiditySource"]["name"] = json!(source);
    request
}

/// A body over the limit is refused by the server rather than deserialized.
#[tokio::test]
async fn oversized_body_is_rejected() {
    let engine = engine().await;

    let mut request = solve_request("1", json!([ROUTER_A]));
    request["liquiditySource"]["name"] = json!("x".repeat(11 * 1024 * 1024));

    assert_eq!(
        engine.post_status("mandate/solve", request).await,
        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
    );
}

/// Beyond the concurrency limit, requests are shed immediately instead of
/// queueing, and the capacity comes back once the in-flight request finishes.
#[tokio::test]
async fn overload_sheds_and_recovers() {
    let engine = engine_with(&["--max-concurrent-requests=1"]).await;
    let gate = tests::gate::install("gate-overload");

    let occupy = engine.post_status("mandate/solve", gated_request("gate-overload"));
    let shed = async {
        // The first request now holds the only permit.
        gate.arrived().await;

        let response = engine
            .post_response("mandate/solve", solve_request("1", json!([ROUTER_A])))
            .await;
        assert_eq!(
            response.status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(response.headers()["retry-after"], "1");
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap(),
            json!({ "message": "solver is at capacity" }),
        );

        gate.release();
    };

    let (occupied, ()) = tokio::join!(occupy, shed);
    assert_eq!(occupied, axum::http::StatusCode::OK);

    let solution = engine
        .post("mandate/solve", solve_request("1", json!([ROUTER_A])))
        .await;
    assert_eq!(solution["solution"]["expectedOut"], OUT_A);
}

/// A request that finishes within the deadline is unaffected by it.
#[tokio::test]
async fn succeeds_below_timeout() {
    let engine = engine_with(&["--request-timeout=10s"]).await;

    let solution = engine
        .post("mandate/solve", solve_request("1", json!([ROUTER_A])))
        .await;

    assert_eq!(solution["solution"]["expectedOut"], OUT_A);
}

/// A request past its deadline is abandoned, and abandoning it returns the
/// permit it was holding.
#[tokio::test]
async fn timeout_releases_capacity() {
    let engine = engine_with(&["--request-timeout=200ms", "--max-concurrent-requests=1"]).await;
    // Never released: the request can only end by timing out.
    tests::gate::install("gate-timeout");

    let response = engine
        .post_response("mandate/solve", gated_request("gate-timeout"))
        .await;
    assert_eq!(response.status(), axum::http::StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        json!({ "message": "solving exceeded the request deadline" }),
    );

    let solution = engine
        .post("mandate/solve", solve_request("1", json!([ROUTER_A])))
        .await;
    assert_eq!(solution["solution"]["expectedOut"], OUT_A);
}

/// The value of one Prometheus sample, or 0 when it has not been touched.
///
/// The registry is process wide, so concurrently running tests share these
/// counters. Assertions below therefore only compare a counter against its own
/// earlier value, which is sound because counters never decrease.
fn sample(metrics: &str, key: &str) -> u64 {
    metrics
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .map_or(0, |value| value.trim().parse::<f64>().unwrap() as u64)
}

fn requests(metrics: &str, endpoint: &str, result: &str) -> u64 {
    sample(
        metrics,
        &format!(
            r#"solver_engine_mandate_requests_total{{endpoint="{endpoint}",result="{result}"}}"#
        ),
    )
}

fn durations(metrics: &str, endpoint: &str) -> u64 {
    sample(
        metrics,
        &format!(r#"solver_engine_mandate_duration_seconds_count{{endpoint="{endpoint}"}}"#),
    )
}

/// Every outcome a handler can reach is counted under its own result label, and
/// each request lands in the duration histogram.
#[tokio::test]
async fn metrics_record_handler_outcomes() {
    let engine = engine().await;
    let before = engine.metrics().await;

    engine
        .post("mandate/solve", solve_request("1", json!([ROUTER_A])))
        .await;
    engine
        .post("mandate/solve", solve_request("1", json!([SIGNER])))
        .await;
    engine
        .post_status("mandate/solve", solve_request("1", json!([])))
        .await;
    engine
        .post(
            "mandate/quote",
            json!({
                "sellToken": WETH,
                "buyToken": COW,
                "sellAmount": SELL_AMOUNT,
                "allowedVenues": [ROUTER_A],
                "liquidity": pools(),
                "liquiditySource": liquidity_source(),
            }),
        )
        .await;

    let after = engine.metrics().await;
    for (endpoint, result) in [
        ("solve", "fill"),
        ("solve", "no_fill"),
        ("solve", "bad_request"),
        ("quote", "fill"),
    ] {
        assert!(
            requests(&after, endpoint, result) > requests(&before, endpoint, result),
            "{endpoint}/{result} was not counted",
        );
    }
    assert!(durations(&after, "solve") > durations(&before, "solve"));
    assert!(durations(&after, "quote") > durations(&before, "quote"));
}

/// A shed request never reaches the handler, so the middleware counts it.
#[tokio::test]
async fn metrics_record_overload() {
    let engine = engine_with(&["--max-concurrent-requests=1"]).await;
    let gate = tests::gate::install("gate-overload-metrics");
    let before = engine.metrics().await;

    let occupy = engine.post_status("mandate/solve", gated_request("gate-overload-metrics"));
    let shed = async {
        gate.arrived().await;
        let status = engine
            .post_status("mandate/solve", solve_request("1", json!([ROUTER_A])))
            .await;
        assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
        gate.release();
    };
    tokio::join!(occupy, shed);

    let after = engine.metrics().await;
    assert!(requests(&after, "solve", "overloaded") > requests(&before, "solve", "overloaded"));
}

/// A request abandoned at its deadline is counted as a timeout, and still lands
/// in the duration histogram — which is the same drop that releases its
/// in-flight slot, so cancellation accounting is exercised here too.
#[tokio::test]
async fn metrics_record_timeout() {
    let engine = engine_with(&["--request-timeout=200ms"]).await;
    tests::gate::install("gate-timeout-metrics");
    let before = engine.metrics().await;

    let status = engine
        .post_status("mandate/solve", gated_request("gate-timeout-metrics"))
        .await;
    assert_eq!(status, axum::http::StatusCode::GATEWAY_TIMEOUT);

    let after = engine.metrics().await;
    assert!(requests(&after, "solve", "timeout") > requests(&before, "solve", "timeout"));
    assert!(durations(&after, "solve") > durations(&before, "solve"));
}

/// An engine pinned to one Mandate deployment.
async fn pinned_engine() -> tests::SolverEngine {
    tests::SolverEngine::new(
        "baseline",
        tests::Config::String(
            r#"
chain-id = "1"
base-tokens = []
max-hops = 0
max-partial-attempts = 5
native-token-price-estimation-amount = "100000000000000000"
mandate-chain-id = 1
mandate-settlement = "0xBcc2C99AE31477bc15309ba34126e3cb607E4117"
            "#
            .to_owned(),
        ),
    )
    .await
}

const SETTLEMENT: &str = "0xBcc2C99AE31477bc15309ba34126e3cb607E4117";

/// A route computed here cannot settle on another deployment, so a request
/// naming one — or naming none at all — is rejected rather than answered.
#[tokio::test]
async fn deployment_is_pinned() {
    let engine = pinned_engine().await;
    let request = |deployment: serde_json::Value| {
        let mut request = solve_request("1", json!([ROUTER_A]));
        let object = request.as_object_mut().unwrap();
        for (key, value) in deployment.as_object().unwrap() {
            object.insert(key.clone(), value.clone());
        }
        request
    };

    let matching = engine
        .post(
            "mandate/solve",
            request(json!({ "chainId": 1, "settlementContract": SETTLEMENT })),
        )
        .await;
    assert_eq!(matching["solution"]["expectedOut"], OUT_A);

    for wrong in [
        json!({ "chainId": 8453, "settlementContract": SETTLEMENT }),
        json!({ "chainId": 1, "settlementContract": POOL_A }),
        // Omitting the fields must not bypass the check.
        json!({ "chainId": 1 }),
        json!({}),
    ] {
        assert_eq!(
            engine.post_status("mandate/solve", request(wrong)).await,
            axum::http::StatusCode::BAD_REQUEST,
        );
    }
}

/// The deployment is unpinned unless configured, so the checked fields stay
/// optional for callers that do not send them.
#[tokio::test]
async fn deployment_is_unchecked_when_unconfigured() {
    let engine = engine().await;

    let solution = engine
        .post("mandate/solve", solve_request("1", json!([ROUTER_A])))
        .await;

    assert_eq!(solution["solution"]["expectedOut"], OUT_A);
}
