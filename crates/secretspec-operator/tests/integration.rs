// SPDX-License-Identifier: Apache-2.0

//! The operator, in-process, against a real API server.
//!
//! Every test is `#[ignore]`d: it needs a disposable cluster. Run them with
//!
//! ```text
//! SECRETSPEC_TEST_KUBECONFIG=<path> \
//!   cargo test -p secretspec-operator --test integration -- --ignored --test-threads=1
//! ```
//!
//! See `docs/development.md` for creating a kind cluster. Each test creates
//! its own namespaces, ServiceAccount and RBAC (exactly design §9), runs the
//! operator on a tokio task with a client impersonating that ServiceAccount,
//! and deletes everything afterwards. The CRD is applied and left installed.
// The suite reports through panics, like any test.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
// Kubernetes kinds (ServiceAccount, SecretSpec, …) read as prose here.
#![allow(clippy::doc_markdown)]
// The emptiness checks carry their own context in the assertion messages.
#![allow(clippy::assert_is_empty)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::ByteString;
use k8s_openapi::api::core::v1::Secret;
use kube::ResourceExt;
use kube::api::{DeleteParams, Patch, PatchParams, PostParams};
use secretspec_api::constants::{
    ANNOTATION_FINGERPRINTS, ANNOTATION_GENERATED_AT, ANNOTATION_KEYS, ANNOTATION_SPEC_UID,
    LABEL_MANAGED_BY, LABEL_SPEC_NAME, MANAGED_BY_VALUE,
};
use secretspec_api::v1alpha1::SecretSpec;
use serde_json::json;
use support::{
    Env, Rbac, SECRET_VERBS, data, log_mark, logs_since, ready, reconciled_lines, wait_until,
    wait_until_for, with_env,
};

/// Waits until the operator has logged at least one reconcile of
/// `ns/name` after `mark`.
async fn wait_reconciled(ns: &str, name: &str, mark: usize) {
    wait_until(&format!("a reconcile of {ns}/{name}"), || async move {
        (!reconciled_lines(mark, ns, name).is_empty()).then_some(())
    })
    .await;
}

fn uid(spec: &SecretSpec) -> String {
    spec.uid().expect("SecretSpec has a uid")
}

fn assert_owned_by(secret: &Secret, spec: &SecretSpec) {
    let refs = secret.owner_references();
    assert_eq!(refs.len(), 1, "one owner reference: {refs:?}");
    let owner = &refs[0];
    assert_eq!(owner.kind, "SecretSpec");
    assert_eq!(owner.api_version, "secretspec.jabbas.eu/v1alpha1");
    assert_eq!(owner.name, spec.name_any());
    assert_eq!(owner.uid, uid(spec));
    assert_eq!(owner.controller, Some(true));
}

/// 1. The examples generate Secrets with the expected keys and metadata.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn examples_generate_secrets() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let operator = env.start_default();
        let cases: [(&str, &[&str]); 5] = [
            ("v01-minimal-fields.yaml", &["password"]),
            ("v03-ssh-default.yaml", &["ssh-privatekey", "ssh-publickey"]),
            (
                "v06-basic-auth-traefik.yaml",
                &["password", "username", "users"],
            ),
            ("v09-bytes-hex.yaml", &["rpc-secret"]),
            ("v12-rds-password.yaml", &["password"]),
        ];
        for (file, _) in &cases {
            env.create_fixture(&ns, file).await;
        }
        for (file, keys) in &cases {
            let name = file.trim_end_matches(".yaml");
            let spec = env.wait_ready(&ns, name).await;
            let condition = ready(&spec).unwrap();
            assert_eq!(condition.reason, "Generated", "{name}");
            let status = spec.status.as_ref().unwrap();
            assert_eq!(status.secret_name.as_deref(), Some(name));

            let secret = env.secrets(&ns).get(name).await.unwrap();
            let values = data(&secret);
            assert_eq!(
                values.keys().map(String::as_str).collect::<Vec<_>>(),
                *keys,
                "{name}: keys"
            );
            assert!(
                values.values().all(|v| !v.is_empty()),
                "{name}: empty value"
            );
            let labels = secret.labels();
            assert_eq!(
                labels.get(LABEL_MANAGED_BY).map(String::as_str),
                Some(MANAGED_BY_VALUE)
            );
            assert_eq!(labels.get(LABEL_SPEC_NAME).map(String::as_str), Some(name));
            let annotations = secret.annotations();
            assert!(annotations.contains_key(ANNOTATION_FINGERPRINTS), "{name}");
            assert!(annotations.contains_key(ANNOTATION_GENERATED_AT), "{name}");
            assert_eq!(
                annotations.get(ANNOTATION_SPEC_UID),
                Some(&uid(&spec)),
                "{name}"
            );
            let role_keys = matches!(
                *file,
                "v03-ssh-default.yaml" | "v06-basic-auth-traefik.yaml"
            );
            assert_eq!(
                annotations.contains_key(ANNOTATION_KEYS),
                role_keys,
                "{name}"
            );
            // Default deletionPolicy is Delete: a controller reference.
            assert_owned_by(&secret, &spec);

            let event = env.wait_event(&ns, name, None, "Generated").await;
            assert_eq!(event.type_.as_deref(), Some("Normal"));
        }

        let ssh = data(&env.secrets(&ns).get("v03-ssh-default").await.unwrap());
        assert!(ssh["ssh-privatekey"].starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(ssh["ssh-publickey"].starts_with("ssh-ed25519 "));
        let basic = data(
            &env.secrets(&ns)
                .get("v06-basic-auth-traefik")
                .await
                .unwrap(),
        );
        assert_eq!(basic["username"], "admin");
        assert_eq!(basic["password"].len(), 32);
        assert!(basic["users"].starts_with("admin:$2y$12$"));
        let hex = data(&env.secrets(&ns).get("v09-bytes-hex").await.unwrap());
        assert_eq!(hex["rpc-secret"].len(), 64);
        assert!(hex["rpc-secret"].bytes().all(|b| b.is_ascii_hexdigit()));
        let rds = data(&env.secrets(&ns).get("v12-rds-password").await.unwrap());
        assert!(!rds["password"].contains(['/', '@', '"', ' ']));

        operator.stop().await.unwrap();
    })
    .await;
}

/// 2. A restart of the operator changes no value and writes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn restart_changes_nothing() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let names = ["v01-minimal-fields", "v03-ssh-default"];
        let operator = env.start_default();
        for name in names {
            env.create_fixture(&ns, &format!("{name}.yaml")).await;
        }
        for name in names {
            env.wait_ready(&ns, name).await;
        }
        operator.stop().await.unwrap();

        let mut before = Vec::new();
        for name in names {
            let secret = env.secrets(&ns).get(name).await.unwrap();
            let spec = env.specs(&ns).get(name).await.unwrap();
            before.push((secret, spec));
        }

        let mark = log_mark();
        let operator = env.start_default();
        for name in names {
            wait_reconciled(&ns, name, mark).await;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
        for (name, (secret, spec)) in names.iter().zip(&before) {
            let now = env.secrets(&ns).get(name).await.unwrap();
            assert_eq!(data(&now), data(secret), "{name}: values changed");
            assert_eq!(
                now.resource_version(),
                secret.resource_version(),
                "{name}: Secret was written"
            );
            let spec_now = env.specs(&ns).get(name).await.unwrap();
            assert_eq!(
                spec_now.resource_version(),
                spec.resource_version(),
                "{name}: SecretSpec status was written"
            );
            for line in reconciled_lines(mark, &ns, name) {
                assert!(line.contains("UpToDate"), "{name}: {line}");
            }
        }
        operator.stop().await.unwrap();
    })
    .await;
}

fn app_spec(name: &str) -> serde_json::Value {
    json!({
        "apiVersion": "secretspec.jabbas.eu/v1alpha1",
        "kind": "SecretSpec",
        "metadata": { "name": name },
        "spec": {
            "data": { "username": "app" },
            "fields": [ { "name": "password" }, { "name": "token", "length": 48 } ]
        }
    })
}

/// 3. Removing one generated key regenerates only that key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn deleting_one_key_regenerates_only_it() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let name = "app";
        let operator = env.start_default();
        let spec = env.create_spec(&ns, app_spec(name)).await;
        env.wait_ready(&ns, name).await;
        let before = data(&env.secrets(&ns).get(name).await.unwrap());

        // A merge patch with `null` removes the key (RFC 7386), like a JSON
        // patch `remove`, without the `jsonpatch` kube feature.
        env.secrets(&ns)
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Merge(json!({ "data": { "password": null } })),
            )
            .await
            .unwrap();

        let after = env
            .wait_secret(&ns, name, "password regenerated", |s| {
                data(s).contains_key("password")
            })
            .await;
        let after = data(&after);
        assert_ne!(after["password"], before["password"]);
        assert_eq!(after["token"], before["token"]);
        assert_eq!(after["username"], "app");
        assert_eq!(after.len(), 3);
        let event = env
            .wait_event(&ns, name, Some(&uid(&spec)), "Regenerated")
            .await;
        assert_eq!(event.type_.as_deref(), Some("Normal"));
        env.wait_ready(&ns, name).await;
        operator.stop().await.unwrap();
    })
    .await;
}

/// 4. Deleting the Secret regenerates every value; literals stay.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn deleting_the_secret_regenerates_everything() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let name = "app";
        let operator = env.start_default();
        let spec = env.create_spec(&ns, app_spec(name)).await;
        env.wait_ready(&ns, name).await;
        let old = env.secrets(&ns).get(name).await.unwrap();
        let old_uid = old.uid().unwrap();

        env.secrets(&ns)
            .delete(name, &DeleteParams::default())
            .await
            .unwrap();
        let new = env
            .wait_secret(&ns, name, "recreated", |s| {
                s.uid().as_deref() != Some(old_uid.as_str())
            })
            .await;
        let (old, new_values) = (data(&old), data(&new));
        assert_ne!(new_values["password"], old["password"]);
        assert_ne!(new_values["token"], old["token"]);
        assert_eq!(new_values["username"], "app");
        assert_owned_by(&new, &spec);
        env.wait_ready(&ns, name).await;
        operator.stop().await.unwrap();
    })
    .await;
}

fn retained_spec(name: &str) -> serde_json::Value {
    json!({
        "apiVersion": "secretspec.jabbas.eu/v1alpha1",
        "kind": "SecretSpec",
        "metadata": { "name": name },
        "spec": { "deletionPolicy": "Retain", "fields": [ { "name": "password" } ] }
    })
}

/// 5. `Retain`: the Secret outlives its SecretSpec and is adopted, values
/// unchanged, by a recreated one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn retain_survives_spec_recreate() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let name = "kept";
        let operator = env.start_default();
        env.create_spec(&ns, retained_spec(name)).await;
        env.wait_ready(&ns, name).await;
        let before = env.secrets(&ns).get(name).await.unwrap();
        assert!(before.owner_references().is_empty(), "Retain: no owner");

        env.specs(&ns)
            .delete(name, &DeleteParams::default())
            .await
            .unwrap();
        wait_until("SecretSpec deleted", || async {
            env.specs(&ns)
                .get_opt(name)
                .await
                .unwrap()
                .is_none()
                .then_some(())
        })
        .await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let survivor = env.secrets(&ns).get(name).await.unwrap();
        assert_eq!(data(&survivor), data(&before));

        let recreated = env.create_spec(&ns, retained_spec(name)).await;
        let new_uid = uid(&recreated);
        env.wait_ready(&ns, name).await;
        env.wait_event(&ns, name, Some(&new_uid), "Adopted").await;
        let adopted = env
            .wait_secret(&ns, name, "spec-uid of the new SecretSpec", |s| {
                s.annotations().get(ANNOTATION_SPEC_UID) == Some(&new_uid)
            })
            .await;
        assert_eq!(data(&adopted), data(&before), "values changed on adoption");
        assert!(adopted.owner_references().is_empty());
        operator.stop().await.unwrap();
    })
    .await;
}

/// 6. `Delete`: garbage collection removes the Secret with its SecretSpec.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn delete_policy_garbage_collects() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let name = "v01-minimal-fields";
        let operator = env.start_default();
        env.create_fixture(&ns, "v01-minimal-fields.yaml").await;
        env.wait_ready(&ns, name).await;
        env.secrets(&ns).get(name).await.unwrap();
        env.specs(&ns)
            .delete(name, &DeleteParams::default())
            .await
            .unwrap();
        env.wait_secret_gone(&ns, name).await;
        operator.stop().await.unwrap();
    })
    .await;
}

/// 7. Namespace mode: only listed namespaces are reconciled, and no request
/// needs more than the namespaced RBAC.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn namespace_mode_ignores_other_namespaces() {
    with_env(Rbac::Namespace, |env: Arc<Env>| async move {
        let (ns_a, ns_b) = (env.ns_a.clone(), env.ns_b.clone());
        let name = "v01-minimal-fields";
        let mark = log_mark();
        let config = env.config();
        assert_eq!(config.watch_namespaces(), vec![ns_a.clone()]);
        let operator = env.start(config);
        env.create_fixture(&ns_a, "v01-minimal-fields.yaml").await;
        env.create_fixture(&ns_b, "v01-minimal-fields.yaml").await;
        env.wait_ready(&ns_a, name).await;
        env.secrets(&ns_a).get(name).await.unwrap();

        tokio::time::sleep(Duration::from_secs(5)).await;
        let other = env.specs(&ns_b).get(name).await.unwrap();
        assert!(
            other.status.is_none(),
            "{ns_b} was reconciled: {:?}",
            other.status
        );
        assert!(env.secrets(&ns_b).get_opt(name).await.unwrap().is_none());
        assert!(env.event_reasons(&ns_b, name, None).await.is_empty());
        assert!(reconciled_lines(mark, &ns_b, name).is_empty());

        let logs = logs_since(mark);
        for needle in ["403", "Forbidden"] {
            assert!(!logs.contains(needle), "operator logged {needle}:\n{logs}");
        }
        operator.stop().await.unwrap();
    })
    .await;
}

/// 8. A rotation that is due runs on the next reconcile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn rotation_with_short_interval() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let name = "rotating";
        let operator = env.start_default();
        let spec = env
            .create_spec(
                &ns,
                json!({
                    "apiVersion": "secretspec.jabbas.eu/v1alpha1",
                    "kind": "SecretSpec",
                    "metadata": { "name": name },
                    "spec": { "rotation": { "interval": "1h" }, "fields": [ { "name": "token" } ] }
                }),
            )
            .await;
        let ready_spec = env.wait_ready(&ns, name).await;
        assert!(
            ready_spec
                .status
                .as_ref()
                .and_then(|s| s.next_rotation_time.as_ref())
                .is_some(),
            "rotation is scheduled"
        );
        let before = data(&env.secrets(&ns).get(name).await.unwrap());

        let two_hours_ago = jiff::Timestamp::now()
            .checked_sub(jiff::SignedDuration::from_hours(2))
            .unwrap()
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        env.secrets(&ns)
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata": {
                        "labels": { "example.com/touch": "1" },
                        "annotations": { ANNOTATION_GENERATED_AT: two_hours_ago }
                    }
                })),
            )
            .await
            .unwrap();

        let rotated = env
            .wait_secret(&ns, name, "token rotated", |s| {
                data(s).get("token") != before.get("token")
                    && s.annotations().get(ANNOTATION_GENERATED_AT) != Some(&two_hours_ago)
            })
            .await;
        let anchor: jiff::Timestamp = rotated.annotations()[ANNOTATION_GENERATED_AT]
            .parse()
            .unwrap();
        let age = jiff::Timestamp::now().duration_since(anchor);
        assert!(
            age < jiff::SignedDuration::from_secs(120),
            "anchor {anchor}"
        );
        let event = env
            .wait_event(&ns, name, Some(&uid(&spec)), "Rotated")
            .await;
        assert_eq!(event.type_.as_deref(), Some("Normal"));
        let after = env
            .wait_spec(&ns, name, "lastRotationTime recorded", |s| {
                s.status
                    .as_ref()
                    .is_some_and(|s| s.last_rotation_time.is_some())
            })
            .await;
        assert!(ready(&after).is_some_and(|c| c.status == "True"));
        operator.stop().await.unwrap();
    })
    .await;
}

/// 9. A key added by another field manager stays, and causes no re-apply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn foreign_key_survives_and_no_reapply_loop() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let name = "v01-minimal-fields";
        let operator = env.start_default();
        let spec = env.create_fixture(&ns, "v01-minimal-fields.yaml").await;
        env.wait_ready(&ns, name).await;
        let before = data(&env.secrets(&ns).get(name).await.unwrap());

        let mark = log_mark();
        let foreign = env
            .secrets(&ns)
            .patch(
                name,
                &PatchParams::apply("secretspec-it-foreign"),
                &Patch::Apply(json!({
                    "apiVersion": "v1",
                    "kind": "Secret",
                    "metadata": { "name": name, "namespace": ns },
                    "data": { "extra": ByteString(b"foreign".to_vec()) }
                })),
            )
            .await
            .unwrap();
        let rv = foreign.resource_version();
        wait_reconciled(&ns, name, mark).await;
        tokio::time::sleep(Duration::from_secs(10)).await;

        let now = env.secrets(&ns).get(name).await.unwrap();
        assert_eq!(now.resource_version(), rv, "Secret was rewritten");
        let values = data(&now);
        assert_eq!(values.get("extra").map(String::as_str), Some("foreign"));
        assert_eq!(values["password"], before["password"]);
        let lines = reconciled_lines(mark, &ns, name);
        assert!(!lines.is_empty());
        for line in &lines {
            assert!(line.contains("UpToDate"), "re-apply: {line}");
        }
        let reasons = env.event_reasons(&ns, name, Some(&uid(&spec))).await;
        assert_eq!(reasons, ["Generated"], "unexpected events");
        operator.stop().await.unwrap();
    })
    .await;
}

/// 10. An unmanaged Secret with the same name is a conflict and stays as is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn conflict_with_unmanaged_secret() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let name = "v01-minimal-fields";
        let existing: Secret = serde_json::from_value(json!({
            "metadata": { "name": name, "namespace": ns },
            "stringData": { "foo": "bar" }
        }))
        .unwrap();
        let existing = env
            .secrets(&ns)
            .create(&PostParams::default(), &existing)
            .await
            .unwrap();

        let operator = env.start_default();
        env.create_fixture(&ns, "v01-minimal-fields.yaml").await;
        env.wait_not_ready(&ns, name, "Conflict").await;
        env.wait_event(&ns, name, None, "Conflict").await;
        tokio::time::sleep(Duration::from_secs(3)).await;

        let now = env.secrets(&ns).get(name).await.unwrap();
        assert_eq!(now.resource_version(), existing.resource_version());
        assert_eq!(data(&now), data(&existing));
        assert!(now.labels().is_empty());
        assert!(now.owner_references().is_empty());
        operator.stop().await.unwrap();
    })
    .await;
}

/// 11. A rejected apply is reported, does not advance `observedGeneration`,
/// and is retried until it succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn apply_failure_is_reported_and_retried() {
    with_env(Rbac::Namespace, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let name = "app";
        let revoked: &[&str] = &["get", "list", "watch", "update"];
        env.set_secret_verbs(revoked).await;
        let operator = env.start_default();

        // A new SecretSpec whose first apply is rejected.
        let spec = env.create_spec(&ns, app_spec(name)).await;
        let spec_uid = uid(&spec);
        let failed = env.wait_not_ready(&ns, name, "ApplyFailed").await;
        let condition = ready(&failed).unwrap();
        assert!(condition.message.contains("403"), "{}", condition.message);
        let status = failed.status.as_ref().unwrap();
        assert_eq!(
            status.observed_generation, None,
            "observedGeneration advanced"
        );
        assert_eq!(status.last_generated_time, None);
        let event = env
            .wait_event(&ns, name, Some(&spec_uid), "ApplyFailed")
            .await;
        assert_eq!(event.type_.as_deref(), Some("Warning"));
        assert!(env.secrets(&ns).get_opt(name).await.unwrap().is_none());

        env.set_secret_verbs(SECRET_VERBS).await;
        let recovered = env.wait_ready(&ns, name).await;
        assert_eq!(recovered.status.unwrap().observed_generation, Some(1));
        let reasons = env.event_reasons(&ns, name, Some(&spec_uid)).await;
        assert!(reasons.iter().any(|r| r == "Generated"), "{reasons:?}");

        // An update whose apply is rejected keeps the previous generation.
        let before = data(&env.secrets(&ns).get(name).await.unwrap());
        env.set_secret_verbs(revoked).await;
        env.specs(&ns)
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Merge(json!({ "spec": { "data": { "username": "app2" } } })),
            )
            .await
            .unwrap();
        let failed = env.wait_not_ready(&ns, name, "ApplyFailed").await;
        assert_eq!(failed.metadata.generation, Some(2));
        let status = failed.status.as_ref().unwrap();
        assert_eq!(
            status.observed_generation,
            Some(1),
            "observedGeneration advanced"
        );
        assert_eq!(data(&env.secrets(&ns).get(name).await.unwrap()), before);

        env.set_secret_verbs(SECRET_VERBS).await;
        let recovered = env.wait_ready(&ns, name).await;
        assert_eq!(recovered.status.unwrap().observed_generation, Some(2));
        let after = data(&env.secrets(&ns).get(name).await.unwrap());
        assert_eq!(after["username"], "app2");
        assert_eq!(after["password"], before["password"]);
        operator.stop().await.unwrap();
    })
    .await;
}

/// 12. With leader election only the Lease holder reconciles, and a stopped
/// leader hands over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn leader_election_single_active() {
    with_env(Rbac::Cluster, |env: Arc<Env>| async move {
        let ns = env.ns_a.clone();
        let leader_config = |identity: &str| {
            let mut config = env.config();
            config.leader_election = true;
            config.pod_name = Some(format!("it-{}-{identity}", env.id));
            config
        };
        let (one_id, two_id) = (format!("it-{}-one", env.id), format!("it-{}-two", env.id));
        let one = env.start(leader_config("one"));
        wait_until("operator one to lead", || async {
            (env.lease_holder().await.as_deref() == Some(one_id.as_str())).then_some(())
        })
        .await;
        let two = env.start(leader_config("two"));

        let first = env.create_fixture(&ns, "v01-minimal-fields.yaml").await;
        env.wait_ready(&ns, "v01-minimal-fields").await;
        let settled = env.specs(&ns).get("v01-minimal-fields").await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(env.lease_holder().await.as_deref(), Some(one_id.as_str()));
        let now = env.specs(&ns).get("v01-minimal-fields").await.unwrap();
        assert_eq!(
            now.resource_version(),
            settled.resource_version(),
            "status written again"
        );
        let events = env
            .events(&ns, "v01-minimal-fields", Some(&uid(&first)))
            .await;
        assert!(!events.is_empty());
        for event in &events {
            assert_eq!(event.reporting_instance.as_deref(), Some(one_id.as_str()));
        }
        assert!(!two.is_finished(), "stand-by operator exited");

        let secret = env.secrets(&ns).get("v01-minimal-fields").await.unwrap();
        one.stop().await.unwrap();
        wait_until_for(
            "operator two to take over",
            Duration::from_secs(30),
            || async {
                (env.lease_holder().await.as_deref() == Some(two_id.as_str())).then_some(())
            },
        )
        .await;

        let second = env.create_fixture(&ns, "v09-bytes-hex.yaml").await;
        env.wait_ready(&ns, "v09-bytes-hex").await;
        let events = env.events(&ns, "v09-bytes-hex", Some(&uid(&second))).await;
        assert!(!events.is_empty());
        for event in &events {
            assert_eq!(event.reporting_instance.as_deref(), Some(two_id.as_str()));
        }
        let unchanged = env.secrets(&ns).get("v01-minimal-fields").await.unwrap();
        assert_eq!(unchanged.resource_version(), secret.resource_version());
        two.stop().await.unwrap();
        assert_eq!(env.lease_holder().await, None, "lease not released");
    })
    .await;
}
