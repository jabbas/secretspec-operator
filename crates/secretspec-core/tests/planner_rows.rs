// SPDX-License-Identifier: Apache-2.0

//! The behaviour table of the design document (section 7.2), one test per row.

// Tests report through panics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use secretspec_api::constants::{
    ANNOTATION_FINGERPRINTS, ANNOTATION_GENERATED_AT, ANNOTATION_KEYS, ANNOTATION_SPEC_UID,
    CONDITION_READY, EVENT_CONFLICT, EVENT_GENERATED, LABEL_MANAGED_BY, LABEL_SPEC_NAME,
    MANAGED_BY_VALUE, REASON_CONFLICT, REASON_GENERATED,
};
use secretspec_api::v1alpha1::SecretKind;
use secretspec_core::plan::{SecretAction, plan};
use support::{SPEC_NAME, SPEC_NAMESPACE, SPEC_UID, foreign_secret, now, secret_spec, seeded_rng};

#[test]
fn row_01_new_secretspec() {
    let spec = secret_spec(
        "{secret: {labels: {team: a}, annotations: {note: b}}, data: {username: app}, \
         fields: [{name: password}, {name: token, bytes: 32, encoding: hex}]}",
    );
    let plan = plan(&spec, None, now(), &mut seeded_rng(1));

    let SecretAction::Apply(secret) = &plan.action else {
        panic!("expected Apply, got {:?}", plan.action);
    };
    let meta = &secret.metadata;
    assert_eq!(meta.name.as_deref(), Some(SPEC_NAME));
    assert_eq!(meta.namespace.as_deref(), Some(SPEC_NAMESPACE));
    assert_eq!(secret.type_.as_deref(), Some("Opaque"));

    let data = secret.data.as_ref().expect("data");
    assert_eq!(
        data.keys().map(String::as_str).collect::<Vec<_>>(),
        ["password", "token", "username"]
    );
    assert_eq!(data["username"].0, b"app");
    assert_eq!(data["password"].0.len(), 32);
    assert_eq!(data["token"].0.len(), 64);

    let labels = meta.labels.as_ref().expect("labels");
    assert_eq!(labels.len(), 3);
    assert_eq!(labels[LABEL_MANAGED_BY], MANAGED_BY_VALUE);
    assert_eq!(labels[LABEL_SPEC_NAME], SPEC_NAME);
    assert_eq!(labels["team"], "a");

    let annotations = meta.annotations.as_ref().expect("annotations");
    assert_eq!(annotations["note"], "b");
    assert_eq!(annotations[ANNOTATION_SPEC_UID], SPEC_UID);
    assert_eq!(annotations[ANNOTATION_GENERATED_AT], "2026-10-19T12:00:00Z");
    assert!(annotations[ANNOTATION_FINGERPRINTS].contains("\"field/password\""));
    assert!(annotations[ANNOTATION_FINGERPRINTS].contains("\"field/token\""));
    assert!(
        !annotations.contains_key(ANNOTATION_KEYS),
        "fields have no keys map"
    );
    assert_eq!(annotations.len(), 4);

    let owners = meta.owner_references.as_ref().expect("owner references");
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].uid, SPEC_UID);
    assert_eq!(owners[0].controller, Some(true));

    let status = &plan.status;
    assert_eq!(status.observed_generation, Some(1));
    assert_eq!(status.kind, Some(SecretKind::Fields));
    assert_eq!(status.secret_name.as_deref(), Some(SPEC_NAME));
    assert_eq!(
        status.last_generated_time.as_ref().map(|t| t.0),
        Some(now())
    );
    assert_eq!(status.last_rotation_time, None);
    assert_eq!(status.next_rotation_time, None);
    let ready = &status.conditions[0];
    assert_eq!(ready.type_, CONDITION_READY);
    assert_eq!(ready.status, "True");
    assert_eq!(ready.reason, REASON_GENERATED);
    assert_eq!(ready.message, "Secret db is up to date");

    assert_eq!(plan.events.len(), 1);
    assert!(!plan.events[0].warning);
    assert_eq!(plan.events[0].reason, EVENT_GENERATED);
    assert_eq!(plan.events[0].message, "generated keys: password, token");
    assert_eq!(plan.requeue_after, None);
}

#[test]
fn row_15_foreign_secret_is_a_conflict() {
    let spec = secret_spec("fields: [{name: password}]");
    let existing = foreign_secret(&[("app", "x")], &[("password", "hunter2")]);
    let plan = plan(&spec, Some(&existing), now(), &mut seeded_rng(1));

    assert!(matches!(plan.action, SecretAction::Nothing));
    let ready = &plan.status.conditions[0];
    assert_eq!(ready.status, "False");
    assert_eq!(ready.reason, REASON_CONFLICT);
    assert_eq!(
        ready.message,
        "Secret db exists and is not managed by this SecretSpec"
    );
    assert_eq!(plan.status.secret_name.as_deref(), Some(SPEC_NAME));
    assert_eq!(plan.status.kind, Some(SecretKind::Fields));
    assert_eq!(plan.status.last_generated_time, None);

    assert_eq!(plan.events.len(), 1);
    assert!(plan.events[0].warning);
    assert_eq!(plan.events[0].reason, EVENT_CONFLICT);
    assert_eq!(
        plan.events[0].message,
        "Secret db exists and is not managed by this SecretSpec"
    );
    assert_eq!(plan.requeue_after, None);
}
