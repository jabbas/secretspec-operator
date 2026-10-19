// SPDX-License-Identifier: Apache-2.0

//! One reconcile of a `SecretSpec`: read the Secret, ask the pure planner what
//! to do, server-side apply the result, write status and publish events.
//!
//! Nothing here logs, events or returns a `Secret`, its data, the apply body or
//! a kube error body; kube errors are reduced to code and reason.

use std::sync::Arc;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use k8s_openapi::api::core::v1::{ObjectReference, Secret};
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType, Recorder};
use kube::{Client, Resource};
use secretspec_api::constants::{CONDITION_READY, FIELD_MANAGER};
use secretspec_api::v1alpha1::{SecretSpec, SecretSpecStatus};
use secretspec_core::generate::os_seeded_rng;
use secretspec_core::{PlannedEvent, SecretAction, plan};
use serde_json::{Map, Value};

use crate::metrics::Metrics;
use crate::outcome::{
    apply_failed_message, apply_failed_status, events_after_apply_failure, kube_error_summary,
    requeue_after,
};
use crate::view::owned_view;

/// Requeue delay while the Secret is being deleted.
const SECRET_DELETING_REQUEUE: Duration = Duration::from_secs(5);

/// Requeue delay after a failed reconcile.
const ERROR_REQUEUE: Duration = Duration::from_secs(30);

/// The `action` of every published event.
const EVENT_ACTION: &str = "Reconcile";

/// Every field of the serialized [`SecretSpecStatus`]. Absent fields are sent
/// as `null` in the status merge patch so that a field the planner dropped
/// (for example `nextRotationTime` after `spec.rotation` is removed) is
/// removed from the object instead of kept.
const STATUS_FIELDS: [&str; 7] = [
    "observedGeneration",
    "kind",
    "secretName",
    "lastGeneratedTime",
    "lastRotationTime",
    "nextRotationTime",
    "conditions",
];

/// Shared state for all reconciles.
pub struct Ctx {
    /// Client for the Secret, status and event requests.
    pub client: Client,
    /// Publishes events on `SecretSpec`s (events.k8s.io).
    pub recorder: Recorder,
    /// Upper bound for the requeue delay.
    pub resync: Duration,
    /// Reconcile and generation metrics.
    pub metrics: Arc<Metrics>,
}

/// Why a reconcile failed. Every message is free of Secret data and kube error
/// bodies.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A Kubernetes request other than the Secret apply failed; holds the
    /// request, HTTP code and reason only.
    #[error("kubernetes API error: {0}")]
    Kube(String),
    /// The operating system's random number generator failed.
    #[error("random number generator unavailable")]
    Rng,
    /// The blocking planner task panicked or was cancelled.
    #[error("planner task failed")]
    Join,
    /// Applying the Secret failed; the message is the `ApplyFailed` condition
    /// message.
    #[error("{0}")]
    ApplyFailed(String),
}

/// Reconciles one `SecretSpec`; see the module documentation.
///
/// # Errors
///
/// [`Error::Kube`] when reading the Secret or writing status fails,
/// [`Error::Rng`] / [`Error::Join`] when planning cannot run, and
/// [`Error::ApplyFailed`] when the Secret apply is rejected (status and the
/// `ApplyFailed` event are written first).
pub async fn reconcile(spec: Arc<SecretSpec>, ctx: Arc<Ctx>) -> Result<Action, Error> {
    let started = Instant::now();
    let result = reconcile_inner(&spec, &ctx, started).await;
    ctx.metrics
        .record_reconcile(result.is_ok(), started.elapsed());
    result
}

/// Requeues a failed reconcile after 30 seconds and logs why.
#[allow(
    clippy::needless_pass_by_value,
    reason = "signature required by kube's Controller::run"
)]
pub fn error_policy(spec: Arc<SecretSpec>, err: &Error, _ctx: Arc<Ctx>) -> Action {
    tracing::warn!(
        namespace = spec.metadata.namespace.as_deref().unwrap_or_default(),
        name = spec.metadata.name.as_deref().unwrap_or_default(),
        error = %err,
        "reconcile failed; retrying"
    );
    Action::requeue(ERROR_REQUEUE)
}

async fn reconcile_inner(
    spec: &Arc<SecretSpec>,
    ctx: &Ctx,
    started: Instant,
) -> Result<Action, Error> {
    if spec.metadata.deletion_timestamp.is_some() {
        return Ok(Action::await_change());
    }
    let (Some(namespace), Some(name)) = (
        spec.metadata.namespace.as_deref(),
        spec.metadata.name.as_deref(),
    ) else {
        tracing::warn!("SecretSpec without namespace or name; ignoring");
        return Ok(Action::await_change());
    };

    // Direct read, not a cache: right after our own apply a cache may still
    // hold the previous Secret.
    let secrets: Api<Secret> = Api::namespaced(ctx.client.clone(), namespace);
    let existing = secrets
        .get_opt(name)
        .await
        .map_err(|err| Error::Kube(format!("get Secret: {}", kube_error_summary(&err))))?;
    if existing
        .as_ref()
        .is_some_and(|secret| secret.metadata.deletion_timestamp.is_some())
    {
        tracing::info!(namespace, name, "Secret is being deleted; requeueing");
        return Ok(Action::requeue(SECRET_DELETING_REQUEUE));
    }
    let view = existing.map(|secret| owned_view(&secret, spec));

    let mut rng = os_seeded_rng().map_err(|_| Error::Rng)?;
    let planned_spec = Arc::clone(spec);
    let plan = tokio::task::spawn_blocking(move || {
        plan(&planned_spec, view.as_ref(), Timestamp::now(), &mut rng)
    })
    .await
    .map_err(|_| Error::Join)?;

    let reference = spec.object_ref(&());
    let action = action_name(&plan.action);

    if let SecretAction::Apply(secret) = plan.action {
        let applied = secrets
            .patch(
                name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&*secret),
            )
            .await;
        drop(secret);
        if let Err(err) = applied {
            let message = apply_failed_message(&err);
            drop(err);
            let status = apply_failed_status(
                spec.status.as_ref(),
                spec.metadata.generation,
                &message,
                Timestamp::now(),
            );
            if let Err(status_err) = write_status(ctx, spec, namespace, name, &status).await {
                tracing::warn!(namespace, name, error = %status_err, "writing ApplyFailed status failed");
            }
            let events = events_after_apply_failure(&plan.events, &message);
            publish(ctx, &reference, namespace, name, &events).await;
            log_reconcile(namespace, name, action, &status, started);
            return Err(Error::ApplyFailed(message));
        }
    }

    let written = write_status(ctx, spec, namespace, name, &plan.status).await;
    // The Secret write (if any) has happened, so its events are true even if
    // the status write failed.
    publish(ctx, &reference, namespace, name, &plan.events).await;
    log_reconcile(namespace, name, action, &plan.status, started);
    written?;
    Ok(Action::requeue(requeue_after(
        plan.requeue_after,
        ctx.resync,
    )))
}

/// Writes `desired` to the status subresource unless it already matches.
async fn write_status(
    ctx: &Ctx,
    spec: &SecretSpec,
    namespace: &str,
    name: &str,
    desired: &SecretSpecStatus,
) -> Result<(), Error> {
    if !status_patch_needed(spec.status.as_ref(), desired) {
        tracing::debug!(namespace, name, "status unchanged; not patching");
        return Ok(());
    }
    let patch = status_merge_patch(desired)?;
    let specs: Api<SecretSpec> = Api::namespaced(ctx.client.clone(), namespace);
    specs
        .patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await
        .map_err(|err| Error::Kube(format!("patch status: {}", kube_error_summary(&err))))?;
    Ok(())
}

/// Publishes `events` on the `SecretSpec` and counts generations. A failed
/// publish is logged and otherwise ignored.
async fn publish(
    ctx: &Ctx,
    reference: &ObjectReference,
    namespace: &str,
    name: &str,
    events: &[PlannedEvent],
) {
    for planned in events {
        ctx.metrics.record_event(planned.reason);
        if ctx
            .recorder
            .publish(&planned_to_event(planned), reference)
            .await
            .is_err()
        {
            tracing::warn!(
                namespace,
                name,
                reason = planned.reason,
                "publishing event failed"
            );
        }
    }
}

fn log_reconcile(
    namespace: &str,
    name: &str,
    action: &'static str,
    status: &SecretSpecStatus,
    started: Instant,
) {
    tracing::info!(
        namespace,
        name,
        action,
        ready_reason = ready_reason(status).unwrap_or_default(),
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "reconciled"
    );
}

/// The kube event for a planned event.
#[must_use]
pub fn planned_to_event(planned: &PlannedEvent) -> Event {
    Event {
        type_: if planned.warning {
            EventType::Warning
        } else {
            EventType::Normal
        },
        reason: planned.reason.to_owned(),
        note: Some(planned.message.clone()),
        action: EVENT_ACTION.to_owned(),
        secondary: None,
    }
}

/// Whether the status subresource must be patched to reach `desired`.
#[must_use]
pub fn status_patch_needed(current: Option<&SecretSpecStatus>, desired: &SecretSpecStatus) -> bool {
    current != Some(desired)
}

/// The merge patch `{"status": …}` that sets the status to exactly `desired`:
/// fields absent from `desired` are `null`, so they are removed.
///
/// # Errors
///
/// [`Error::Kube`] if the status cannot be serialized (never expected).
pub fn status_merge_patch(desired: &SecretSpecStatus) -> Result<Value, Error> {
    let Value::Object(mut fields) = serde_json::to_value(desired)
        .map_err(|_| Error::Kube("status could not be serialized".to_owned()))?
    else {
        return Err(Error::Kube("status is not an object".to_owned()));
    };
    for field in STATUS_FIELDS {
        fields.entry(field).or_insert(Value::Null);
    }
    let mut patch = Map::new();
    patch.insert("status".to_owned(), Value::Object(fields));
    Ok(Value::Object(patch))
}

/// The variant name of `action`, for logs.
fn action_name(action: &SecretAction) -> &'static str {
    match action {
        SecretAction::Apply(_) => "Apply",
        SecretAction::UpToDate => "UpToDate",
        SecretAction::Nothing => "Nothing",
    }
}

/// The reason of the `Ready` condition, if any.
fn ready_reason(status: &SecretSpecStatus) -> Option<&str> {
    status
        .conditions
        .iter()
        .find(|condition| condition.type_ == CONDITION_READY)
        .map(|condition| condition.reason.as_str())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
    use secretspec_api::constants::{EVENT_GENERATED, EVENT_LOW_ENTROPY, REASON_GENERATED};
    use secretspec_api::v1alpha1::SecretKind;

    use super::*;

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    fn full_status() -> SecretSpecStatus {
        SecretSpecStatus {
            observed_generation: Some(2),
            kind: Some(SecretKind::Fields),
            secret_name: Some("db".to_owned()),
            last_generated_time: Some(Time(ts("2026-01-01T00:00:00Z"))),
            last_rotation_time: Some(Time(ts("2026-01-02T00:00:00Z"))),
            next_rotation_time: Some(Time(ts("2026-01-03T00:00:00Z"))),
            conditions: vec![Condition {
                type_: CONDITION_READY.to_owned(),
                status: "True".to_owned(),
                reason: REASON_GENERATED.to_owned(),
                message: "ok".to_owned(),
                observed_generation: Some(2),
                last_transition_time: Time(ts("2026-01-01T00:00:00Z")),
            }],
        }
    }

    #[test]
    fn planned_to_event_maps_type_reason_and_note() {
        let normal = planned_to_event(&PlannedEvent {
            warning: false,
            reason: EVENT_GENERATED,
            message: "generated password".to_owned(),
        });
        assert_eq!(normal.type_, EventType::Normal);
        assert_eq!(normal.reason, EVENT_GENERATED);
        assert_eq!(normal.note.as_deref(), Some("generated password"));
        assert_eq!(normal.action, "Reconcile");
        assert!(normal.secondary.is_none());

        let warning = planned_to_event(&PlannedEvent {
            warning: true,
            reason: EVENT_LOW_ENTROPY,
            message: "weak".to_owned(),
        });
        assert_eq!(warning.type_, EventType::Warning);
        assert_eq!(warning.reason, EVENT_LOW_ENTROPY);
    }

    #[test]
    fn status_patch_needed_only_on_difference() {
        let status = full_status();
        assert!(!status_patch_needed(Some(&status), &status.clone()));
        assert!(status_patch_needed(None, &status));
        let mut changed = status.clone();
        changed.observed_generation = Some(3);
        assert!(status_patch_needed(Some(&status), &changed));
        let mut dropped = status.clone();
        dropped.next_rotation_time = None;
        assert!(status_patch_needed(Some(&status), &dropped));
    }

    #[test]
    fn status_merge_patch_nulls_absent_fields() {
        let patch = status_merge_patch(&SecretSpecStatus::default()).unwrap();
        let status = patch["status"].as_object().unwrap();
        assert_eq!(status.len(), STATUS_FIELDS.len());
        assert!(status.values().all(Value::is_null), "{patch}");
    }

    #[test]
    fn status_merge_patch_covers_every_field() {
        let patch = status_merge_patch(&full_status()).unwrap();
        let status = patch["status"].as_object().unwrap();
        // Every serialized field is one of STATUS_FIELDS: no field can be
        // missed by the null-filling.
        assert_eq!(status.len(), STATUS_FIELDS.len(), "{patch}");
        assert!(status.values().all(|value| !value.is_null()), "{patch}");
        assert_eq!(status["observedGeneration"], 2);
        assert_eq!(status["nextRotationTime"], "2026-01-03T00:00:00Z");
    }

    #[test]
    fn action_names() {
        assert_eq!(action_name(&SecretAction::UpToDate), "UpToDate");
        assert_eq!(action_name(&SecretAction::Nothing), "Nothing");
        assert_eq!(action_name(&SecretAction::Apply(Box::default())), "Apply");
    }

    #[test]
    fn ready_reason_reads_ready_condition() {
        assert_eq!(ready_reason(&full_status()), Some(REASON_GENERATED));
        assert_eq!(ready_reason(&SecretSpecStatus::default()), None);
    }
}
