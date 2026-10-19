// SPDX-License-Identifier: Apache-2.0

//! Controller wiring: one kube `Controller` per watched scope (all namespaces,
//! or one per listed namespace), sharing a single reconcile context.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::{FutureExt, StreamExt};
use k8s_openapi::api::core::v1::Secret;
use kube::api::Api;
use kube::runtime::controller::{self, Controller};
use kube::runtime::events::{Recorder, Reporter};
use kube::runtime::reflector::{self, ObjectRef};
use kube::runtime::utils::PredicateConfig;
use kube::runtime::{WatchStreamExt, predicates, watcher};
use kube::{Client, ResourceExt};
use secretspec_api::constants::{
    FIELD_MANAGER, LABEL_MANAGED_BY, LABEL_SPEC_NAME, MANAGED_BY_VALUE,
};
use secretspec_api::v1alpha1::SecretSpec;

use crate::config::Config;
use crate::metrics::Metrics;
use crate::outcome::kube_error_summary;
use crate::reconcile::{Ctx, Error, error_policy, reconcile};

/// Runs the controllers until `shutdown` resolves, then waits for in-flight
/// reconciles to finish. Sets `ready` once every controller is started.
///
/// With `watch_namespaces()` empty there is one cluster-wide controller;
/// otherwise one controller per listed namespace, and no request in this
/// module is cluster-wide (`Api::all` is never used in that mode).
///
/// # Errors
///
/// Currently never fails; controller errors are logged and retried.
pub async fn run(
    cfg: Config,
    client: Client,
    metrics: Arc<Metrics>,
    ready: Arc<AtomicBool>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let shutdown = shutdown.boxed().shared();
    let recorder = Recorder::new(
        client.clone(),
        Reporter {
            controller: FIELD_MANAGER.to_owned(),
            instance: cfg.identity(),
        },
    );
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        recorder,
        resync: cfg.resync_interval,
        metrics: Arc::clone(&metrics),
    });

    let namespaces = cfg.watch_namespaces();
    let scopes: Vec<(Api<SecretSpec>, Api<Secret>)> = if namespaces.is_empty() {
        vec![(Api::all(client.clone()), Api::all(client))]
    } else {
        namespaces
            .iter()
            .map(|ns| {
                (
                    Api::namespaced(client.clone(), ns),
                    Api::namespaced(client.clone(), ns),
                )
            })
            .collect()
    };

    let runs: Vec<_> = scopes
        .into_iter()
        .map(|(specs, secrets)| {
            let controller =
                build(specs, secrets, cfg.concurrency).graceful_shutdown_on(shutdown.clone());
            metrics.register_store(controller.store());
            controller
                .run(reconcile, error_policy, Arc::clone(&ctx))
                .for_each(|result| {
                    log_result(result);
                    std::future::ready(())
                })
        })
        .collect();

    tracing::info!(
        scope = if namespaces.is_empty() { "cluster" } else { "namespaces" },
        namespaces = ?namespaces,
        "controllers started"
    );
    ready.store(true, Ordering::SeqCst);
    futures::future::join_all(runs).await;
    tracing::info!("controllers stopped");
    Ok(())
}

/// One controller for the given scope.
fn build(specs: Api<SecretSpec>, secrets: Api<Secret>, concurrency: u16) -> Controller<SecretSpec> {
    let (reader, writer) = reflector::store();
    // The reflector sees every event (so `Controller::store()` and the metrics
    // built from it drop deleted SecretSpecs); only the trigger stream is
    // filtered. The generation predicate means status writes do not retrigger
    // a reconcile. It can also suppress delete events, which is fine: deleting
    // a SecretSpec needs no reconcile (the Secret is garbage-collected under
    // `Delete` or deliberately kept under `Retain`).
    let trigger = watcher(specs, watcher::Config::default())
        .default_backoff()
        .reflect(writer)
        .applied_objects()
        .predicate_filter(predicates::generation, PredicateConfig::default());
    let selector = format!("{LABEL_MANAGED_BY}={MANAGED_BY_VALUE}");
    Controller::for_stream(trigger, reader)
        .watches(
            secrets,
            watcher::Config::default().labels(&selector),
            secret_to_spec,
        )
        .with_config(controller::Config::default().concurrency(concurrency))
}

/// The `SecretSpec` that owns a managed Secret: same namespace, name from the
/// spec-name label. `None` when the label (or the namespace) is missing.
#[allow(
    clippy::needless_pass_by_value,
    reason = "signature required by kube's Controller::watches mapper"
)]
pub fn secret_to_spec(secret: Secret) -> Option<ObjectRef<SecretSpec>> {
    let namespace = secret.namespace()?;
    let name = secret.labels().get(LABEL_SPEC_NAME)?;
    if name.is_empty() {
        return None;
    }
    Some(ObjectRef::new(name).within(&namespace))
}

/// Logs one controller result without Secret data or kube error bodies.
fn log_result(
    result: Result<
        (ObjectRef<SecretSpec>, controller::Action),
        controller::Error<Error, watcher::Error>,
    >,
) {
    match result {
        Ok((obj, _)) => tracing::trace!(
            namespace = obj.namespace.as_deref().unwrap_or_default(),
            name = %obj.name,
            "reconciled"
        ),
        // `error_policy` already logged it.
        Err(controller::Error::ReconcilerFailed(..)) => {}
        Err(controller::Error::ObjectNotFound(obj)) => tracing::debug!(
            namespace = obj.namespace.as_deref().unwrap_or_default(),
            name = %obj.name,
            "object no longer exists"
        ),
        Err(controller::Error::QueueError(err)) => {
            tracing::warn!(error = %watcher_error_summary(&err), "watch failed; retrying");
        }
        Err(controller::Error::RunnerError(err)) => {
            tracing::error!(error = %err, "controller runner failed");
        }
    }
}

/// A watcher error reduced to its kind and the HTTP code and reason.
fn watcher_error_summary(err: &watcher::Error) -> String {
    match err {
        watcher::Error::InitialListFailed(e) => format!("initial list: {}", kube_error_summary(e)),
        watcher::Error::WatchStartFailed(e) => format!("watch start: {}", kube_error_summary(e)),
        watcher::Error::WatchFailed(e) => format!("watch: {}", kube_error_summary(e)),
        watcher::Error::WatchError(status) => format!("watch: {} {}", status.code, status.reason),
        watcher::Error::NoResourceVersion => "no resourceVersion in watch result".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use kube::api::ObjectMeta;

    use super::*;

    fn secret(namespace: Option<&str>, labels: &[(&str, &str)]) -> Secret {
        Secret {
            metadata: ObjectMeta {
                name: Some("db-credentials".into()),
                namespace: namespace.map(str::to_owned),
                labels: Some(
                    labels
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect::<BTreeMap<_, _>>(),
                ),
                ..ObjectMeta::default()
            },
            ..Secret::default()
        }
    }

    #[test]
    fn secret_to_spec_uses_label() {
        let s = secret(
            Some("apps"),
            &[
                (LABEL_SPEC_NAME, "db"),
                (LABEL_MANAGED_BY, MANAGED_BY_VALUE),
            ],
        );
        let obj = secret_to_spec(s).unwrap();
        assert_eq!(obj.name, "db");
        assert_eq!(obj.namespace.as_deref(), Some("apps"));

        assert_eq!(
            secret_to_spec(secret(
                Some("apps"),
                &[(LABEL_MANAGED_BY, MANAGED_BY_VALUE)]
            )),
            None
        );
        assert_eq!(
            secret_to_spec(secret(Some("apps"), &[(LABEL_SPEC_NAME, "")])),
            None
        );
        assert_eq!(
            secret_to_spec(secret(None, &[(LABEL_SPEC_NAME, "db")])),
            None
        );
    }
}
