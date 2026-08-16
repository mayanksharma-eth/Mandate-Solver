//! Serve a solver engine API.

use {
    crate::{domain::solver::Solver, infra::metrics},
    axum::response::IntoResponse,
    observe::distributed_tracing::tracing_axum::{make_span, record_trace_id},
    std::{future::Future, net::SocketAddr, sync::Arc, time::Duration},
    tokio::sync::oneshot,
    tower::{BoxError, limit::GlobalConcurrencyLimitLayer},
};

mod routes;

const REQUEST_BODY_LIMIT: usize = 10 * 1024 * 1024;

pub struct Api {
    pub addr: SocketAddr,
    pub solver: Solver,
    /// The number of `/mandate/*` requests that may be in flight at once.
    pub max_concurrent_requests: usize,
    /// The deadline for a single `/mandate/*` request.
    pub request_timeout: Duration,
}

impl Api {
    pub async fn serve(
        self,
        bind: Option<oneshot::Sender<SocketAddr>>,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), hyper::Error> {
        // Outermost first: errors from the layers below are turned into
        // responses, requests beyond the concurrency limit are shed rather than
        // queued, and only a request holding a permit gets to start the clock on
        // its own deadline. Cloning the stack for the second route shares the
        // semaphore, so the limit covers both Mandate routes together.
        let mandate = tower::ServiceBuilder::new()
            .layer(axum::error_handling::HandleErrorLayer::new(handle_error))
            .load_shed()
            .layer(GlobalConcurrencyLimitLayer::new(
                self.max_concurrent_requests,
            ))
            .timeout(self.request_timeout);

        let app = axum::Router::new()
            .route("/metrics", axum::routing::get(routes::metrics))
            .route("/healthz", axum::routing::get(routes::healthz))
            .route("/solve", axum::routing::post(routes::solve))
            .route(
                "/mandate/solve",
                axum::routing::post(routes::mandate::solve).layer(mandate.clone()),
            )
            .route(
                "/mandate/quote",
                axum::routing::post(routes::mandate::quote).layer(mandate),
            )
            .layer(
                tower::ServiceBuilder::new()
                    .layer(tower_http::trace::TraceLayer::new_for_http().make_span_with(make_span))
                    .map_request(record_trace_id),
            )
            .with_state(Arc::new(self.solver))
            .layer(tower_http::limit::RequestBodyLimitLayer::new(
                REQUEST_BODY_LIMIT,
            ))
            // axum's default body limit needs to be disabled to not have the default limit on top of our custom limit
            .layer(axum::extract::DefaultBodyLimit::disable());

        let server = axum::Server::bind(&self.addr).serve(app.into_make_service());
        if let Some(bind) = bind {
            let _ = bind.send(server.local_addr());
        }

        server.with_graceful_shutdown(shutdown).await
    }
}

/// Turns middleware errors into the same JSON error shape the handlers use, so
/// a client parses one body format regardless of which layer rejected it.
///
/// These requests never reach a handler, so this is the only place their
/// outcome can be counted.
async fn handle_error(uri: axum::http::Uri, err: BoxError) -> axum::response::Response {
    let endpoint = match uri.path() {
        "/mandate/solve" => metrics::endpoint::SOLVE,
        "/mandate/quote" => metrics::endpoint::QUOTE,
        _ => metrics::endpoint::UNKNOWN,
    };

    if err.is::<tower::load_shed::error::Overloaded>() {
        metrics::mandate_rejected(endpoint, metrics::result::OVERLOADED);
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::RETRY_AFTER, "1")],
            error("solver is at capacity"),
        )
            .into_response();
    }
    if err.is::<tower::timeout::error::Elapsed>() {
        metrics::mandate_rejected(endpoint, metrics::result::TIMEOUT);
        return (
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            error("solving exceeded the request deadline"),
        )
            .into_response();
    }
    metrics::mandate_rejected(endpoint, metrics::result::INTERNAL_ERROR);
    tracing::error!(?err, "unexpected error in the mandate middleware");
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        error("internal error"),
    )
        .into_response()
}

fn error(message: &'static str) -> axum::response::Json<routes::Response<()>> {
    axum::response::Json(routes::Response::Err(message.into()))
}
