// SPDX-License-Identifier: Apache-2.0

//! What the reconciler reports when applying the Secret fails, and how soon it
//! looks again.

use std::time::Duration;

use jiff::Timestamp;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use secretspec_api::constants::{
    CONDITION_READY, EVENT_ANCHOR_RESET, EVENT_APPLY_FAILED, EVENT_FINGERPRINTS_RESET,
    REASON_APPLY_FAILED,
};
use secretspec_api::v1alpha1::SecretSpecStatus;
use secretspec_core::PlannedEvent;

/// Shortest requeue delay, so a zero or past planned time cannot spin.
const MIN_REQUEUE: Duration = Duration::from_secs(1);

/// The status to write after applying the Secret failed.
///
/// Everything of the `previous` status is kept (including `observedGeneration`
/// and all times) except the `Ready` condition, which becomes `False` /
/// `ApplyFailed` with `message`. Its `lastTransitionTime` is kept when `Ready`
/// was already `False`/`ApplyFailed`, otherwise it is `now`. The condition's
/// `observedGeneration` is `generation`.
#[must_use]
pub fn apply_failed_status(
    previous: Option<&SecretSpecStatus>,
    generation: Option<i64>,
    message: &str,
    now: Timestamp,
) -> SecretSpecStatus {
    let mut status = previous.cloned().unwrap_or_default();
    let last_transition_time = status
        .conditions
        .iter()
        .find(|condition| condition.type_ == CONDITION_READY)
        .filter(|condition| condition.status == "False" && condition.reason == REASON_APPLY_FAILED)
        .map_or(Time(now), |condition| {
            condition.last_transition_time.clone()
        });
    status
        .conditions
        .retain(|condition| condition.type_ != CONDITION_READY);
    status.conditions.insert(
        0,
        Condition {
            type_: CONDITION_READY.to_owned(),
            status: "False".to_owned(),
            reason: REASON_APPLY_FAILED.to_owned(),
            message: message.to_owned(),
            observed_generation: generation,
            last_transition_time,
        },
    );
    status
}

/// The events to publish after a failed apply.
///
/// Planned Warning events are kept, except `FingerprintsReset` and `AnchorReset`
/// (they describe state that was not persisted, and will be planned again on
/// the retry). Normal events (`Generated`, `Regenerated`, `Rotated`, `Adopted`)
/// are dropped because they did not happen. One Warning `ApplyFailed` with
/// `message` is added. After a successful apply or no-op the caller publishes
/// the planned events unchanged.
#[must_use]
pub fn events_after_apply_failure(planned: &[PlannedEvent], message: &str) -> Vec<PlannedEvent> {
    let mut events: Vec<PlannedEvent> = planned
        .iter()
        .filter(|event| {
            event.warning
                && event.reason != EVENT_FINGERPRINTS_RESET
                && event.reason != EVENT_ANCHOR_RESET
        })
        .cloned()
        .collect();
    events.push(PlannedEvent {
        warning: true,
        reason: EVENT_APPLY_FAILED,
        message: message.to_owned(),
    });
    events
}

/// How long to wait before the next reconcile: the planned delay (for example
/// until the next rotation) capped at `resync`, and at least one second.
#[must_use]
pub fn requeue_after(planned: Option<Duration>, resync: Duration) -> Duration {
    planned
        .map_or(resync, |planned| planned.min(resync))
        .max(MIN_REQUEUE)
}

/// A message for the `ApplyFailed` condition and event.
///
/// For an API-server rejection it is the HTTP code and the machine-readable
/// reason only; the server's message can echo submitted Secret data and is
/// never included. For every other error it is a fixed string naming the error
/// class, never the inner error's text.
#[must_use]
pub fn apply_failed_message(err: &kube::Error) -> String {
    format!("apply of Secret failed: {}", kube_error_summary(err))
}

/// A body-free summary of a kube client error: the HTTP code and
/// machine-readable reason for an API-server rejection, otherwise a fixed
/// string naming the error class. Never the server's message or the inner
/// error's text, which can echo request data.
#[must_use]
pub fn kube_error_summary(err: &kube::Error) -> String {
    match err {
        kube::Error::Api(status) => format!("{} {}", status.code, status.reason),
        kube::Error::HyperError(_)
        | kube::Error::Service(_)
        | kube::Error::HttpError(_)
        | kube::Error::ReadEvents(_) => "transport error".to_owned(),
        kube::Error::SerdeError(_) | kube::Error::FromUtf8(_) => {
            "response could not be decoded".to_owned()
        }
        kube::Error::RustlsTls(_) | kube::Error::TlsRequired => "TLS error".to_owned(),
        kube::Error::Auth(_) => "authentication error".to_owned(),
        kube::Error::BuildRequest(_) => "request could not be built".to_owned(),
        _ => "client error".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    // Tests report through panics.
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
    use secretspec_api::constants::{
        CONDITION_READY, EVENT_ADOPTED, EVENT_ANCHOR_RESET, EVENT_APPLY_FAILED,
        EVENT_FINGERPRINTS_RESET, EVENT_GENERATED, EVENT_LOW_ENTROPY, EVENT_REGENERATED,
        EVENT_ROTATED, REASON_APPLY_FAILED, REASON_GENERATED,
    };
    use secretspec_api::v1alpha1::SecretKind;

    use super::*;

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    fn ready(status: &str, reason: &str, at: &str, generation: i64) -> Condition {
        Condition {
            type_: CONDITION_READY.to_owned(),
            status: status.to_owned(),
            reason: reason.to_owned(),
            message: "old".to_owned(),
            observed_generation: Some(generation),
            last_transition_time: Time(ts(at)),
        }
    }

    fn previous(condition: Condition) -> SecretSpecStatus {
        SecretSpecStatus {
            observed_generation: Some(3),
            kind: Some(SecretKind::Fields),
            secret_name: Some("db".to_owned()),
            last_generated_time: Some(Time(ts("2026-01-01T00:00:00Z"))),
            last_rotation_time: Some(Time(ts("2026-01-02T00:00:00Z"))),
            next_rotation_time: Some(Time(ts("2026-01-03T00:00:00Z"))),
            conditions: vec![condition],
        }
    }

    #[test]
    fn apply_failed_status_keeps_previous() {
        let now = ts("2026-02-01T00:00:00Z");
        let before = previous(ready("True", REASON_GENERATED, "2026-01-01T00:00:00Z", 3));
        let status = apply_failed_status(Some(&before), Some(4), "boom", now);

        assert_eq!(status.observed_generation, Some(3));
        assert_eq!(status.kind, before.kind);
        assert_eq!(status.secret_name, before.secret_name);
        assert_eq!(status.last_generated_time, before.last_generated_time);
        assert_eq!(status.last_rotation_time, before.last_rotation_time);
        assert_eq!(status.next_rotation_time, before.next_rotation_time);
        assert_eq!(status.conditions.len(), 1);
        let condition = &status.conditions[0];
        assert_eq!(condition.type_, CONDITION_READY);
        assert_eq!(condition.status, "False");
        assert_eq!(condition.reason, REASON_APPLY_FAILED);
        assert_eq!(condition.message, "boom");
        assert_eq!(condition.observed_generation, Some(4));
        // Ready was True before: a transition happens now.
        assert_eq!(condition.last_transition_time, Time(now));
    }

    #[test]
    fn apply_failed_status_keeps_transition_time_when_already_failed() {
        let now = ts("2026-02-01T00:00:00Z");
        let before = previous(ready(
            "False",
            REASON_APPLY_FAILED,
            "2026-01-15T00:00:00Z",
            3,
        ));
        let status = apply_failed_status(Some(&before), Some(4), "again", now);
        let condition = &status.conditions[0];
        assert_eq!(condition.message, "again");
        assert_eq!(
            condition.last_transition_time,
            Time(ts("2026-01-15T00:00:00Z"))
        );
    }

    #[test]
    fn apply_failed_status_without_previous() {
        let now = ts("2026-02-01T00:00:00Z");
        let status = apply_failed_status(None, Some(1), "boom", now);
        assert_eq!(status.observed_generation, None);
        assert_eq!(status.conditions.len(), 1);
        assert_eq!(status.conditions[0].last_transition_time, Time(now));
        assert_eq!(status.conditions[0].observed_generation, Some(1));
    }

    #[test]
    fn apply_failed_status_carries_other_conditions() {
        let now = ts("2026-02-01T00:00:00Z");
        let mut before = previous(ready("True", REASON_GENERATED, "2026-01-01T00:00:00Z", 3));
        let mut other = ready("True", "Fine", "2026-01-01T00:00:00Z", 3);
        other.type_ = "Other".to_owned();
        before.conditions.push(other.clone());
        let status = apply_failed_status(Some(&before), Some(3), "boom", now);
        assert_eq!(status.conditions.len(), 2);
        assert_eq!(status.conditions[0].type_, CONDITION_READY);
        assert_eq!(status.conditions[1], other);
    }

    fn event(warning: bool, reason: &'static str) -> PlannedEvent {
        PlannedEvent {
            warning,
            reason,
            message: format!("{reason} message"),
        }
    }

    #[test]
    fn events_after_apply_failure_filters_normal_events() {
        let planned = [
            event(false, EVENT_GENERATED),
            event(false, EVENT_REGENERATED),
            event(false, EVENT_ROTATED),
            event(false, EVENT_ADOPTED),
            event(true, EVENT_FINGERPRINTS_RESET),
            event(true, EVENT_ANCHOR_RESET),
            event(true, EVENT_LOW_ENTROPY),
        ];
        let events = events_after_apply_failure(&planned, "boom");
        assert_eq!(
            events,
            [
                event(true, EVENT_LOW_ENTROPY),
                PlannedEvent {
                    warning: true,
                    reason: EVENT_APPLY_FAILED,
                    message: "boom".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn events_after_apply_failure_without_plan() {
        let events = events_after_apply_failure(&[], "boom");
        assert_eq!(events.len(), 1);
        assert!(events[0].warning);
        assert_eq!(events[0].reason, EVENT_APPLY_FAILED);
    }

    #[test]
    fn requeue_after_caps_and_floors() {
        let resync = Duration::from_secs(600);
        assert_eq!(requeue_after(None, resync), resync);
        assert_eq!(
            requeue_after(Some(Duration::from_secs(30)), resync),
            Duration::from_secs(30)
        );
        assert_eq!(
            requeue_after(Some(Duration::from_secs(3_600)), resync),
            resync
        );
        assert_eq!(
            requeue_after(Some(Duration::ZERO), resync),
            Duration::from_secs(1)
        );
        assert_eq!(
            requeue_after(None, Duration::from_millis(10)),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn apply_failed_message_has_no_body() {
        let status = kube::core::Status {
            code: 422,
            reason: "Invalid".to_owned(),
            message: "data[password]: c2VjcmV0".to_owned(),
            ..kube::core::Status::default()
        };
        let message = apply_failed_message(&kube::Error::Api(Box::new(status)));
        assert_eq!(message, "apply of Secret failed: 422 Invalid");
        assert!(!message.contains("c2VjcmV0"));
    }

    #[test]
    fn apply_failed_message_for_other_errors_is_generic() {
        let message = apply_failed_message(&kube::Error::TlsRequired);
        assert!(message.starts_with("apply of Secret failed: "));
        assert!(!message.contains("TLS required"));
        let message = apply_failed_message(&kube::Error::LinesCodecMaxLineLengthExceeded);
        assert!(message.starts_with("apply of Secret failed: "));
    }
}
