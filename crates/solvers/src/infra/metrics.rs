use {
    crate::domain::{auction, solution},
    std::time::Instant,
};

/// The Mandate endpoint a metric belongs to. Bounded by construction: these are
/// the only values ever used as an `endpoint` label.
pub mod endpoint {
    pub const SOLVE: &str = "solve";
    pub const QUOTE: &str = "quote";
    /// A middleware rejection on a path that is not a Mandate route. Cannot
    /// happen with the current routing, but keeps the label set closed.
    pub const UNKNOWN: &str = "unknown";
}

/// How a Mandate request ended. Shared by the metrics `result` label and the
/// span field of the same name, so a trace and a counter agree on the wording.
pub mod result {
    pub const FILL: &str = "fill";
    pub const NO_FILL: &str = "no_fill";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const OVERLOADED: &str = "overloaded";
    pub const TIMEOUT: &str = "timeout";
    pub const INTERNAL_ERROR: &str = "internal_error";
}

/// Metrics for the solver engine.
#[derive(Debug, Clone, prometheus_metric_storage::MetricStorage)]
#[metric(subsystem = "solver_engine")]
struct Metrics {
    /// The amount of time this solver engine has for solving.
    #[metric(buckets(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15))]
    time_limit: prometheus::Histogram,

    /// The amount of time this solver engine has left when it finished solving.
    #[metric(buckets(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15))]
    remaining_time: prometheus::Histogram,

    /// Errors that occurred during solving.
    #[metric(labels("reason"))]
    solve_errors: prometheus::IntCounterVec,

    /// The number of solutions that were found.
    solutions: prometheus::IntCounter,

    /// Mandate requests by endpoint and outcome.
    #[metric(labels("endpoint", "result"))]
    mandate_requests_total: prometheus::IntCounterVec,

    /// How long a Mandate request spent in its handler, including requests
    /// abandoned at their deadline.
    #[metric(
        labels("endpoint"),
        buckets(0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0)
    )]
    mandate_duration_seconds: prometheus::HistogramVec,

    /// Mandate requests currently in their handler.
    #[metric(labels("endpoint"))]
    mandate_requests_inflight: prometheus::IntGaugeVec,
}

/// Tracks one Mandate request through its handler.
///
/// The counters live in `Drop` rather than at the return sites so that a
/// request cancelled at its deadline — which never returns — still releases the
/// in-flight gauge and reports its duration.
pub struct MandateRequest {
    endpoint: &'static str,
    start: Instant,
    result: Option<&'static str>,
}

/// Starts tracking a Mandate request. Call once per handler invocation.
pub fn mandate_request(endpoint: &'static str) -> MandateRequest {
    get()
        .mandate_requests_inflight
        .with_label_values(&[endpoint])
        .inc();
    MandateRequest {
        endpoint,
        start: Instant::now(),
        result: None,
    }
}

impl MandateRequest {
    /// Records the outcome, on both the request metrics and the current span.
    /// A request that never reaches this — because it was shed or timed out —
    /// is counted by [`mandate_rejected`] instead, so outcomes are counted once.
    pub fn record(&mut self, result: &'static str) {
        self.result = Some(result);
        let span = tracing::Span::current();
        span.record("result", result);
        span.record("duration_ms", self.start.elapsed().as_millis());
    }
}

impl Drop for MandateRequest {
    fn drop(&mut self) {
        get()
            .mandate_duration_seconds
            .with_label_values(&[self.endpoint])
            .observe(self.start.elapsed().as_secs_f64());
        get()
            .mandate_requests_inflight
            .with_label_values(&[self.endpoint])
            .dec();
        if let Some(result) = self.result {
            get()
                .mandate_requests_total
                .with_label_values(&[self.endpoint, result])
                .inc();
        }
    }
}

/// Counts a Mandate request rejected by the middleware, which never reaches a
/// handler and so has no [`MandateRequest`] of its own.
pub fn mandate_rejected(endpoint: &'static str, result: &'static str) {
    get()
        .mandate_requests_total
        .with_label_values(&[endpoint, result])
        .inc();
}

/// Setup the metrics registry.
pub fn init() {
    observe::metrics::setup_registry_reentrant(Some("solver-engine".to_owned()), None);
}

pub fn solve(auction: &auction::Auction) {
    get().time_limit.observe(
        auction
            .deadline
            .remaining()
            .unwrap_or_default()
            .as_secs_f64(),
    );
}

pub fn solved(deadline: &auction::Deadline, solutions: &[solution::Solution]) {
    get()
        .remaining_time
        .observe(deadline.remaining().unwrap_or_default().as_secs_f64());
    get().solutions.inc_by(solutions.len() as u64);
}

/// Get the metrics instance.
fn get() -> &'static Metrics {
    Metrics::instance(observe::metrics::get_storage_registry())
        .expect("unexpected error getting metrics instance")
}
