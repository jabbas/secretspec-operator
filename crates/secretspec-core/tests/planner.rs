// SPDX-License-Identifier: Apache-2.0

//! `plan()` details beyond the behaviour table: status, events, comparison.

// Tests report through panics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::time::Duration;

use jiff::Timestamp;
use k8s_openapi::ByteString;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use secretspec_api::constants::{
    ANNOTATION_GENERATED_AT, ANNOTATION_KEYS, ANNOTATION_SPEC_UID, EVENT_INVALID_SPEC,
    EVENT_LOW_ENTROPY, EVENT_PASSWORD_TRUNCATED, LABEL_MANAGED_BY, LABEL_SPEC_NAME,
    MANAGED_BY_VALUE, REASON_GENERATED, REASON_INVALID_SPEC, REASON_WAITING_FOR_GC,
};
use secretspec_api::v1alpha1::SecretKind;
use secretspec_core::plan::{SecretAction, plan};
use support::{
    SPEC_NAME, SPEC_UID, annotation, applied, later, now, reasons, secret_spec, seeded_rng, value,
    with_status,
};

#[test]
fn same_generation_is_up_to_date() {
    let spec = secret_spec("fields: [{name: password}]");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);

    // Without a status nothing has been observed yet: re-apply, silently.
    let unobserved = plan(&spec, Some(&secret), now(), &mut seeded_rng(2));
    assert_eq!(applied(&unobserved), secret);
    assert!(unobserved.events.is_empty(), "{:?}", unobserved.events);
    assert_eq!(unobserved.status.observed_generation, Some(1));

    let spec = with_status(spec, &unobserved);
    let observed = plan(&spec, Some(&secret), now(), &mut seeded_rng(3));
    assert!(matches!(observed.action, SecretAction::UpToDate));
    assert!(observed.events.is_empty(), "{:?}", observed.events);
    assert_eq!(observed.status, unobserved.status);
}

#[test]
fn removed_spec_label_triggers_reapply() {
    let old = secret_spec("{secret: {labels: {team: a}}, fields: [{name: password}]}");
    let first = plan(&old, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    assert_eq!(secret.metadata.labels.as_ref().unwrap()["team"], "a");

    let mut new = secret_spec("fields: [{name: password}]");
    new.metadata.generation = Some(2);
    let new = with_status(new, &first);
    let p = plan(&new, Some(&secret), later(1), &mut seeded_rng(2));
    let desired = applied(&p);
    assert!(
        !desired
            .metadata
            .labels
            .as_ref()
            .unwrap()
            .contains_key("team")
    );
    assert_eq!(desired.data, secret.data);
    assert!(p.events.is_empty(), "{:?}", p.events);
    assert_eq!(p.status.observed_generation, Some(2));
    assert_eq!(p.status.conditions[0].observed_generation, Some(2));

    // After the re-apply (server-side apply dropped the label) the next
    // call with the recorded generation is up to date.
    let new = with_status(new, &p);
    let again = plan(&new, Some(&desired), later(2), &mut seeded_rng(3));
    assert!(matches!(again.action, SecretAction::UpToDate));
    assert!(again.events.is_empty(), "{:?}", again.events);
}

#[test]
fn foreign_metadata_and_owners_do_not_count() {
    let spec = secret_spec("fields: [{name: password}]");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let mut secret = applied(&first);
    let spec = with_status(spec, &first);
    secret
        .metadata
        .labels
        .as_mut()
        .unwrap()
        .insert("other".to_owned(), "x".to_owned());
    secret
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .insert("kubectl/last".to_owned(), "x".to_owned());
    secret
        .metadata
        .owner_references
        .as_mut()
        .unwrap()
        .push(OwnerReference {
            api_version: "v1".to_owned(),
            kind: "ConfigMap".to_owned(),
            name: "cm".to_owned(),
            uid: "other".to_owned(),
            controller: None,
            block_owner_deletion: None,
        });
    secret.metadata.resource_version = Some("42".to_owned());
    let second = plan(&spec, Some(&secret), now(), &mut seeded_rng(2));
    assert!(matches!(second.action, SecretAction::UpToDate));
}

#[test]
fn extra_data_key_or_changed_label_needs_apply() {
    let spec = secret_spec("fields: [{name: password}]");
    let secret = applied(&plan(&spec, None, now(), &mut seeded_rng(1)));

    let mut extra = secret.clone();
    extra
        .data
        .as_mut()
        .unwrap()
        .insert("stale".to_owned(), ByteString(b"x".to_vec()));
    let p = plan(&spec, Some(&extra), now(), &mut seeded_rng(2));
    let desired = applied(&p);
    assert!(!desired.data.as_ref().unwrap().contains_key("stale"));
    assert!(p.events.is_empty(), "{:?}", p.events);

    let mut relabelled = secret;
    relabelled
        .metadata
        .annotations
        .as_mut()
        .unwrap()
        .remove(ANNOTATION_SPEC_UID);
    let p = plan(&spec, Some(&relabelled), now(), &mut seeded_rng(2));
    assert!(matches!(p.action, SecretAction::Apply(_)));
}

#[test]
fn ssh_public_key_missing_is_rederived() {
    let spec = secret_spec("sshKeyPair: {}");
    let secret = applied(&plan(&spec, None, now(), &mut seeded_rng(1)));
    let mut damaged = secret.clone();
    damaged.data.as_mut().unwrap().remove("ssh-publickey");
    let p = plan(&spec, Some(&damaged), now(), &mut seeded_rng(2));
    let desired = applied(&p);
    assert_eq!(desired.data, secret.data);
    assert!(p.events.is_empty(), "{:?}", p.events);
    assert_eq!(
        annotation(&desired, ANNOTATION_KEYS),
        Some(r#"{"ssh/private-key":"ssh-privatekey","ssh/public-key":"ssh-publickey"}"#)
    );
}

#[test]
fn rotation_exactly_at_the_boundary_requeues_a_full_interval() {
    let spec = secret_spec("{rotation: {interval: 1h}, fields: [{name: p}]}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let one_hour_later = now().checked_add(Duration::from_secs(3600)).unwrap();
    let due = plan(&spec, Some(&secret), one_hour_later, &mut seeded_rng(2));
    assert_eq!(due.requeue_after, Some(Duration::from_secs(3600)));
}

#[test]
fn generated_at_has_seconds_precision() {
    let spec = secret_spec("fields: [{name: p}]");
    let fractional: Timestamp = "2026-10-19T12:00:00.987654321Z".parse().unwrap();
    let p = plan(&spec, None, fractional, &mut seeded_rng(1));
    let desired = applied(&p);
    assert_eq!(
        annotation(&desired, ANNOTATION_GENERATED_AT),
        Some("2026-10-19T12:00:00Z")
    );
    assert_eq!(
        p.status.last_generated_time.as_ref().map(|t| t.0),
        Some(now())
    );
    assert_eq!(p.status.conditions[0].last_transition_time.0, now());
}

#[test]
fn previous_owner_pending_gc_waits() {
    let spec = secret_spec("fields: [{name: password}]");
    let secret = applied(&plan(&spec, None, now(), &mut seeded_rng(1)));
    let mut recreated = secret_spec("fields: [{name: password}]");
    recreated.metadata.uid = Some("new-uid".to_owned());
    let previous = plan(&recreated, None, now(), &mut seeded_rng(3));
    let recreated = with_status(recreated, &previous);
    let p = plan(&recreated, Some(&secret), later(1), &mut seeded_rng(2));
    assert!(matches!(p.action, SecretAction::Nothing));
    assert!(p.events.is_empty(), "{:?}", p.events);
    assert_eq!(p.requeue_after, Some(Duration::from_secs(5)));
    let ready = &p.status.conditions[0];
    assert_eq!(ready.status, "False");
    assert_eq!(ready.reason, REASON_WAITING_FOR_GC);
    assert_eq!(ready.last_transition_time.0, later(1));
    assert_eq!(p.status.kind, Some(SecretKind::Fields));
    assert_eq!(
        p.status.last_generated_time, previous.status.last_generated_time,
        "times preserved"
    );
}

#[test]
fn invalid_spec_writes_nothing() {
    let spec = secret_spec("{fields: [{name: p}], sshKeyPair: {}}");
    let p = plan(&spec, None, now(), &mut seeded_rng(1));
    assert!(matches!(p.action, SecretAction::Nothing));
    assert_eq!(p.status.kind, None);
    assert_eq!(p.status.secret_name.as_deref(), Some(SPEC_NAME));
    assert_eq!(p.status.observed_generation, Some(1));
    let ready = &p.status.conditions[0];
    assert_eq!(ready.reason, REASON_INVALID_SPEC);
    assert_eq!(
        ready.message,
        "spec: exactly one of fields, sshKeyPair or basicAuth must be set"
    );
    assert_eq!(reasons(&p), [EVENT_INVALID_SPEC]);
    assert!(p.events[0].warning);
}

#[test]
fn low_entropy_is_reported_when_generated() {
    let spec = secret_spec("fields: [{name: pin, length: 6, charset: hex}, {name: ok}]");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    assert_eq!(
        first.events[1].reason, EVENT_LOW_ENTROPY,
        "{:?}",
        first.events
    );
    assert!(first.events[1].warning);
    assert_eq!(
        first.events[1].message,
        "keys with less than 128 bits of entropy: pin"
    );
    let secret = applied(&first);
    let again = plan(&spec, Some(&secret), now(), &mut seeded_rng(2));
    assert!(again.events.is_empty(), "kept values are not reported");
}

#[test]
fn long_basic_auth_password_warns_about_truncation() {
    let spec = secret_spec("basicAuth: {username: admin, password: {length: 70, prefix: abc}}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let truncated: Vec<_> = first
        .events
        .iter()
        .filter(|event| event.reason == EVENT_PASSWORD_TRUNCATED)
        .collect();
    assert_eq!(truncated.len(), 1);
    assert!(truncated[0].warning);
    assert_eq!(
        truncated[0].message,
        "password for key password is longer than 72 bytes; the htpasswd hash covers only the first 72 bytes"
    );
    let secret = applied(&first);
    let again = plan(&spec, Some(&secret), now(), &mut seeded_rng(2));
    assert!(again.events.is_empty(), "{:?}", again.events);

    let exact = secret_spec("basicAuth: {username: admin, password: {length: 72}}");
    let p = plan(&exact, None, now(), &mut seeded_rng(1));
    assert!(!reasons(&p).contains(&EVENT_PASSWORD_TRUNCATED));
}

#[test]
fn basic_auth_secret_carries_role_keys_and_username() {
    let spec = secret_spec("basicAuth: {username: admin, keys: {htpasswd: users}}");
    let p = plan(&spec, None, now(), &mut seeded_rng(1));
    let desired = applied(&p);
    assert_eq!(value(&desired, "username"), b"admin");
    assert!(value(&desired, "users").starts_with(b"admin:$2y$12$"));
    assert_eq!(p.events[0].message, "generated keys: password, users");
    assert_eq!(p.status.kind, Some(SecretKind::BasicAuth));
    assert_eq!(
        annotation(&desired, ANNOTATION_KEYS),
        Some(
            r#"{"basic-auth/htpasswd":"users","basic-auth/password":"password","basic-auth/username":"username"}"#
        )
    );
    let labels = desired.metadata.labels.as_ref().unwrap();
    assert_eq!(labels[LABEL_MANAGED_BY], MANAGED_BY_VALUE);
    assert_eq!(labels[LABEL_SPEC_NAME], SPEC_NAME);
    assert_eq!(p.status.conditions[0].reason, REASON_GENERATED);
    assert_eq!(
        desired.metadata.owner_references.as_ref().unwrap()[0].uid,
        SPEC_UID
    );
}
