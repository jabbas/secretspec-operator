// SPDX-License-Identifier: Apache-2.0

//! The behaviour table of the design document (section 7.2), one test per row.

// Tests report through panics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::time::Duration;

use jiff::Timestamp;
use k8s_openapi::api::core::v1::Secret;
use secretspec_api::constants::{
    ANNOTATION_FINGERPRINTS, ANNOTATION_GENERATED_AT, ANNOTATION_KEYS, ANNOTATION_SPEC_UID,
    CONDITION_READY, EVENT_ADOPTED, EVENT_ANCHOR_RESET, EVENT_CONFLICT, EVENT_FINGERPRINTS_RESET,
    EVENT_GENERATED, EVENT_REGENERATED, EVENT_ROTATED, LABEL_MANAGED_BY, LABEL_SPEC_NAME,
    MANAGED_BY_VALUE, REASON_CONFLICT, REASON_GENERATED,
};
use secretspec_api::v1alpha1::SecretKind;
use secretspec_core::plan::{Plan, SecretAction, plan};
use support::{
    DAY, SPEC_NAME, SPEC_NAMESPACE, SPEC_UID, annotation, applied, earlier, foreign_secret, later,
    next_generation, now, reasons, secret_spec, seeded_rng, string_map, value, with_status,
};

/// Plans `old` at [`now`], then `new` (generation 2, status of the first
/// plan) against the Secret the first plan produced, at `at`.
fn change(old: &str, new: &str, at: Timestamp) -> (Secret, Plan, Plan) {
    let spec = secret_spec(old);
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = next_generation(secret_spec(new), &first);
    let second = plan(&spec, Some(&secret), at, &mut seeded_rng(2));
    (secret, first, second)
}

/// The plan reports success with no change of the recorded generation time.
fn assert_quiet(first: &Plan, second: &Plan) {
    assert!(second.events.is_empty(), "{:?}", second.events);
    let ready = &second.status.conditions[0];
    assert_eq!(ready.status, "True");
    assert_eq!(ready.reason, REASON_GENERATED);
    assert_eq!(
        second.status.last_generated_time, first.status.last_generated_time,
        "nothing was generated"
    );
}

fn assert_ready(plan: &Plan) {
    let ready = &plan.status.conditions[0];
    assert_eq!(ready.type_, CONDITION_READY);
    assert_eq!(ready.status, "True");
    assert_eq!(ready.reason, REASON_GENERATED);
}

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
fn row_02_changed_parameters_regenerate_that_key_only() {
    let (secret, first, second) = change(
        "fields: [{name: a, length: 32}, {name: b, length: 32}]",
        "fields: [{name: a, length: 40}, {name: b, length: 32}]",
        later(1),
    );
    let desired = applied(&second);
    assert_eq!(value(&desired, "a").len(), 40);
    assert_ne!(value(&desired, "a"), value(&secret, "a"));
    assert_eq!(value(&desired, "b"), value(&secret, "b"));
    assert_ne!(
        annotation(&desired, ANNOTATION_FINGERPRINTS),
        annotation(&secret, ANNOTATION_FINGERPRINTS)
    );
    assert_eq!(reasons(&second), [EVENT_REGENERATED]);
    assert!(!second.events[0].warning);
    assert_eq!(second.events[0].message, "regenerated keys: a");
    assert_ready(&second);
    assert_eq!(
        second.status.last_generated_time.as_ref().map(|t| t.0),
        Some(later(1))
    );
    assert_ne!(
        second.status.last_generated_time,
        first.status.last_generated_time
    );
    // A regeneration is not a rotation: the anchor stays.
    assert_eq!(
        annotation(&desired, ANNOTATION_GENERATED_AT),
        Some("2026-10-19T12:00:00Z")
    );
    assert_eq!(second.status.last_rotation_time, None);
}

#[test]
fn row_03_added_and_removed_keys() {
    // Added: only the new key is generated.
    let (secret, _, added) = change(
        "fields: [{name: a}, {name: b}]",
        "fields: [{name: a}, {name: b}, {name: c}]",
        later(1),
    );
    let desired = applied(&added);
    assert_eq!(value(&desired, "a"), value(&secret, "a"));
    assert_eq!(value(&desired, "b"), value(&secret, "b"));
    assert_eq!(value(&desired, "c").len(), 32);
    assert_eq!(reasons(&added), [EVENT_REGENERATED]);
    assert_eq!(added.events[0].message, "regenerated keys: c");
    assert_ready(&added);
    assert_eq!(
        added.status.last_generated_time.as_ref().map(|t| t.0),
        Some(later(1))
    );

    // Removed: the key leaves the Secret, the others stay, nothing is generated.
    let (secret, first, removed) = change(
        "fields: [{name: a}, {name: b}, {name: c}]",
        "fields: [{name: a}, {name: b}]",
        later(1),
    );
    let desired = applied(&removed);
    assert_eq!(
        desired
            .data
            .as_ref()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(value(&desired, "a"), value(&secret, "a"));
    assert_eq!(value(&desired, "b"), value(&secret, "b"));
    assert!(
        !annotation(&desired, ANNOTATION_FINGERPRINTS)
            .unwrap()
            .contains("field/c")
    );
    assert_quiet(&first, &removed);
}

#[test]
fn row_04_changed_literal_keeps_generated_values() {
    let (secret, first, second) = change(
        "{data: {username: app}, fields: [{name: password}]}",
        "{data: {username: other}, fields: [{name: password}]}",
        later(1),
    );
    let desired = applied(&second);
    assert_eq!(value(&desired, "username"), b"other");
    assert_eq!(value(&desired, "password"), value(&secret, "password"));
    assert_eq!(
        annotation(&desired, ANNOTATION_FINGERPRINTS),
        annotation(&secret, ANNOTATION_FINGERPRINTS)
    );
    assert_quiet(&first, &second);
}

#[test]
fn row_05_non_generating_changes_do_not_regenerate() {
    // deletionPolicy: the owner reference goes, the values stay.
    let (secret, first, second) = change(
        "fields: [{name: password}]",
        "{deletionPolicy: Retain, fields: [{name: password}]}",
        later(1),
    );
    let desired = applied(&second);
    assert!(secret.metadata.owner_references.is_some());
    assert_eq!(desired.metadata.owner_references, None);
    assert_eq!(desired.data, secret.data);
    assert_quiet(&first, &second);

    // The applied Secret no longer has our controller reference; the next
    // call recognises it by `spec-uid` and finds it up to date.
    let retain = next_generation(
        secret_spec("{deletionPolicy: Retain, fields: [{name: password}]}"),
        &second,
    );
    let again = plan(&retain, Some(&desired), later(2), &mut seeded_rng(3));
    assert!(matches!(again.action, SecretAction::UpToDate));
    assert!(again.events.is_empty(), "{:?}", again.events);

    // secret.labels / secret.annotations.
    let (secret, first, second) = change(
        "{secret: {labels: {team: a}}, fields: [{name: password}]}",
        "{secret: {labels: {team: b}, annotations: {note: n}}, fields: [{name: password}]}",
        later(1),
    );
    let desired = applied(&second);
    assert_eq!(desired.metadata.labels.as_ref().unwrap()["team"], "b");
    assert_eq!(annotation(&desired, "note"), Some("n"));
    assert_eq!(desired.data, secret.data);
    assert_quiet(&first, &second);

    // SSH comment: the public-key line is rewritten, the private key stays.
    let (secret, first, second) = change(
        "sshKeyPair: {comment: one}",
        "sshKeyPair: {comment: two}",
        later(1),
    );
    let desired = applied(&second);
    assert_eq!(
        value(&desired, "ssh-privatekey"),
        value(&secret, "ssh-privatekey")
    );
    assert_ne!(
        value(&desired, "ssh-publickey"),
        value(&secret, "ssh-publickey")
    );
    assert!(value(&desired, "ssh-publickey").ends_with(b" two\n"));
    assert!(value(&secret, "ssh-publickey").ends_with(b" one\n"));
    assert_quiet(&first, &second);

    // basicAuth.username: the htpasswd line is recomputed, the password stays.
    let (secret, first, second) = change(
        "basicAuth: {username: admin}",
        "basicAuth: {username: root}",
        later(1),
    );
    let desired = applied(&second);
    assert_eq!(value(&desired, "username"), b"root");
    assert_eq!(value(&desired, "password"), value(&secret, "password"));
    assert_ne!(value(&desired, "auth"), value(&secret, "auth"));
    assert!(value(&desired, "auth").starts_with(b"root:$2y$12$"));
    assert_quiet(&first, &second);
}

#[test]
fn row_06_spec_metadata_changes_do_nothing() {
    let spec = secret_spec("{rotation: {interval: 90d}, fields: [{name: password}]}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);

    // A Helm chart-version bump: labels and annotations of the SecretSpec
    // change, `metadata.generation` does not.
    let mut bumped = with_status(spec, &first);
    bumped.metadata.labels = Some(string_map(&[("helm.sh/chart", "app-2.0.0")]));
    bumped.metadata.annotations = Some(string_map(&[("meta.helm.sh/release-name", "app")]));
    let second = plan(&bumped, Some(&secret), later(1), &mut seeded_rng(2));
    assert!(matches!(second.action, SecretAction::UpToDate));
    assert!(second.events.is_empty(), "{:?}", second.events);
    assert_eq!(second.status, first.status);
}

#[test]
fn row_07_restart_reapply_and_cache_loss_do_nothing() {
    let spec = secret_spec(
        "{rotation: {interval: 90d}, data: {user: u}, \
         fields: [{name: a}, {name: b, bytes: 16, encoding: hex}]}",
    );
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = with_status(spec, &first);

    // A restarted operator reads the same inputs a day later and holds a
    // different RNG; nothing is written, reported or changed in the status.
    let second = plan(&spec, Some(&secret), later(1), &mut seeded_rng(99));
    assert!(matches!(second.action, SecretAction::UpToDate));
    assert!(second.events.is_empty(), "{:?}", second.events);
    assert_eq!(second.status, first.status);
    assert_eq!(
        second.status.conditions[0].last_transition_time,
        first.status.conditions[0].last_transition_time
    );
    assert_eq!(second.requeue_after, Some(Duration::from_secs(89 * DAY)));
}

#[test]
fn row_08_deleted_or_emptied_key_is_regenerated() {
    let spec = secret_spec("fields: [{name: a}, {name: b}]");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = with_status(spec, &first);

    let mut deleted = secret.clone();
    deleted.data.as_mut().unwrap().remove("b");
    let mut emptied = secret.clone();
    emptied
        .data
        .as_mut()
        .unwrap()
        .insert("b".to_owned(), k8s_openapi::ByteString(Vec::new()));

    for (case, damaged) in [("deleted", deleted), ("emptied", emptied)] {
        let p = plan(&spec, Some(&damaged), later(1), &mut seeded_rng(2));
        let desired = applied(&p);
        assert_eq!(value(&desired, "a"), value(&secret, "a"), "{case}");
        assert_eq!(value(&desired, "b").len(), 32, "{case}");
        assert_ne!(value(&desired, "b"), value(&secret, "b"), "{case}");
        assert_eq!(reasons(&p), [EVENT_REGENERATED], "{case}");
        assert_eq!(p.events[0].message, "regenerated keys: b", "{case}");
        assert_ready(&p);
        assert_eq!(
            p.status.last_generated_time.as_ref().map(|t| t.0),
            Some(later(1)),
            "{case}"
        );
        // A regeneration is not a rotation: the anchor stays.
        assert_eq!(
            annotation(&desired, ANNOTATION_GENERATED_AT),
            Some("2026-10-19T12:00:00Z"),
            "{case}"
        );
    }
}

#[test]
fn row_09_deleted_secret_is_fully_regenerated() {
    let spec = secret_spec("{data: {user: u}, fields: [{name: a}, {name: b}]}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = with_status(spec, &first);

    let second = plan(&spec, None, later(1), &mut seeded_rng(2));
    let desired = applied(&second);
    assert_ne!(value(&desired, "a"), value(&secret, "a"));
    assert_ne!(value(&desired, "b"), value(&secret, "b"));
    assert_eq!(value(&desired, "user"), b"u");
    assert_eq!(reasons(&second), [EVENT_GENERATED]);
    assert_eq!(second.events[0].message, "generated keys: a, b");
    assert_ready(&second);
    assert_eq!(
        second.status.last_generated_time.as_ref().map(|t| t.0),
        Some(later(1))
    );
    assert_eq!(
        annotation(&desired, ANNOTATION_GENERATED_AT),
        Some(later(1).to_string().as_str())
    );
}

#[test]
fn row_10_elapsed_interval_rotates_everything() {
    let spec = secret_spec("{rotation: {interval: 90d}, data: {user: u}, sshKeyPair: {}}");
    let first = plan(&spec, None, earlier(91), &mut seeded_rng(1));
    let secret = applied(&first);
    assert_eq!(
        annotation(&secret, ANNOTATION_GENERATED_AT),
        Some(earlier(91).to_string().as_str())
    );
    let spec = with_status(spec, &first);

    let due = plan(&spec, Some(&secret), now(), &mut seeded_rng(2));
    let desired = applied(&due);
    assert_ne!(
        value(&desired, "ssh-privatekey"),
        value(&secret, "ssh-privatekey")
    );
    assert_ne!(
        value(&desired, "ssh-publickey"),
        value(&secret, "ssh-publickey")
    );
    assert_eq!(value(&desired, "user"), b"u");
    assert_eq!(reasons(&due), [EVENT_ROTATED]);
    assert!(!due.events[0].warning);
    assert_eq!(
        due.events[0].message,
        "rotated keys: ssh-privatekey, ssh-publickey"
    );
    assert_ready(&due);
    assert_eq!(
        annotation(&desired, ANNOTATION_GENERATED_AT),
        Some(now().to_string().as_str())
    );
    assert_eq!(
        due.status.last_rotation_time.as_ref().map(|t| t.0),
        Some(now())
    );
    assert_eq!(
        due.status.last_generated_time.as_ref().map(|t| t.0),
        Some(now())
    );
    assert_eq!(
        due.status.next_rotation_time.as_ref().map(|t| t.0),
        Some(later(90))
    );
    assert_eq!(due.requeue_after, Some(Duration::from_secs(90 * DAY)));

    // Plain fields rotate too; literals stay.
    let spec = secret_spec(
        "{rotation: {interval: 90d}, data: {user: u}, fields: [{name: a}, {name: b, bytes: 16, encoding: hex}]}",
    );
    let first = plan(&spec, None, earlier(91), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = with_status(spec, &first);
    let due = plan(&spec, Some(&secret), now(), &mut seeded_rng(2));
    let desired = applied(&due);
    assert_ne!(value(&desired, "a"), value(&secret, "a"));
    assert_ne!(value(&desired, "b"), value(&secret, "b"));
    assert_eq!(value(&desired, "user"), b"u");
    assert_eq!(reasons(&due), [EVENT_ROTATED]);
    assert_eq!(due.events[0].message, "rotated keys: a, b");

    // Missed intervals coalesce into one rotation.
    let spec = secret_spec("{rotation: {interval: 90d}, data: {user: u}, sshKeyPair: {}}");
    let old = plan(&spec, None, earlier(400), &mut seeded_rng(1));
    let stale = applied(&old);
    let spec = with_status(spec, &old);
    let overdue = plan(&spec, Some(&stale), now(), &mut seeded_rng(2));
    assert_eq!(reasons(&overdue), [EVENT_ROTATED]);
    assert_eq!(
        overdue.status.next_rotation_time.as_ref().map(|t| t.0),
        Some(later(90))
    );
}

#[test]
fn row_11_enabling_rotation_does_not_rotate() {
    let spec = secret_spec("fields: [{name: a}]");
    let first = plan(&spec, None, earlier(10), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = next_generation(
        secret_spec("{rotation: {interval: 90d}, fields: [{name: a}]}"),
        &first,
    );

    // The anchor is the existing `generated-at`.
    let second = plan(&spec, Some(&secret), now(), &mut seeded_rng(2));
    let desired = applied(&second);
    assert_eq!(desired.data, secret.data);
    assert!(second.events.is_empty(), "{:?}", second.events);
    assert_ready(&second);
    assert_eq!(
        annotation(&desired, ANNOTATION_GENERATED_AT),
        Some(earlier(10).to_string().as_str())
    );
    assert_eq!(
        second.status.next_rotation_time.as_ref().map(|t| t.0),
        Some(later(80)),
        "anchor + 90d"
    );
    assert_eq!(second.requeue_after, Some(Duration::from_secs(80 * DAY)));
    assert_eq!(second.status.last_rotation_time, None);

    // Without `generated-at` the anchor is now.
    let mut bare = secret;
    bare.metadata
        .annotations
        .as_mut()
        .unwrap()
        .remove(ANNOTATION_GENERATED_AT);
    let third = plan(&spec, Some(&bare), now(), &mut seeded_rng(2));
    let desired = applied(&third);
    assert_eq!(desired.data, bare.data);
    assert!(third.events.is_empty(), "{:?}", third.events);
    assert_eq!(
        annotation(&desired, ANNOTATION_GENERATED_AT),
        Some(now().to_string().as_str())
    );
    assert_eq!(
        third.status.next_rotation_time.as_ref().map(|t| t.0),
        Some(later(90))
    );
}

#[test]
fn row_12_removed_rotation_keeps_values() {
    let spec = secret_spec("{rotation: {interval: 90d}, fields: [{name: a}]}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    assert!(first.status.next_rotation_time.is_some());
    let spec = next_generation(secret_spec("fields: [{name: a}]"), &first);

    // Even long after the interval would have elapsed.
    let second = plan(&spec, Some(&secret), later(200), &mut seeded_rng(2));
    let desired = applied(&second);
    assert_eq!(desired.data, secret.data);
    assert!(second.events.is_empty(), "{:?}", second.events);
    assert_ready(&second);
    assert_eq!(second.status.next_rotation_time, None);
    assert_eq!(second.status.last_rotation_time, None);
    assert_eq!(second.requeue_after, None);
    assert_eq!(
        annotation(&desired, ANNOTATION_GENERATED_AT),
        annotation(&secret, ANNOTATION_GENERATED_AT)
    );
}

#[test]
fn row_13_deletion_policy_decides_the_owner_reference() {
    let delete = plan(
        &secret_spec("{deletionPolicy: Delete, fields: [{name: a}]}"),
        None,
        now(),
        &mut seeded_rng(1),
    );
    let owners = applied(&delete)
        .metadata
        .owner_references
        .expect("a Delete Secret is owned by the SecretSpec");
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].kind, "SecretSpec");
    assert_eq!(owners[0].name, SPEC_NAME);
    assert_eq!(owners[0].uid, SPEC_UID);
    assert_eq!(owners[0].controller, Some(true));
    assert_eq!(owners[0].block_owner_deletion, Some(true));

    let retain = applied(&plan(
        &secret_spec("{deletionPolicy: Retain, fields: [{name: a}]}"),
        None,
        now(),
        &mut seeded_rng(1),
    ));
    assert_eq!(
        retain.metadata.owner_references, None,
        "a Retain Secret outlives the SecretSpec"
    );
    // It stays recognisable for a later adoption.
    assert_eq!(annotation(&retain, ANNOTATION_SPEC_UID), Some(SPEC_UID));
    assert_eq!(
        retain.metadata.labels.as_ref().unwrap()[LABEL_SPEC_NAME],
        SPEC_NAME
    );
}

#[test]
fn row_14_recreated_secretspec_adopts_the_retained_secret() {
    let retained = "{deletionPolicy: Retain, fields: [{name: a}, {name: b}]}";
    let secret = applied(&plan(
        &secret_spec(retained),
        None,
        now(),
        &mut seeded_rng(1),
    ));

    // Same parameters: everything is kept.
    let mut recreated = secret_spec(retained);
    recreated.metadata.uid = Some("new-uid".to_owned());
    let adopted = plan(&recreated, Some(&secret), later(1), &mut seeded_rng(2));
    let desired = applied(&adopted);
    assert_eq!(desired.data, secret.data);
    assert_eq!(annotation(&desired, ANNOTATION_SPEC_UID), Some("new-uid"));
    assert_eq!(reasons(&adopted), [EVENT_ADOPTED]);
    assert!(!adopted.events[0].warning);
    assert_eq!(adopted.events[0].message, "adopted existing Secret db");
    assert_ready(&adopted);
    assert_eq!(adopted.status.last_generated_time, None);

    // A key whose parameters changed meanwhile is regenerated; the matching
    // key is kept.
    let mut changed =
        secret_spec("{deletionPolicy: Retain, fields: [{name: a}, {name: b, length: 40}]}");
    changed.metadata.uid = Some("new-uid".to_owned());
    let adopted = plan(&changed, Some(&secret), later(1), &mut seeded_rng(2));
    let desired = applied(&adopted);
    assert_eq!(value(&desired, "a"), value(&secret, "a"));
    assert_eq!(value(&desired, "b").len(), 40);
    assert_eq!(reasons(&adopted), [EVENT_REGENERATED, EVENT_ADOPTED]);
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

#[test]
fn row_16_lost_fingerprints_keep_values() {
    let spec = secret_spec("fields: [{name: a}, {name: b}]");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = with_status(spec, &first);
    let mut damaged = secret.clone();
    damaged
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .remove(ANNOTATION_FINGERPRINTS);

    let p = plan(&spec, Some(&damaged), later(1), &mut seeded_rng(2));
    let desired = applied(&p);
    assert_eq!(desired.data, secret.data, "every value is kept");
    assert_eq!(
        annotation(&desired, ANNOTATION_FINGERPRINTS),
        annotation(&secret, ANNOTATION_FINGERPRINTS),
        "fingerprints are re-stamped from the spec"
    );
    assert_eq!(reasons(&p), [EVENT_FINGERPRINTS_RESET]);
    assert!(p.events[0].warning);
    assert_eq!(
        p.events[0].message,
        "kept existing values for keys: a, b (fingerprint metadata was missing or invalid)"
    );
    assert_ready(&p);
    assert_eq!(
        p.status.last_generated_time, first.status.last_generated_time,
        "nothing was generated"
    );
}

#[test]
fn row_17_renamed_keys_move_values() {
    let (secret, first, second) = change(
        "basicAuth: {username: admin}",
        "basicAuth: {username: admin, keys: {username: user, password: pw, htpasswd: users}}",
        later(1),
    );
    let desired = applied(&second);
    assert_eq!(
        desired
            .data
            .as_ref()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["pw", "user", "users"]
    );
    assert_eq!(value(&desired, "pw"), value(&secret, "password"));
    assert_eq!(value(&desired, "users"), value(&secret, "auth"));
    assert_eq!(value(&desired, "user"), b"admin");
    assert_eq!(
        annotation(&desired, ANNOTATION_KEYS),
        Some(
            r#"{"basic-auth/htpasswd":"users","basic-auth/password":"pw","basic-auth/username":"user"}"#
        )
    );
    assert_quiet(&first, &second);

    let (secret, first, second) = change(
        "sshKeyPair: {}",
        "sshKeyPair: {keys: {privateKey: id, publicKey: id.pub}}",
        later(1),
    );
    let desired = applied(&second);
    assert_eq!(value(&desired, "id"), value(&secret, "ssh-privatekey"));
    assert_eq!(value(&desired, "id.pub"), value(&secret, "ssh-publickey"));
    assert_eq!(desired.data.as_ref().unwrap().len(), 2);
    assert_quiet(&first, &second);
}

#[test]
fn row_18_type_mismatch_is_a_conflict() {
    let retained = secret_spec("{deletionPolicy: Retain, basicAuth: {username: admin}}");
    let secret = applied(&plan(&retained, None, now(), &mut seeded_rng(1)));
    let spec = secret_spec(
        "{deletionPolicy: Retain, secret: {type: kubernetes.io/basic-auth}, basicAuth: {username: admin}}",
    );
    let p = plan(&spec, Some(&secret), now(), &mut seeded_rng(2));
    assert!(matches!(p.action, SecretAction::Nothing), "nothing written");
    let ready = &p.status.conditions[0];
    assert_eq!(ready.status, "False");
    assert_eq!(ready.reason, REASON_CONFLICT);
    assert_eq!(
        ready.message,
        "Secret db has type Opaque; delete it to recreate it with type kubernetes.io/basic-auth"
    );
    assert_eq!(p.events.len(), 1);
    assert!(p.events[0].warning);
    assert_eq!(p.events[0].reason, EVENT_CONFLICT);
    assert_eq!(p.events[0].message, ready.message);
    assert_eq!(p.requeue_after, None);
}

#[test]
fn row_19_anchor_reset() {
    let spec = secret_spec("{rotation: {interval: 90d}, fields: [{name: p}]}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = with_status(spec, &first);
    let mut damaged = secret.clone();
    damaged
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert(ANNOTATION_GENERATED_AT.to_owned(), "yesterday".to_owned());

    let p = plan(&spec, Some(&damaged), later(200), &mut seeded_rng(2));
    let desired = applied(&p);
    assert_eq!(desired.data, secret.data, "no rotation");
    assert_eq!(reasons(&p), [EVENT_ANCHOR_RESET]);
    assert!(p.events[0].warning);
    assert_eq!(
        p.events[0].message,
        format!(
            "annotation {ANNOTATION_GENERATED_AT} was not a valid RFC 3339 time; \
             the rotation anchor was reset to {}",
            later(200)
        )
    );
    assert_ready(&p);
    assert_eq!(
        annotation(&desired, ANNOTATION_GENERATED_AT),
        Some(later(200).to_string().as_str())
    );
    assert_eq!(p.status.last_rotation_time, None);
    assert_eq!(
        p.status.last_generated_time, first.status.last_generated_time,
        "nothing was generated"
    );
    assert_eq!(
        p.status.next_rotation_time.as_ref().map(|t| t.0),
        Some(later(290))
    );
    assert_eq!(p.requeue_after, Some(Duration::from_secs(90 * DAY)));
}
