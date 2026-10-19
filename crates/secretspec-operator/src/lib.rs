// SPDX-License-Identifier: Apache-2.0

//! The `SecretSpec` operator: configuration, logging, reconcile, controller
//! wiring, leader election and the health/metrics server.

pub mod config;
pub mod controller;
pub mod leader;
pub mod logging;
pub mod metrics;
pub mod outcome;
pub mod reconcile;
pub mod server;
pub mod view;

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::FutureExt;
use kube::Client;
use secretspec_api::constants::GROUP;

use crate::config::Config;
use crate::metrics::Metrics;
use crate::outcome::kube_error_summary;

/// Runs the operator until `shutdown` resolves: the HTTP server, then (after
/// one successful API discovery call) the controllers, under leader election
/// when enabled.
///
/// Readiness (`/readyz`): not ready until discovery succeeds; then ready while
/// standing by as a non-leader, and ready once the controllers are started
/// when leading or when leader election is disabled.
///
/// # Errors
///
/// Fails when the HTTP server cannot start, when leader election has no
/// lease namespace or identity, or when leadership is lost.
pub async fn run(
    config: Config,
    client: Client,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let shutdown = shutdown.boxed().shared();
    let metrics = Arc::new(Metrics::new());
    let ready = Arc::new(AtomicBool::new(false));

    // Checked up front so a misconfiguration fails before anything starts.
    let leadership = if config.leader_election {
        let lease_ns = config
            .lease_namespace
            .clone()
            .filter(|ns| !ns.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("leader election needs POD_NAMESPACE"))?;
        let identity = config.identity().ok_or_else(|| {
            anyhow::anyhow!("leader election needs POD_NAME (no hostname found either)")
        })?;
        Some((lease_ns, identity))
    } else {
        None
    };

    let server = server::serve(
        config.http_bind,
        Arc::clone(&metrics),
        Arc::clone(&ready),
        shutdown.clone(),
    );
    let operator = async {
        let Some(()) = wait_for_discovery(&client, shutdown.clone()).await else {
            return Ok(());
        };
        let Some((lease_ns, identity)) = leadership else {
            return controller::run(config, client, metrics, ready, shutdown.clone()).await;
        };
        let lease_name = config.lease_name.clone();

        // Standing by as a non-leader counts as ready.
        ready.store(true, Ordering::SeqCst);
        let leading = Arc::new(AtomicBool::new(false));
        let work = {
            let leading = Arc::clone(&leading);
            let shutdown = shutdown.clone();
            let client = client.clone();
            async move {
                leading.store(true, Ordering::SeqCst);
                controller::run(config, client, metrics, ready, shutdown).await
            }
        };
        let leadership =
            leader::run_with_leadership(client, &lease_ns, &lease_name, &identity, work);
        // While standing by, shutdown cancels the wait; once leading, the
        // controllers drain and the lease is released by `leadership` itself.
        let standing_by_shutdown = async {
            shutdown.clone().await;
            if leading.load(Ordering::SeqCst) {
                futures::future::pending::<()>().await;
            }
        };
        tokio::select! {
            biased;
            result = leadership => result,
            () = standing_by_shutdown => Ok(()),
        }
    };
    futures::future::try_join(server, operator).await.map(drop)
}

/// Retries discovery of the `SecretSpec` API group (with backoff) until it
/// succeeds; `None` when `shutdown` resolved first.
async fn wait_for_discovery(client: &Client, shutdown: impl Future<Output = ()>) -> Option<()> {
    let discover = async {
        let mut delay = Duration::from_secs(1);
        loop {
            match kube::discovery::group(client, GROUP).await {
                Ok(_) => {
                    tracing::info!(group = GROUP, "API discovery succeeded");
                    return;
                }
                Err(err) => tracing::warn!(
                    group = GROUP,
                    error = %kube_error_summary(&err),
                    retry_in = ?delay,
                    "API discovery failed; is the CRD installed?"
                ),
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(30));
        }
    };
    tokio::select! {
        () = discover => Some(()),
        () = shutdown => None,
    }
}

/// Resolves on SIGTERM or SIGINT (Ctrl+C).
pub async fn shutdown_signal() {
    let interrupt = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "cannot listen for SIGINT");
            futures::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "cannot listen for SIGTERM");
                futures::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = futures::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutdown requested");
}
