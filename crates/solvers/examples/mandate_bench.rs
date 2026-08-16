//! Load and latency benchmark for `POST /mandate/solve`.
//!
//! Drives the real HTTP path — middleware, validation, allowlist filtering,
//! routing, response — against a running `solvers` binary, and probes
//! `GET /healthz` from a separate low-concurrency client throughout, so runtime
//! starvation shows up as health-check latency.
//!
//! The liquidity fixture is deterministic and derived entirely from
//! `--entries`: `--print-config` emits the solver config whose base tokens match
//! the fixture, so a run is reproducible from the two flags alone.
//!
//! ```sh
//! cargo run --release -p solvers --example mandate_bench -- \
//!     --entries 500 --concurrency 32 --requests 320
//! ```

use {
    clap::Parser,
    serde_json::json,
    std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    },
};

/// Canonical WETH, so the engine's `chain-id = "1"` config resolves to it.
const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
const BUY: &str = "0xDEf1CA1fb7FBcDC777520aa7f396b4E015F497aB";
/// Every pool shares one router, so the intent's allowlist stays a single entry
/// no matter how large the liquidity set gets.
const ROUTER: &str = "0x7a250d5630b4cf539739df2c5dacb4c659f2488d";
const SELL_AMOUNT: &str = "1000000000000000000";

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8099")]
    url: String,
    /// Liquidity entries in the request payload.
    #[arg(long, default_value = "10")]
    entries: usize,
    /// Concurrent in-flight requests.
    #[arg(long, default_value = "1")]
    concurrency: usize,
    /// Total requests to send. 0 only probes `/healthz`, for an idle baseline.
    #[arg(long, default_value = "200")]
    requests: usize,
    /// How long to probe `/healthz` when `--requests 0`.
    #[arg(long, default_value = "3")]
    idle_secs: u64,
    /// Gap between `/healthz` probes.
    #[arg(long, default_value = "20")]
    healthz_interval_ms: u64,
    /// Name for this scenario in the JSON output.
    #[arg(long, default_value = "run")]
    label: String,
    /// Base tokens in the generated config. Defaults to one per intermediate
    /// token, i.e. every liquidity entry is on a candidate path. A small fixed
    /// value models a production config, where the base-token set does not grow
    /// with the liquidity an indexer sends.
    #[arg(long)]
    base_tokens: Option<usize>,
    /// Print the matching solver config and exit.
    #[arg(long)]
    print_config: bool,
    /// Print the request payload and exit.
    #[arg(long)]
    print_payload: bool,
    /// Send the same payload with an allowlist no venue matches. Everything
    /// before route finding still runs — parsing, validation, filtering — so the
    /// gap to a normal run is the pathfinder's share of the latency.
    #[arg(long)]
    no_route: bool,
}

fn token(i: usize) -> String {
    format!("0x{:040x}", 0x1000 + i)
}

fn pool_address(i: usize) -> String {
    format!("0x{:040x}", 0x0010_0000 + i)
}

/// The fixture shape: `direct` pools straight from WETH to the buy token, and
/// for each intermediate token a WETH -> T and a T -> BUY pool. Every
/// intermediate is a configured base token, so each one is a real path
/// candidate the router has to price rather than dead weight.
fn topology(entries: usize) -> (usize, usize) {
    let direct = 2.min(entries);
    let intermediates = entries.saturating_sub(direct) / 2;
    (direct, intermediates)
}

fn pool(
    id: usize,
    address: String,
    token_a: &str,
    balance_a: u128,
    token_b: &str,
    balance_b: u128,
) -> serde_json::Value {
    json!({
        "kind": "constantProduct",
        "id": id.to_string(),
        "address": address,
        "router": ROUTER,
        "gasEstimate": "110000",
        "fee": "0.003",
        "tokens": {
            token_a: { "balance": balance_a.to_string() },
            token_b: { "balance": balance_b.to_string() },
        },
    })
}

fn liquidity(entries: usize) -> Vec<serde_json::Value> {
    const ETH: u128 = 1_000_000_000_000_000_000;
    let (direct, intermediates) = topology(entries);
    let mut pools = Vec::with_capacity(direct + 2 * intermediates);

    for j in 0..direct {
        pools.push(pool(
            pools.len(),
            pool_address(pools.len()),
            WETH,
            100 * ETH + (j as u128) * ETH,
            BUY,
            200_000 * ETH,
        ));
    }
    for i in 0..intermediates {
        let intermediate = token(i);
        pools.push(pool(
            pools.len(),
            pool_address(pools.len()),
            WETH,
            1_000 * ETH + (i as u128) * ETH,
            &intermediate,
            3_000_000 * ETH,
        ));
        pools.push(pool(
            pools.len(),
            pool_address(pools.len()),
            &intermediate,
            3_000_000 * ETH,
            BUY,
            6_000_000 * ETH + (i as u128) * ETH,
        ));
    }
    pools
}

fn payload(entries: usize, no_route: bool) -> serde_json::Value {
    let venue = if no_route {
        // Passes validation, matches no pool or router in the fixture.
        "0x2222222222222222222222222222222222222222"
    } else {
        ROUTER
    };
    json!({
        "intent": {
            "signer": "0x1111111111111111111111111111111111111111",
            "nonce": "42",
            "sellToken": WETH,
            "buyToken": BUY,
            "sellAmount": SELL_AMOUNT,
            "minBuyAmount": "1",
            "maxSlippageBps": 50,
            "deadline": 4_102_444_800u64,
            "allowedVenues": [venue],
        },
        "liquidity": liquidity(entries),
        "liquiditySource": { "name": "bench", "block": 21_000_000 },
    })
}

fn config(entries: usize, base_tokens: Option<usize>) -> String {
    let (_, intermediates) = topology(entries);
    let intermediates = base_tokens.unwrap_or(intermediates).min(intermediates);
    let base_tokens = (0..intermediates)
        .map(|i| format!("  \"{}\",", token(i)))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "chain-id = \"1\"\nmax-hops = 1\nmax-partial-attempts = 5\nnative-token-price-estimation-amount = \
         \"100000000000000000\"\nbase-tokens = [\n{base_tokens}\n]\n"
    )
}

struct Sample {
    millis: f64,
    status: u16,
    /// Why the request never produced a status, for the `0` rows.
    error: Option<&'static str>,
}

fn classify(err: &reqwest::Error) -> &'static str {
    if err.is_connect() {
        "connect"
    } else if err.is_timeout() {
        "timeout"
    } else if err.is_body() || err.is_decode() {
        "body"
    } else if err.is_request() {
        "request"
    } else {
        "other"
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p / 100.0 * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank]
}

fn summarize(samples: &[f64]) -> serde_json::Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({
        "count": sorted.len(),
        "p50_ms": percentile(&sorted, 50.0),
        "p95_ms": percentile(&sorted, 95.0),
        "p99_ms": percentile(&sorted, 99.0),
        "max_ms": sorted.last().copied().unwrap_or_default(),
    })
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    if args.print_config {
        print!("{}", config(args.entries, args.base_tokens));
        return;
    }
    let body = serde_json::to_string(&payload(args.entries, args.no_route)).unwrap();
    if args.print_payload {
        println!("{body}");
        return;
    }

    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(args.concurrency + 4)
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();
    let solve_url = format!("{}/mandate/solve", args.url.trim_end_matches('/'));
    let healthz_url = format!("{}/healthz", args.url.trim_end_matches('/'));

    // Warm up connections and any lazily built state so the first measured
    // request is not paying for both.
    let warmup = Instant::now();
    let warmup_status = client
        .post(&solve_url)
        .header("content-type", "application/json")
        .body(body.clone())
        .send()
        .await
        .map(|response| response.status().as_u16())
        .unwrap_or(0);
    let warmup_ms = warmup.elapsed().as_secs_f64() * 1e3;

    let done = Arc::new(AtomicBool::new(false));
    let healthz = tokio::spawn({
        let (client, url, done) = (client.clone(), healthz_url, done.clone());
        let interval = Duration::from_millis(args.healthz_interval_ms);
        async move {
            let mut samples = Vec::new();
            while !done.load(Ordering::Relaxed) {
                let start = Instant::now();
                let ok = client.get(&url).send().await.is_ok();
                if ok {
                    samples.push(start.elapsed().as_secs_f64() * 1e3);
                }
                tokio::time::sleep(interval).await;
            }
            samples
        }
    });

    let started = Instant::now();
    let mut samples = Vec::new();
    if args.requests == 0 {
        tokio::time::sleep(Duration::from_secs(args.idle_secs)).await;
    } else {
        let workers = (0..args.concurrency).map(|worker| {
            let (client, url, body) = (client.clone(), solve_url.clone(), body.clone());
            // Spread the remainder over the first workers so the total is exact.
            let count = args.requests / args.concurrency
                + usize::from(worker < args.requests % args.concurrency);
            tokio::spawn(async move {
                let mut samples = Vec::with_capacity(count);
                for _ in 0..count {
                    let start = Instant::now();
                    let (status, error) = match client
                        .post(&url)
                        .header("content-type", "application/json")
                        .body(body.clone())
                        .send()
                        .await
                    {
                        Ok(response) => {
                            let status = response.status().as_u16();
                            match response.bytes().await {
                                Ok(_) => (status, None),
                                Err(err) => (status, Some(classify(&err))),
                            }
                        }
                        Err(err) => (0, Some(classify(&err))),
                    };
                    samples.push(Sample {
                        millis: start.elapsed().as_secs_f64() * 1e3,
                        status,
                        error,
                    });
                }
                samples
            })
        });
        for worker in futures::future::join_all(workers).await {
            samples.extend(worker.unwrap());
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    done.store(true, Ordering::Relaxed);
    let healthz = healthz.await.unwrap();

    let mut by_status = serde_json::Map::new();
    let mut statuses: Vec<u16> = samples.iter().map(|sample| sample.status).collect();
    statuses.sort_unstable();
    statuses.dedup();
    for status in statuses {
        let latencies = samples
            .iter()
            .filter(|sample| sample.status == status)
            .map(|sample| sample.millis)
            .collect::<Vec<_>>();
        by_status.insert(status.to_string(), summarize(&latencies));
    }

    let mut errors = serde_json::Map::new();
    for reason in samples.iter().filter_map(|sample| sample.error) {
        let entry = errors.entry(reason).or_insert(json!(0));
        *entry = json!(entry.as_u64().unwrap_or_default() + 1);
    }

    let latencies = samples
        .iter()
        .map(|sample| sample.millis)
        .collect::<Vec<_>>();
    let (direct, intermediates) = topology(args.entries);
    println!(
        "{}",
        json!({
            "label": args.label,
            "entries": direct + 2 * intermediates,
            "concurrency": args.concurrency,
            "requests": samples.len(),
            "payload_bytes": body.len(),
            "wall_secs": elapsed,
            "rps": if elapsed > 0.0 { samples.len() as f64 / elapsed } else { 0.0 },
            "warmup": { "status": warmup_status, "ms": warmup_ms },
            "latency": summarize(&latencies),
            "by_status": by_status,
            "errors": errors,
            "healthz": summarize(&healthz),
        })
    );
}
