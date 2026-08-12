use {
    super::Response,
    crate::domain::{eth, mandate, solver::Solver},
    std::sync::Arc,
    tracing::Instrument,
};

pub mod dto;

type Json<T> = axum::response::Json<Response<T>>;

pub async fn solve(
    state: axum::extract::State<Arc<Solver>>,
    axum::extract::Json(request): axum::extract::Json<dto::SolveRequest>,
) -> (axum::http::StatusCode, Json<dto::SolveResponse>) {
    let handle_request = async {
        if let Err(err) = dto::validate_intent(&request.intent) {
            return bad_request(err);
        }
        let snapshot = match dto::snapshot_to_domain(&request.liquidity, request.liquidity_source) {
            Ok(value) => value,
            Err(err) => return bad_request(err),
        };
        let intent = dto::intent_to_domain(&request.intent);

        let fill = mandate::solve(&state.routing(), &intent, &snapshot).await;
        tracing::trace!(?intent, ?fill, "solved intent");

        ok(dto::SolveResponse {
            solution: fill.map(Into::into),
        })
    };

    handle_request
        .instrument(tracing::info_span!("/mandate/solve"))
        .await
}

pub async fn quote(
    state: axum::extract::State<Arc<Solver>>,
    axum::extract::Json(request): axum::extract::Json<dto::QuoteRequest>,
) -> (axum::http::StatusCode, Json<dto::QuoteResponse>) {
    let handle_request = async {
        if let Err(err) = dto::validate_quote(&request) {
            return bad_request(err);
        }
        let snapshot = match dto::snapshot_to_domain(&request.liquidity, request.liquidity_source) {
            Ok(value) => value,
            Err(err) => return bad_request(err),
        };

        let sell = eth::Asset {
            token: eth::TokenAddress(request.sell_token),
            amount: request.sell_amount,
        };
        let quote = mandate::quote(
            &state.routing(),
            sell,
            eth::TokenAddress(request.buy_token),
            request.allowed_venues.into_iter().collect(),
            &snapshot,
        )
        .await;

        ok(dto::QuoteResponse {
            quote: quote.map(Into::into),
        })
    };

    handle_request
        .instrument(tracing::info_span!("/mandate/quote"))
        .await
}

fn ok<T>(value: T) -> (axum::http::StatusCode, Json<T>) {
    (
        axum::http::StatusCode::OK,
        axum::response::Json(Response::Ok(value)),
    )
}

fn bad_request<T>(err: super::Error) -> (axum::http::StatusCode, Json<T>) {
    tracing::warn!(?err, "invalid mandate request");
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::response::Json(Response::Err(err)),
    )
}
