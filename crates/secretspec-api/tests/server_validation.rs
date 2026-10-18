// SPDX-License-Identifier: Apache-2.0

//! Server-side validation of the generated CRD against a real API server.
//!
//! Every test is `#[ignore]`d: it needs a cluster. Run them with
//!
//! ```text
//! SECRETSPEC_TEST_KUBECONFIG=<path> \
//!   cargo test -p secretspec-api --test server_validation -- --ignored --test-threads=1
//! ```
//!
//! See `docs/development.md` for creating a kind cluster. The tests apply the
//! CRD (idempotent, left installed), then use dry-run creates in a throw-away
//! namespace; only `type_change_rejected_on_update` creates a real object.
//! Where the server and the client-side tests disagree, the server wins.
// The suite reports through panics, like any test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fmt::Write as _;
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{DeleteParams, DynamicObject, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::{ApiResource, GroupVersionKind};
use kube::runtime::wait::{await_condition, conditions};
use kube::{Api, Client, Config};
use serde_json::{Value, json};

const FIELD_MANAGER: &str = "secretspec-test";
const CRD_NAME: &str = "secretspecs.secretspec.jabbas.eu";
const ESTABLISHED_TIMEOUT: Duration = Duration::from_secs(60);

// --- helpers ---------------------------------------------------------------

async fn client() -> Client {
    let path = std::env::var("SECRETSPEC_TEST_KUBECONFIG").unwrap_or_else(|_| {
        panic!(
            "SECRETSPEC_TEST_KUBECONFIG is not set: point it at the kubeconfig of a disposable \
             cluster (see docs/development.md), e.g. a kind cluster"
        )
    });
    let kubeconfig = Kubeconfig::read_from(&path)
        .unwrap_or_else(|e| panic!("cannot read kubeconfig {path}: {e}"));
    let config = Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .unwrap_or_else(|e| panic!("invalid kubeconfig {path}: {e}"));
    Client::try_from(config).expect("build kube client")
}

fn crd_manifest() -> CustomResourceDefinition {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../charts/secretspec-operator/files/crd/secretspecs.secretspec.jabbas.eu.yaml");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read generated CRD {}: {e}", path.display()));
    serde_norway::from_str(&text).expect("generated CRD parses")
}

/// Server-side applies the generated CRD and waits for `Established=True`.
async fn ensure_crd(client: &Client) {
    let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
    let crd = crd_manifest();
    let name = crd.metadata.name.clone().expect("CRD name");
    assert_eq!(name, CRD_NAME);
    crds.patch(
        &name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&crd),
    )
    .await
    .unwrap_or_else(|e| panic!("API server rejected the generated CRD: {e}"));
    tokio::time::timeout(
        ESTABLISHED_TIMEOUT,
        await_condition(crds, &name, conditions::is_crd_established()),
    )
    .await
    .unwrap_or_else(|_| panic!("CRD {name} not Established within {ESTABLISHED_TIMEOUT:?}"))
    .expect("watch CRD");
}

fn secretspec_resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("secretspec.jabbas.eu", "v1alpha1", "SecretSpec"),
        "secretspecs",
    )
}

fn unique_namespace() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    format!(
        "secretspec-crd-test-{:x}",
        nanos ^ u128::from(std::process::id())
    )
}

/// Runs `body` in a fresh namespace and deletes the namespace afterwards,
/// whether or not `body` panicked.
async fn with_namespace<F, Fut>(body: F)
where
    F: FnOnce(Client, String) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let client = client().await;
    ensure_crd(&client).await;
    let namespaces: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(client.clone());
    let ns = unique_namespace();
    namespaces
        .create(
            &PostParams::default(),
            &k8s_openapi::api::core::v1::Namespace {
                metadata: ObjectMeta {
                    name: Some(ns.clone()),
                    ..ObjectMeta::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("cannot create namespace {ns}: {e}"));

    let outcome = tokio::spawn(body(client.clone(), ns.clone())).await;
    if let Err(e) = namespaces.delete(&ns, &DeleteParams::default()).await {
        eprintln!("warning: could not delete namespace {ns}: {e}");
    }
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

fn api_for(client: &Client, ns: &str) -> Api<DynamicObject> {
    Api::namespaced_with(client.clone(), ns, &secretspec_resource())
}

/// The fixture manifest retargeted to namespace `ns`.
fn manifest_in(ns: &str, mut value: Value) -> DynamicObject {
    value["metadata"]["namespace"] = json!(ns);
    serde_json::from_value(value).expect("fixture is a valid object")
}

fn dry_run() -> PostParams {
    PostParams {
        dry_run: true,
        field_manager: Some(FIELD_MANAGER.to_owned()),
    }
}

/// `Ok(HTTP code, message)` when the server rejected the request.
fn rejection(err: &kube::Error) -> Option<(u16, &str)> {
    match err {
        kube::Error::Api(status) => Some((status.code, status.message.as_str())),
        _ => None,
    }
}

// --- tests -----------------------------------------------------------------

#[tokio::test]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn crd_applies_and_becomes_established() {
    let client = client().await;
    ensure_crd(&client).await;
}

#[tokio::test]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn valid_fixtures_accepted() {
    with_namespace(|client, ns| async move {
        let api = api_for(&client, &ns);
        let mut failures = String::new();
        for fixture in common::load("valid") {
            let obj = manifest_in(&ns, fixture.value);
            if let Err(e) = api.create(&dry_run(), &obj).await {
                let _ = writeln!(failures, "{}: rejected: {e}", fixture.file);
            }
        }
        assert!(failures.is_empty(), "valid fixtures rejected:\n{failures}");
    })
    .await;
}

#[tokio::test]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn invalid_fixtures_rejected() {
    with_namespace(|client, ns| async move {
        let api = api_for(&client, &ns);
        let mut failures = String::new();
        for fixture in common::load("invalid") {
            let obj = manifest_in(&ns, fixture.value);
            match api.create(&dry_run(), &obj).await {
                Ok(_) => {
                    let _ = writeln!(failures, "{}: accepted by the server", fixture.file);
                }
                Err(e) => match rejection(&e) {
                    Some((422, message)) => {
                        for expect in fixture.expect.iter().chain(&fixture.server_expect) {
                            if message.contains(expect.as_str()) {
                                continue;
                            }
                            let _ = writeln!(
                                failures,
                                "{}: message lacks {expect:?}\n    got: {message}",
                                fixture.file
                            );
                        }
                    }
                    _ => {
                        let _ = writeln!(failures, "{}: expected HTTP 422, got: {e}", fixture.file);
                    }
                },
            }
        }
        assert!(
            failures.is_empty(),
            "invalid fixtures mishandled:\n{failures}"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn dry_run_materialises_expected_defaults() {
    with_namespace(|client, ns| async move {
        let api = api_for(&client, &ns);
        let create = |file: &'static str| {
            let value = common::load("valid")
                .into_iter()
                .find(|f| f.file.starts_with(file))
                .unwrap_or_else(|| panic!("no valid fixture {file}"))
                .value;
            let api = api.clone();
            let ns = ns.clone();
            async move {
                api.create(&dry_run(), &manifest_in(&ns, value))
                    .await
                    .unwrap_or_else(|e| panic!("{file}: {e}"))
                    .data
            }
        };

        let v01 = create("v01-").await;
        assert_eq!(v01["spec"]["deletionPolicy"], "Delete", "{v01}");
        assert_eq!(v01["spec"]["secret"]["type"], "Opaque", "{v01}");
        let field = v01["spec"]["fields"][0].as_object().expect("fields[0]");
        assert!(
            !field.contains_key("length"),
            "length must stay unset: {field:?}"
        );
        assert!(
            !field.contains_key("charset"),
            "charset must stay unset: {field:?}"
        );

        let v03 = create("v03-").await;
        let ssh = &v03["spec"]["sshKeyPair"];
        assert_eq!(ssh["algorithm"], "ed25519", "{v03}");
        assert_eq!(
            ssh["keys"],
            json!({"privateKey": "ssh-privatekey", "publicKey": "ssh-publickey"}),
            "{v03}"
        );
        let ssh = ssh.as_object().expect("sshKeyPair");
        assert!(
            !ssh.contains_key("rsaBits"),
            "rsaBits must stay unset: {ssh:?}"
        );
        assert!(
            !ssh.contains_key("comment"),
            "comment must stay unset: {ssh:?}"
        );

        let v06 = create("v06-").await;
        assert_eq!(
            v06["spec"]["basicAuth"]["keys"]["username"], "username",
            "{v06}"
        );
        assert_eq!(
            v06["spec"]["basicAuth"]["keys"]["password"], "password",
            "{v06}"
        );
    })
    .await;
}

#[tokio::test]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn type_change_rejected_on_update() {
    with_namespace(|client, ns| async move {
        let api = api_for(&client, &ns);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/update/u01-type-immutable");
        let read = |name: &str| -> Value {
            let text = std::fs::read_to_string(dir.join(name)).expect("read update fixture");
            serde_norway::from_str(&text).expect("update fixture parses")
        };
        let expect = std::fs::read_to_string(dir.join("new.yaml"))
            .expect("read new.yaml")
            .lines()
            .find_map(|l| l.strip_prefix("# expect:").map(|v| v.trim().to_owned()))
            .expect("new.yaml has `# expect:`");

        let created = api
            .create(&PostParams::default(), &manifest_in(&ns, read("old.yaml")))
            .await
            .unwrap_or_else(|e| panic!("old.yaml must be accepted: {e}"));

        let mut replacement = manifest_in(&ns, read("new.yaml"));
        replacement.metadata.resource_version = created.metadata.resource_version.clone();
        let name = created.metadata.name.clone().expect("name");
        let err = api
            .replace(&name, &PostParams::default(), &replacement)
            .await
            .expect_err("changing secret.type must be rejected");
        let (code, message) = rejection(&err).unwrap_or_else(|| panic!("not an API error: {err}"));
        assert_eq!(code, 422, "{message}");
        assert!(
            message.contains(&expect),
            "message lacks {expect:?}: {message}"
        );
    })
    .await;
}

fn spec_object(spec: &Value) -> Value {
    json!({
        "apiVersion": "secretspec.jabbas.eu/v1alpha1",
        "kind": "SecretSpec",
        "metadata": {"name": "null-probe"},
        "spec": spec,
    })
}

/// An explicit `null` on an optional property means "unset": enum properties
/// (non-nullable in the CRD) have it pruned, other optional properties keep it,
/// and the CEL rules treat both the same as an absent property.
#[tokio::test]
#[ignore = "needs a cluster: set SECRETSPEC_TEST_KUBECONFIG"]
async fn explicit_null_optionals_behave_as_unset() {
    with_namespace(|client, ns| async move {
        let api = api_for(&client, &ns);
        let create = |spec: Value| {
            let obj = manifest_in(&ns, spec_object(&spec));
            let api = api.clone();
            async move { api.create(&dry_run(), &obj).await }
        };

        // Accepted: every optional (including all enums) set to null.
        let fields = create(json!({"fields": [{
            "name": "a", "length": null, "charset": null, "alphabet": null, "exclude": null,
            "bytes": null, "encoding": null, "require": null, "prefix": null, "suffix": null,
        }]}))
        .await
        .unwrap_or_else(|e| panic!("null optionals on fields[]: {e}"));
        let field = fields.data["spec"]["fields"][0]
            .as_object()
            .expect("fields[0]")
            .clone();
        assert!(
            !field.contains_key("charset"),
            "enum null is pruned: {field:?}"
        );
        assert!(
            !field.contains_key("encoding"),
            "enum null is pruned: {field:?}"
        );

        // `rsaBits: null` / `ecdsaCurve: null` do not trip the "only with" rules.
        let ssh = create(json!({"sshKeyPair": {
            "algorithm": "ed25519", "rsaBits": null, "ecdsaCurve": null, "comment": null,
        }}))
        .await
        .unwrap_or_else(|e| panic!("null optionals on sshKeyPair: {e}"));
        let ssh = ssh.data["spec"]["sshKeyPair"]
            .as_object()
            .expect("sshKeyPair")
            .clone();
        assert!(!ssh.contains_key("rsaBits"), "enum null is pruned: {ssh:?}");
        assert!(
            !ssh.contains_key("ecdsaCurve"),
            "enum null is pruned: {ssh:?}"
        );

        create(json!({"basicAuth": {"username": "u", "password": {"charset": null}}}))
            .await
            .unwrap_or_else(|e| panic!("null charset on basicAuth.password: {e}"));

        // Rejected exactly as if the null property were absent.
        let cases = [
            (
                json!({"fields": [{"name": "a", "charset": null, "alphabet": "abc"}]}),
                secretspec_api::constants::MSG_ALPHABET_WITH_CUSTOM,
            ),
            (
                json!({"fields": [{"name": "a", "bytes": null, "encoding": "hex"}]}),
                secretspec_api::constants::MSG_ENCODING_WITH_BYTES,
            ),
            (
                json!({"fields": [{"name": "a", "bytes": 4, "encoding": null}]}),
                secretspec_api::constants::MSG_ENCODING_WITH_BYTES,
            ),
        ];
        for (spec, expect) in cases {
            let err = create(spec.clone())
                .await
                .expect_err(&format!("{spec} must be rejected"));
            let (code, message) = rejection(&err).unwrap_or_else(|| panic!("{err}"));
            assert_eq!(code, 422, "{message}");
            assert!(message.contains(expect), "{spec}: {message}");
        }
    })
    .await;
}
