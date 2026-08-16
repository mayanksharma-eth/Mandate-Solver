use {
    super::Response,
    crate::{
        domain::{eth, mandate, solver::Solver},
        infra::metrics::{self, MandateRequest, endpoint, result},
    },
    std::sync::Arc,
    tracing::{Instrument, field},
};

pub mod dto;

type Json<T> = axum::response::Json<Response<T>>;

pub async fn solve(
    state: axum::extract::State<Arc<Solver>>,
    axum::extract::Json(request): axum::extract::Json<dto::SolveRequest>,
) -> (axum::http::StatusCode, Json<dto::SolveResponse>) {
    let span = tracing::info_span!(
        "/mandate/solve",
        endpoint = endpoint::SOLVE,
        chain_id = request.deployment.chain_id,
        liquidity = request.liquidity.len(),
        venues = request.intent.allowed_venues.len(),
        sell_amount = %request.intent.sell_amount,
        result = field::Empty,
        segments = field::Empty,
        expected_out = field::Empty,
        duration_ms = field::Empty,
    );
    let mut metrics = metrics::mandate_request(endpoint::SOLVE);

    let handle_request = async {
        // Lets a test hold a request inside the middleware stack for as long as
        // it needs to, without a sleep.
        #[cfg(test)]
        crate::tests::gate::wait(&request.liquidity_source.name).await;

        if let Err(err) = dto::validate_deployment(&request.deployment, &state.mandate_deployment())
        {
            return bad_request(&mut metrics, err);
        }
        if let Err(err) = dto::validate_intent(&request.intent) {
            return bad_request(&mut metrics, err);
        }
        let snapshot = match dto::snapshot_to_domain(&request.liquidity, request.liquidity_source) {
            Ok(value) => value,
            Err(err) => return bad_request(&mut metrics, err),
        };
        let intent = dto::intent_to_domain(&request.intent);

        let fill = mandate::solve(&state.routing(), &intent, &snapshot).await;
        tracing::trace!(?intent, ?fill, "solved intent");

        match &fill {
            Some(fill) => {
                let span = tracing::Span::current();
                span.record("segments", fill.segments.len());
                span.record("expected_out", field::display(fill.expected_out));
                metrics.record(result::FILL);
            }
            None => metrics.record(result::NO_FILL),
        }

        ok(dto::SolveResponse {
            solution: fill.map(Into::into),
        })
    };

    handle_request.instrument(span).await
}

pub async fn quote(
    state: axum::extract::State<Arc<Solver>>,
    axum::extract::Json(request): axum::extract::Json<dto::QuoteRequest>,
) -> (axum::http::StatusCode, Json<dto::QuoteResponse>) {
    let span = tracing::info_span!(
        "/mandate/quote",
        endpoint = endpoint::QUOTE,
        chain_id = request.deployment.chain_id,
        liquidity = request.liquidity.len(),
        venues = request.allowed_venues.len(),
        sell_amount = %request.sell_amount,
        result = field::Empty,
        expected_out = field::Empty,
        duration_ms = field::Empty,
    );
    let mut metrics = metrics::mandate_request(endpoint::QUOTE);

    let handle_request = async {
        if let Err(err) = dto::validate_deployment(&request.deployment, &state.mandate_deployment())
        {
            return bad_request(&mut metrics, err);
        }
        if let Err(err) = dto::validate_quote(&request) {
            return bad_request(&mut metrics, err);
        }
        let snapshot = match dto::snapshot_to_domain(&request.liquidity, request.liquidity_source) {
            Ok(value) => value,
            Err(err) => return bad_request(&mut metrics, err),
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

        match &quote {
            Some(quote) => {
                tracing::Span::current().record("expected_out", field::display(quote.expected_out));
                metrics.record(result::FILL);
            }
            None => metrics.record(result::NO_FILL),
        }

        ok(dto::QuoteResponse {
            quote: quote.map(Into::into),
        })
    };

    handle_request.instrument(span).await
}

fn ok<T>(value: T) -> (axum::http::StatusCode, Json<T>) {
    (
        axum::http::StatusCode::OK,
        axum::response::Json(Response::Ok(value)),
    )
}

fn bad_request<T>(
    metrics: &mut MandateRequest,
    err: super::Error,
) -> (axum::http::StatusCode, Json<T>) {
    metrics.record(result::BAD_REQUEST);
    tracing::warn!(?err, "invalid mandate request");
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::response::Json(Response::Err(err)),
    )
}
