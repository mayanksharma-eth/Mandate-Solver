use {
    axum::{
        Json, Router,
        extract::State,
        http::StatusCode,
        routing::{get, post},
    },
    mandate_solver::{ExecutionPlan, SolveRequest, SolverConfig, solve},
    std::sync::Arc,
};

#[derive(Clone)]
struct AppState(SolverConfig);

#[tokio::main]
async fn main() {
    let chain_id = std::env::var("MANDATE_CHAIN_ID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(11155111);
    let settlement_contract = std::env::var("MANDATE_SETTLEMENT_ADDRESS")
        .expect("MANDATE_SETTLEMENT_ADDRESS is required")
        .parse()
        .expect("invalid MANDATE_SETTLEMENT_ADDRESS");
    let state = Arc::new(AppState(SolverConfig {
        chain_id,
        settlement_contract,
        max_hops: 2,
    }));
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/solve", post(solve_route))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn solve_route(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SolveRequest>,
) -> Result<Json<Option<ExecutionPlan>>, (StatusCode, String)> {
    solve(&state.0, request)
        .map(Json)
        .map_err(|error| (StatusCode::BAD_REQUEST, error.to_string()))
}
