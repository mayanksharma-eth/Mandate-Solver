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
    let mut config = SolverConfig::base_sepolia_alpha();
    if let Ok(value) = std::env::var("MANDATE_CHAIN_ID") {
        config.chain_id = value.parse().expect("invalid MANDATE_CHAIN_ID");
    }
    if let Ok(value) = std::env::var("MANDATE_SETTLEMENT_ADDRESS") {
        config.settlement_contract = value.parse().expect("invalid MANDATE_SETTLEMENT_ADDRESS");
    }
    let state = Arc::new(AppState(config));
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
