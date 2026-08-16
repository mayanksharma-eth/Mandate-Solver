//! CLI arguments for the `solvers` binary.

use {
    clap::{Parser, Subcommand},
    std::{net::SocketAddr, path::PathBuf, time::Duration},
};

/// Run a solver engine
#[derive(Parser, Debug)]
#[command(version)]
pub struct Args {
    /// The log filter.
    #[arg(
        long,
        env,
        default_value = "warn,solvers=debug,shared=debug,model=debug,solver=debug"
    )]
    pub log: String,

    /// Whether to use JSON format for the logs.
    #[clap(long, env, default_value = "false")]
    pub use_json_logs: bool,

    /// The socket address to bind to.
    #[arg(long, env, default_value = "127.0.0.1:7872")]
    pub addr: SocketAddr,

    /// How many `/mandate/*` requests may be solved at the same time. Requests
    /// arriving beyond this limit are shed with a 503 instead of queueing.
    #[arg(long, env, default_value = "32")]
    pub max_concurrent_requests: usize,

    /// How long a `/mandate/*` request may take before it is abandoned with a
    /// 504.
    #[arg(long, env, default_value = "10s", value_parser = humantime::parse_duration)]
    pub request_timeout: Duration,

    #[command(subcommand)]
    pub command: Command,
}

/// The solver engine to run. The config field is a path to the solver
/// configuration file. This file should be in TOML format.
#[derive(Subcommand, Debug)]
#[clap(rename_all = "lowercase")]
pub enum Command {
    /// solve individual orders exclusively via provided onchain liquidity
    Baseline {
        #[clap(long, env)]
        config: PathBuf,
    },
}
