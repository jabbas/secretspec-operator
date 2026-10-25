// SPDX-License-Identifier: Apache-2.0

//! Properties of `plan()` that hold across inputs: idempotence, secrecy of
//! generated values, type conflicts, rotation anchors and lost metadata.

// Tests report through panics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod support;

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use secretspec_api::constants::{
    ANNOTATION_FINGERPRINTS, ANNOTATION_GENERATED_AT, EVENT_ADOPTED, EVENT_ANCHOR_RESET,
    EVENT_CONFLICT, EVENT_FINGERPRINTS_RESET, EVENT_ROTATED, REASON_CONFLICT, REASON_GENERATED,
};
use secretspec_api::v1alpha1::SecretSpec;
use secretspec_core::plan::{CONFLICT_REQUEUE, SecretAction, plan};
use support::{
    DAY, annotation, applied, earlier, later, now, reasons, secret_spec, seeded_rng, with_status,
};

/// The valid fixtures of the API crate as `(file name, SecretSpec)`, with
/// name, namespace, UID and generation set. `rsa` selects the fixtures whose
/// key pair is RSA, which take minutes to generate in a debug build, or all
/// the others.
fn fixtures(rsa: bool) -> Vec<(String, SecretSpec)> {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../secretspec-api/tests/fixtures/valid");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("fixture directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    paths.sort();
    assert!(paths.len() >= 19, "found {} fixtures", paths.len());
    paths
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).expect("fixture readable");
            let mut spec: SecretSpec = serde_norway::from_str(&text).expect("fixture parses");
            spec.metadata.namespace = Some("test".to_owned());
            spec.metadata.uid = Some(support::SPEC_UID.to_owned());
            spec.metadata.generation = Some(1);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, spec)
        })
        .filter(|(_, spec)| {
            let is_rsa = serde_json::to_string(&spec.spec)
                .expect("spec serializes")
                .contains("\"rsa\"");
            is_rsa == rsa
        })
        .collect()
}

/// Plan, feed the result back, plan again: nothing to do.
fn check_idempotent(name: &str, spec: SecretSpec) {
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = match &first.action {
        SecretAction::Apply(secret) => (**secret).clone(),
        other => panic!("{name}: expected Apply, got {other:?}"),
    };
    let fed_back = with_status(spec, &first);
    // A minute later (inside the shortest rotation interval) with a
    // different RNG: reading back what was written changes nothing.
    let at = now().checked_add(Duration::from_secs(60)).unwrap();
    let second = plan(&fed_back, Some(&secret), at, &mut seeded_rng(2));
    assert!(
        matches!(second.action, SecretAction::UpToDate),
        "{name}: {:?}",
        second.action
    );
    assert!(second.events.is_empty(), "{name}: {:?}", second.events);
    assert_eq!(second.status, first.status, "{name}: status changed");
}

/// No line of any generated value appears in the `Debug` output of the plan
/// (events and status included).
fn check_not_logged(name: &str, spec: &SecretSpec) {
    let p = plan(spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&p);
    let renderings = [format!("{p:?}"), format!("{p:#?}")];
    let mut checked = 0;
    for (key, bytes) in secret.data.as_ref().unwrap() {
        let text = String::from_utf8_lossy(&bytes.0);
        // Multi-line values (PEM) leak line by line. Lines under 8 bytes are
        // skipped: a short value could coincide with a key name by chance.
        for line in text.lines().map(str::trim).filter(|line| line.len() >= 8) {
            let escaped = format!("{line:?}");
            let escaped = &escaped[1..escaped.len() - 1];
            for rendering in &renderings {
                assert!(!rendering.contains(line), "{name}: Debug leaks {key}");
                assert!(!rendering.contains(escaped), "{name}: Debug leaks {key}");
            }
            checked += 1;
        }
    }
    assert!(checked > 0, "{name}: nothing was checked");
    for rendering in &renderings {
        assert!(rendering.contains("data_keys"), "{name}: key names shown");
    }
}

#[test]
fn planner_is_idempotent_for_every_fixture() {
    for (name, spec) in fixtures(false) {
        check_idempotent(&name, spec);
    }
}

#[test]
#[ignore = "slow: RSA 3072"]
fn planner_is_idempotent_for_rsa_fixtures() {
    let rsa = fixtures(true);
    assert_eq!(rsa.len(), 1, "the v04 fixture");
    for (name, spec) in rsa {
        check_idempotent(&name, spec);
    }
}

#[test]
fn values_never_logged() {
    for (name, spec) in fixtures(false) {
        check_not_logged(&name, &spec);
    }
}

#[test]
#[ignore = "slow: RSA 3072"]
fn values_never_logged_for_rsa_fixtures() {
    let rsa = fixtures(true);
    assert_eq!(rsa.len(), 1, "the v04 fixture");
    for (name, spec) in rsa {
        check_not_logged(&name, &spec);
    }
}

#[test]
fn type_mismatch_is_conflict() {
    const FIELDS: &str = "fields: [{name: a}]";
    const CUSTOM: &str = "{secret: {type: example.com/custom}, fields: [{name: a}]}";
    const BASIC: &str = "basicAuth: {username: admin}";
    const BASIC_TYPED: &str =
        "{secret: {type: kubernetes.io/basic-auth}, basicAuth: {username: admin}}";
    const SSH_TYPED: &str = "{secret: {type: kubernetes.io/ssh-auth}, sshKeyPair: {}}";

    // (spec, type of the existing Secret, type the spec asks for when that
    // differs from the existing one). `None` and the empty string both read
    // as `Opaque`; a spec without `secret.type` asks for `Opaque`.
    let cases = [
        (FIELDS, Some("kubernetes.io/ssh-auth"), Some("Opaque")),
        (FIELDS, Some("example.com/custom"), Some("Opaque")),
        (CUSTOM, Some("Opaque"), Some("example.com/custom")),
        (CUSTOM, None, Some("example.com/custom")),
        (CUSTOM, Some(""), Some("example.com/custom")),
        (
            CUSTOM,
            Some("example.com/other"),
            Some("example.com/custom"),
        ),
        (
            BASIC_TYPED,
            Some("Opaque"),
            Some("kubernetes.io/basic-auth"),
        ),
        (BASIC, Some("kubernetes.io/basic-auth"), Some("Opaque")),
        (SSH_TYPED, Some("Opaque"), Some("kubernetes.io/ssh-auth")),
        // Compatible: no conflict.
        (FIELDS, None, None),
        (FIELDS, Some(""), None),
        (FIELDS, Some("Opaque"), None),
        (CUSTOM, Some("example.com/custom"), None),
        (BASIC_TYPED, Some("kubernetes.io/basic-auth"), None),
        (SSH_TYPED, Some("kubernetes.io/ssh-auth"), None),
    ];

    for (yaml, existing_type, conflict) in cases {
        // Owned (same UID) and adoptable (recreated SecretSpec) alike.
        for adopt in [false, true] {
            let first = plan(&secret_spec(yaml), None, now(), &mut seeded_rng(1));
            let mut secret = applied(&first);
            secret.type_ = existing_type.map(str::to_owned);

            let mut spec = secret_spec(yaml);
            if adopt {
                spec.metadata.uid = Some("new-uid".to_owned());
                secret.metadata.owner_references = None;
            }
            let spec = with_status(spec, &first);
            let p = plan(&spec, Some(&secret), later(1), &mut seeded_rng(2));
            let case = format!("{yaml} over {existing_type:?} (adopt: {adopt})");

            let ready = &p.status.conditions[0];
            let Some(want) = conflict else {
                assert!(
                    !matches!(p.action, SecretAction::Nothing),
                    "{case}: no conflict expected"
                );
                assert_eq!(ready.reason, REASON_GENERATED, "{case}");
                let expected: &[&str] = if adopt { &[EVENT_ADOPTED] } else { &[] };
                assert_eq!(reasons(&p), expected, "{case}");
                continue;
            };

            assert!(matches!(p.action, SecretAction::Nothing), "{case}");
            assert_eq!(ready.status, "False", "{case}");
            assert_eq!(ready.reason, REASON_CONFLICT, "{case}");
            let have = existing_type
                .filter(|ty| !ty.is_empty())
                .unwrap_or("Opaque");
            assert_eq!(
                ready.message,
                format!("Secret db has type {have}; delete it to recreate it with type {want}"),
                "{case}"
            );
            assert_eq!(reasons(&p), [EVENT_CONFLICT], "{case}");
            assert!(p.events[0].warning, "{case}");
            assert_eq!(
                p.status.last_generated_time, first.status.last_generated_time,
                "{case}: times preserved"
            );
            assert_eq!(p.requeue_after, Some(CONFLICT_REQUEUE), "{case}");
        }
    }
}

#[test]
fn rotation_anchor_edge_cases() {
    let spec = secret_spec("{rotation: {interval: 90d}, fields: [{name: p}]}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = with_status(spec, &first);
    let with_anchor = |text: &str| {
        let mut damaged = secret.clone();
        damaged
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(ANNOTATION_GENERATED_AT.to_owned(), text.to_owned());
        damaged
    };

    // An anchor in the future (clock skew): no rotation now, none endlessly;
    // the next one falls due a full interval after the anchor.
    for days in [1, 30, 3650] {
        let anchor = later(days);
        let damaged = with_anchor(&anchor.to_string());
        let p = plan(&spec, Some(&damaged), now(), &mut seeded_rng(2));
        let case = format!("anchor in {days} days");
        assert!(
            !matches!(p.action, SecretAction::Nothing),
            "{case}: {:?}",
            p.action
        );
        if let SecretAction::Apply(desired) = &p.action {
            assert_eq!(desired.data, secret.data, "{case}: no rotation");
            assert_eq!(
                annotation(desired, ANNOTATION_GENERATED_AT),
                Some(anchor.to_string().as_str()),
                "{case}: anchor kept"
            );
        } else {
            // Nothing else changes, so the Secret may already be up to date.
            assert!(matches!(p.action, SecretAction::UpToDate), "{case}");
        }
        assert!(p.events.is_empty(), "{case}: {:?}", p.events);
        assert_eq!(
            p.status.next_rotation_time.as_ref().map(|t| t.0),
            Some(later(days + 90)),
            "{case}"
        );
        assert_eq!(
            p.requeue_after,
            Some(Duration::from_secs((days + 90) * DAY)),
            "{case}"
        );
        assert_eq!(p.status.last_rotation_time, None, "{case}");
    }

    // An anchor so far out that anchor + interval overflows: no rotation, no
    // timer, no panic.
    let damaged = with_anchor("9999-12-30T00:00:00Z");
    let p = plan(&spec, Some(&damaged), now(), &mut seeded_rng(2));
    assert!(p.events.is_empty(), "{:?}", p.events);
    assert_eq!(p.status.next_rotation_time, None);
    assert_eq!(p.requeue_after, None);
    if let SecretAction::Apply(desired) = &p.action {
        assert_eq!(desired.data, secret.data);
    }

    // An unreadable anchor: reset to now, warn, never rotate.
    for text in ["yesterday", "", "2026-13-45T00:00:00Z", "1700000000"] {
        let damaged = with_anchor(text);
        let p = plan(&spec, Some(&damaged), later(500), &mut seeded_rng(2));
        let desired = applied(&p);
        assert_eq!(desired.data, secret.data, "{text:?}: no rotation");
        assert_eq!(reasons(&p), [EVENT_ANCHOR_RESET], "{text:?}");
        assert!(p.events[0].warning, "{text:?}");
        assert_eq!(
            annotation(&desired, ANNOTATION_GENERATED_AT),
            Some(later(500).to_string().as_str()),
            "{text:?}"
        );
        assert_eq!(
            p.status.next_rotation_time.as_ref().map(|t| t.0),
            Some(later(590)),
            "{text:?}"
        );
        assert_eq!(p.status.last_rotation_time, None, "{text:?}");
    }

    // The reset is stable: with the new anchor the next call is up to date.
    let damaged = with_anchor("yesterday");
    let reset = plan(&spec, Some(&damaged), later(500), &mut seeded_rng(2));
    let repaired = applied(&reset);
    let spec = with_status(spec, &reset);
    let again = plan(&spec, Some(&repaired), later(501), &mut seeded_rng(3));
    assert!(matches!(again.action, SecretAction::UpToDate));
    assert!(again.events.is_empty(), "{:?}", again.events);

    // An anchor exactly one interval old is due; one second younger is not.
    let due = with_anchor(&earlier(90).to_string());
    let p = plan(&spec, Some(&due), now(), &mut seeded_rng(2));
    assert_eq!(reasons(&p), [EVENT_ROTATED]);
    let young = with_anchor(
        &earlier(90)
            .checked_add(Duration::from_secs(1))
            .unwrap()
            .to_string(),
    );
    let p = plan(&spec, Some(&young), now(), &mut seeded_rng(2));
    assert!(p.events.is_empty(), "{:?}", p.events);
}

#[test]
fn fingerprints_lost_keeps_values() {
    let spec = secret_spec("{basicAuth: {username: admin}}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let secret = applied(&first);
    let spec = with_status(spec, &first);
    let original = annotation(&secret, ANNOTATION_FINGERPRINTS)
        .unwrap()
        .to_owned();

    // The annotation with one slot's entry replaced by garbage.
    let mut object: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&original).unwrap();
    assert_eq!(object.len(), 2, "{original}");
    object.insert(
        "basic-auth/password".to_owned(),
        serde_json::Value::String("NOT-HEX".to_owned()),
    );
    let one_invalid = serde_json::to_string(&object).unwrap();

    // (annotation after damage, keys the warning names)
    let cases: [(Option<&str>, &str); 5] = [
        (None, "auth, password"),
        (Some("x"), "auth, password"),
        (Some("{}"), "auth, password"),
        (Some("[]"), "auth, password"),
        (Some(one_invalid.as_str()), "password"),
    ];
    for (broken, named) in cases {
        let mut damaged = secret.clone();
        let annotations = damaged.metadata.annotations.as_mut().unwrap();
        match broken {
            None => annotations.remove(ANNOTATION_FINGERPRINTS),
            Some(text) => annotations.insert(ANNOTATION_FINGERPRINTS.to_owned(), text.to_owned()),
        };
        let p = plan(&spec, Some(&damaged), later(1), &mut seeded_rng(2));
        let desired = applied(&p);
        assert_eq!(desired.data, secret.data, "{broken:?}: values kept");
        assert_eq!(
            annotation(&desired, ANNOTATION_FINGERPRINTS),
            Some(original.as_str()),
            "{broken:?}: re-stamped"
        );
        assert_eq!(reasons(&p), [EVENT_FINGERPRINTS_RESET], "{broken:?}");
        assert!(p.events[0].warning, "{broken:?}");
        assert_eq!(
            p.events[0].message,
            format!(
                "kept existing values for keys: {named} (fingerprint metadata was missing or invalid)"
            ),
            "{broken:?}"
        );
        assert_eq!(
            p.status.last_generated_time, first.status.last_generated_time,
            "{broken:?}: nothing was generated"
        );

        // Re-stamped metadata is stable: the next reconcile is quiet.
        let spec = with_status(spec.clone(), &p);
        let again = plan(&spec, Some(&desired), later(2), &mut seeded_rng(3));
        assert!(matches!(again.action, SecretAction::UpToDate), "{broken:?}");
        assert!(again.events.is_empty(), "{broken:?}: {:?}", again.events);
    }
}

#[test]
fn generation_failure_is_retryable() {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use secretspec_api::constants::{EVENT_GENERATION_FAILED, REASON_GENERATION_FAILED};
    use secretspec_api::v1alpha1::SecretKind;
    use secretspec_core::generate::GenerateError;
    use secretspec_core::plan::halt_for_generate_error;

    let spec = secret_spec("{rotation: {interval: 90d}, fields: [{name: password}]}");
    let first = plan(&spec, None, now(), &mut seeded_rng(1));
    let mut spec = with_status(spec, &first);
    let status = spec.status.as_mut().expect("status");
    status.last_generated_time = Some(Time(earlier(3)));
    status.last_rotation_time = Some(Time(earlier(2)));
    status.next_rotation_time = Some(Time(later(80)));
    let before = spec.status.clone().expect("status");

    for (error, display) in [
        (
            GenerateError::Ssh("encode".to_owned()),
            "value generation failed: ssh key generation failed: encode",
        ),
        (
            GenerateError::Bcrypt,
            "value generation failed: bcrypt failed",
        ),
    ] {
        let p = halt_for_generate_error(&error, &spec, SecretKind::Fields, now());
        assert!(matches!(p.action, SecretAction::Nothing), "{error}");
        assert_eq!(p.requeue_after, Some(Duration::from_secs(30)), "{error}");

        let ready = &p.status.conditions[0];
        assert_eq!(ready.status, "False", "{error}");
        assert_eq!(ready.reason, REASON_GENERATION_FAILED, "{error}");
        assert_eq!(ready.message, display);

        assert_eq!(reasons(&p), vec![EVENT_GENERATION_FAILED], "{error}");
        assert!(p.events[0].warning, "{error}");
        assert_eq!(p.events[0].message, display);

        assert_eq!(p.status.kind, Some(SecretKind::Fields), "{error}");
        assert_eq!(p.status.last_generated_time, before.last_generated_time);
        assert_eq!(p.status.last_rotation_time, before.last_rotation_time);
        assert_eq!(p.status.next_rotation_time, before.next_rotation_time);
    }
}
