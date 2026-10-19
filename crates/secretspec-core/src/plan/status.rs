// SPDX-License-Identifier: Apache-2.0

//! The `SecretSpec` status the planner reports.

use jiff::Timestamp;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use secretspec_api::constants::CONDITION_READY;
use secretspec_api::v1alpha1::{SecretKind, SecretSpec, SecretSpecStatus};

/// The state of the `Ready` condition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Readiness {
    /// `True` or `False`.
    pub ready: bool,
    /// One of the `REASON_*` constants.
    pub reason: &'static str,
    /// Human-readable detail; never contains secret material.
    pub message: String,
}

/// The time fields of the status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Times {
    /// Last time any value was (re)generated.
    pub last_generated: Option<Time>,
    /// Time of the last scheduled rotation.
    pub last_rotation: Option<Time>,
    /// Time of the next scheduled rotation.
    pub next_rotation: Option<Time>,
}

impl Times {
    /// The times recorded in the current status of `spec`, unchanged.
    #[must_use]
    pub fn previous(spec: &SecretSpec) -> Self {
        spec.status
            .as_ref()
            .map_or_else(Self::default, |status| Self {
                last_generated: status.last_generated_time.clone(),
                last_rotation: status.last_rotation_time.clone(),
                next_rotation: status.next_rotation_time.clone(),
            })
    }
}

/// The status to write for `spec`.
///
/// The `Ready` condition keeps its previous `lastTransitionTime` when both its
/// status and its reason are unchanged; otherwise the transition happens at
/// `now`. Conditions of other types are carried over.
#[must_use]
pub fn status(
    spec: &SecretSpec,
    kind: Option<SecretKind>,
    readiness: Readiness,
    times: Times,
    now: Timestamp,
) -> SecretSpecStatus {
    let previous = spec.status.as_ref();
    let generation = spec.metadata.generation;
    let status_text = if readiness.ready { "True" } else { "False" };

    let previous_ready = previous
        .into_iter()
        .flat_map(|status| status.conditions.iter())
        .find(|condition| condition.type_ == CONDITION_READY);
    let last_transition_time = match previous_ready {
        Some(condition)
            if condition.status == status_text && condition.reason == readiness.reason =>
        {
            condition.last_transition_time.clone()
        }
        _ => Time(now),
    };

    let mut conditions: Vec<Condition> = previous
        .into_iter()
        .flat_map(|status| status.conditions.iter())
        .filter(|condition| condition.type_ != CONDITION_READY)
        .cloned()
        .collect();
    conditions.insert(
        0,
        Condition {
            type_: CONDITION_READY.to_owned(),
            status: status_text.to_owned(),
            reason: readiness.reason.to_owned(),
            message: readiness.message,
            observed_generation: generation,
            last_transition_time,
        },
    );

    SecretSpecStatus {
        observed_generation: generation,
        kind,
        secret_name: spec.metadata.name.clone(),
        last_generated_time: times.last_generated,
        last_rotation_time: times.last_rotation,
        next_rotation_time: times.next_rotation,
        conditions,
    }
}

#[cfg(test)]
mod tests {
    // Tests report through panics.
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use secretspec_api::constants::{REASON_CONFLICT, REASON_GENERATED};
    use secretspec_api::v1alpha1::SecretSpecSpec;

    use super::*;

    fn ts(text: &str) -> Timestamp {
        text.parse().expect("timestamp")
    }

    fn spec_with(previous: Option<SecretSpecStatus>) -> SecretSpec {
        let body: SecretSpecSpec =
            serde_json::from_str(r#"{"sshKeyPair":{}}"#).expect("spec deserializes");
        let mut spec = SecretSpec::new("db", body);
        spec.metadata.generation = Some(7);
        spec.status = previous;
        spec
    }

    fn generated() -> Readiness {
        Readiness {
            ready: true,
            reason: REASON_GENERATED,
            message: "Secret db is up to date".to_owned(),
        }
    }

    fn other_condition() -> Condition {
        Condition {
            type_: "Other".to_owned(),
            status: "True".to_owned(),
            reason: "Because".to_owned(),
            message: String::new(),
            observed_generation: None,
            last_transition_time: Time(ts("2020-01-01T00:00:00Z")),
        }
    }

    #[test]
    fn first_status_transitions_now() {
        let now = ts("2026-10-19T12:00:00Z");
        let s = status(
            &spec_with(None),
            Some(SecretKind::SshKeyPair),
            generated(),
            Times::default(),
            now,
        );
        assert_eq!(s.observed_generation, Some(7));
        assert_eq!(s.secret_name.as_deref(), Some("db"));
        assert_eq!(s.kind, Some(SecretKind::SshKeyPair));
        assert_eq!(s.conditions.len(), 1);
        let ready = &s.conditions[0];
        assert_eq!(ready.type_, CONDITION_READY);
        assert_eq!(ready.status, "True");
        assert_eq!(ready.observed_generation, Some(7));
        assert_eq!(ready.last_transition_time, Time(now));
    }

    #[test]
    fn unchanged_status_and_reason_keep_transition_time() {
        let earlier = ts("2026-01-01T00:00:00Z");
        let first = status(
            &spec_with(None),
            None,
            generated(),
            Times::default(),
            earlier,
        );
        let mut readiness = generated();
        readiness.message = "different message".to_owned();
        let second = status(
            &spec_with(Some(first)),
            None,
            readiness,
            Times::default(),
            ts("2026-10-19T12:00:00Z"),
        );
        assert_eq!(second.conditions[0].last_transition_time, Time(earlier));
        assert_eq!(second.conditions[0].message, "different message");
    }

    #[test]
    fn changed_reason_transitions_now() {
        let earlier = ts("2026-01-01T00:00:00Z");
        let now = ts("2026-10-19T12:00:00Z");
        let first = status(
            &spec_with(None),
            None,
            generated(),
            Times::default(),
            earlier,
        );
        let conflict = Readiness {
            ready: false,
            reason: REASON_CONFLICT,
            message: "m".to_owned(),
        };
        let second = status(
            &spec_with(Some(first)),
            None,
            conflict,
            Times::default(),
            now,
        );
        assert_eq!(second.conditions[0].status, "False");
        assert_eq!(second.conditions[0].last_transition_time, Time(now));
    }

    #[test]
    fn other_conditions_are_kept_and_times_preserved() {
        let at = Time(ts("2026-02-02T00:00:00Z"));
        let previous = SecretSpecStatus {
            last_generated_time: Some(at.clone()),
            last_rotation_time: Some(at.clone()),
            next_rotation_time: Some(at.clone()),
            conditions: vec![other_condition()],
            ..SecretSpecStatus::default()
        };
        let spec = spec_with(Some(previous));
        let s = status(
            &spec,
            None,
            generated(),
            Times::previous(&spec),
            ts("2026-10-19T12:00:00Z"),
        );
        assert_eq!(s.conditions.len(), 2);
        assert_eq!(s.conditions[0].type_, CONDITION_READY);
        assert_eq!(s.conditions[1], other_condition());
        assert_eq!(s.last_generated_time, Some(at.clone()));
        assert_eq!(s.last_rotation_time, Some(at.clone()));
        assert_eq!(s.next_rotation_time, Some(at));
    }
}
