// SPDX-License-Identifier: Apache-2.0

//! `SecretSpec` operator entry point.

use clap::Parser;
use secretspec_operator::config::Config;
use secretspec_operator::logging::init_logging;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::parse();
    config.validate()?;
    init_logging(&config.log_level, config.log_format)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        ?config,
        "secretspec-operator starting"
    );
    let client = kube::Client::try_default()
        .await
        .map_err(|e| anyhow::anyhow!("cannot build the Kubernetes client: {e}"))?;
    secretspec_operator::run(config, client, secretspec_operator::shutdown_signal()).await
}
