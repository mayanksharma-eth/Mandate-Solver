//! Solver engine end-to-end tests.
//!
//! Note that this is setup as a "unit test" in that it is part of the `src/`
//! directory. This is done intentionally as Cargo builds separate binaries for
//! each file in `tests/`, which makes `cargo test` slower.

use {
    reqwest::Url,
    std::{io::Write, path::PathBuf},
    tokio::{sync::oneshot, task::JoinHandle},
};

mod cases;
pub mod gate;

/// A solver engine handle for E2E testing.
pub struct SolverEngine {
    url: Url,
    tempfile: Option<tempfile::TempPath>,
    handle: JoinHandle<()>,
}

/// Solver configuration.
pub enum Config {
    None,
    File(PathBuf),
    String(String),
}

impl SolverEngine {
    /// Creates a new solver engine handle for the specified command
    /// configuration.
    pub async fn new(command: &str, config: Config) -> Self {
        Self::with_args(command, config, &[]).await
    }

    /// Same, with additional command line arguments for the engine.
    pub async fn with_args(command: &str, config: Config, extra: &[&str]) -> Self {
        let (bind, bind_receiver) = oneshot::channel();

        let mut args = vec![
            "/test/solvers/path".to_owned(),
            "--addr=0.0.0.0:0".to_owned(),
            "--log=solvers=trace".to_owned(),
        ];
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        args.push(command.to_owned());
        let tempfile = match config {
            Config::None => None,
            Config::File(path) => {
                args.push(format!("--config={}", path.display()));
                None
            }
            Config::String(config) => {
                let mut file = tempfile::NamedTempFile::new().unwrap();
                file.write_all(config.as_bytes()).unwrap();
                let path = file.into_temp_path();
                args.push(format!("--config={}", path.display()));
                Some(path)
            }
        };

        let handle = tokio::spawn(crate::run(args, Some(bind)));

        let addr = bind_receiver.await.unwrap();
        let url = format!("http://{addr}/").parse().unwrap();

        Self {
            url,
            tempfile,
            handle,
        }
    }

    /// Solves a raw JSON auction.
    pub async fn solve(&self, auction: serde_json::Value) -> serde_json::Value {
        self.post("solve", auction).await
    }

    /// Posts a raw JSON body to an engine endpoint.
    pub async fn post(&self, path: &str, body: serde_json::Value) -> serde_json::Value {
        let client = reqwest::Client::new();
        let url = shared::url::join(&self.url, path);
        let response = client.post(url).json(&body).send().await.unwrap();

        if !response.status().is_success() {
            panic!(
                "HTTP {}: {:?}",
                response.status(),
                response.text().await.unwrap(),
            );
        }

        response.json().await.unwrap()
    }

    /// Posts a raw JSON body, returning the status instead of panicking on
    /// errors.
    pub async fn post_status(&self, path: &str, body: serde_json::Value) -> reqwest::StatusCode {
        self.post_response(path, body).await.status()
    }

    /// Fetches the engine's Prometheus metrics.
    pub async fn metrics(&self) -> String {
        let url = shared::url::join(&self.url, "metrics");
        reqwest::get(url).await.unwrap().text().await.unwrap()
    }

    /// Posts a raw JSON body, returning the whole response.
    pub async fn post_response(&self, path: &str, body: serde_json::Value) -> reqwest::Response {
        let client = reqwest::Client::new();
        let url = shared::url::join(&self.url, path);
        client.post(url).json(&body).send().await.unwrap()
    }
}

impl Drop for SolverEngine {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
