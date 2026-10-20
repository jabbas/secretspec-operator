// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests: the Helm chart from the working tree, installed with the
//! operator image into a disposable cluster.
//!
//! Every test is `#[ignore]`d: it needs a cluster with the image already
//! loaded (e.g. `kind load`). Run them with
//!
//! ```text
//! SECRETSPEC_E2E_KUBECONFIG=<path> SECRETSPEC_E2E_IMAGE=localhost/secretspec-operator:e2e \
//!   cargo test -p secretspec-operator --test e2e -- --ignored --test-threads=1
//! ```
//!
//! `SECRETSPEC_E2E_IMAGE` is `<registry>/<repository>:<tag>`; the chart is
//! installed with `image.pullPolicy=Never`. `helm` (or `$HELM`) must be on the
//! PATH. The release `secretspec-operator` lives in namespace
//! `secretspec-system` (PSA `restricted`); each test installs or upgrades it
//! with the values it needs, works in its own app namespaces and deletes them
//! afterwards. The release and the CRD are left installed. See
//! `docs/development.md` for the full local procedure.
// The suite reports through panics, like any test.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
// Kubernetes kinds (SecretSpec, ServiceAccount, …) read as prose here.
#![allow(clippy::doc_markdown)]

// Shared with the in-process integration suite; only part of it is used here.
#[allow(dead_code)]
mod support;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Pod, Secret};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::api::{
    Api, DeleteParams, DynamicObject, ListParams, LogParams, Patch, PatchParams, PostParams,
};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::core::{ApiResource, GroupVersionKind};
use kube::{Client, ResourceExt};
use secretspec_api::constants::{LABEL_MANAGED_BY, MANAGED_BY_VALUE};
use secretspec_api::v1alpha1::SecretSpec;
use serde_json::Value;
use support::{assert_not_logged, data, ready, wait_until_for};

const RELEASE: &str = "secretspec-operator";
const RELEASE_NAMESPACE: &str = "secretspec-system";
const CRD_NAME: &str = "secretspecs.secretspec.jabbas.eu";
const FIELD_MANAGER: &str = "secretspec-e2e";
/// Upper bound of `helm --wait` and of every rollout wait.
const ROLLOUT: Duration = Duration::from_secs(300);
/// Upper bound of waits for the operator's work (Ready, Secrets).
const TIMEOUT: Duration = Duration::from_secs(120);
const PSA_ENFORCE: &str = "pod-security.kubernetes.io/enforce";

// --- environment ---------------------------------------------------------------

fn kubeconfig_path() -> String {
    std::env::var("SECRETSPEC_E2E_KUBECONFIG").unwrap_or_else(|_| {
        panic!(
            "SECRETSPEC_E2E_KUBECONFIG is not set: point it at the kubeconfig of a disposable \
             cluster with the operator image loaded (see docs/development.md)"
        )
    })
}

async fn client() -> Client {
    let path = kubeconfig_path();
    let kubeconfig = Kubeconfig::read_from(&path)
        .unwrap_or_else(|e| panic!("cannot read kubeconfig {path}: {e}"));
    let config = kube::Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .unwrap_or_else(|e| panic!("invalid kubeconfig {path}: {e}"));
    Client::try_from(config).expect("build kube client")
}

/// `SECRETSPEC_E2E_IMAGE` split into the chart's image values.
struct Image {
    registry: String,
    repository: String,
    tag: String,
}

fn image() -> Image {
    let reference = std::env::var("SECRETSPEC_E2E_IMAGE").unwrap_or_else(|_| {
        panic!(
            "SECRETSPEC_E2E_IMAGE is not set: the operator image loaded into the cluster, \
             e.g. localhost/secretspec-operator:e2e"
        )
    });
    let parsed = reference.split_once('/').and_then(|(registry, rest)| {
        let (repository, tag) = rest.rsplit_once(':')?;
        let is_registry =
            registry.contains('.') || registry.contains(':') || registry == "localhost";
        (is_registry && !repository.is_empty() && !tag.is_empty() && !tag.contains('/')).then(
            || Image {
                registry: registry.to_owned(),
                repository: repository.to_owned(),
                tag: tag.to_owned(),
            },
        )
    });
    parsed.unwrap_or_else(|| {
        panic!("SECRETSPEC_E2E_IMAGE={reference:?} is not <registry>/<repository>:<tag>")
    })
}

fn workspace_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

// --- helm ----------------------------------------------------------------------

/// Runs `helm` with `args` against the test cluster; returns stdout, panics
/// with helm's output on failure.
async fn helm(args: Vec<String>) -> String {
    let program = std::env::var_os("HELM").unwrap_or_else(|| OsString::from("helm"));
    let kubeconfig = kubeconfig_path();
    tokio::task::spawn_blocking(move || {
        let output = Command::new(&program)
            .arg("--kubeconfig")
            .arg(&kubeconfig)
            .args(&args)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}", program.to_string_lossy()));
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        assert!(
            output.status.success(),
            "helm {args:?} failed:\n{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        stdout
    })
    .await
    .expect("helm task")
}

/// `helm upgrade --install` of the chart in the working tree with the image
/// under test and `sets`; waits for the rollout and the operator to be Ready.
async fn install(client: &Client, sets: &[&str]) -> Pod {
    ensure_release_namespace(client).await;
    let image = image();
    let mut args: Vec<String> = [
        "upgrade",
        "--install",
        RELEASE,
        &workspace_path("charts/secretspec-operator").to_string_lossy(),
        "--namespace",
        RELEASE_NAMESPACE,
        "--wait",
        "--timeout",
        &format!("{}s", ROLLOUT.as_secs()),
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let mut all_sets = vec![
        format!("image.registry={}", image.registry),
        format!("image.repository={}", image.repository),
        format!("image.tag={}", image.tag),
        "image.pullPolicy=Never".to_owned(),
    ];
    all_sets.extend(sets.iter().map(|s| (*s).to_owned()));
    for set in all_sets {
        args.push("--set".to_owned());
        args.push(set);
    }
    helm(args).await;
    wait_operator_ready(client).await
}

async fn uninstall() {
    helm(vec![
        "uninstall".to_owned(),
        RELEASE.to_owned(),
        "--namespace".to_owned(),
        RELEASE_NAMESPACE.to_owned(),
        "--wait".to_owned(),
        "--timeout".to_owned(),
        format!("{}s", ROLLOUT.as_secs()),
    ])
    .await;
}

// --- namespaces ----------------------------------------------------------------

fn restricted_namespace(name: &str) -> Namespace {
    Namespace {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            labels: Some(BTreeMap::from([
                (PSA_ENFORCE.to_owned(), "restricted".to_owned()),
                (
                    "pod-security.kubernetes.io/enforce-version".to_owned(),
                    "latest".to_owned(),
                ),
            ])),
            ..ObjectMeta::default()
        },
        ..Namespace::default()
    }
}

/// The release namespace, PSA `restricted` (enforced).
async fn ensure_release_namespace(client: &Client) {
    let namespaces: Api<Namespace> = Api::all(client.clone());
    namespaces
        .patch(
            RELEASE_NAMESPACE,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&restricted_namespace(RELEASE_NAMESPACE)),
        )
        .await
        .unwrap_or_else(|e| panic!("cannot apply namespace {RELEASE_NAMESPACE}: {e}"));
}

fn unique_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    format!(
        "{:08x}",
        (nanos ^ u128::from(std::process::id())) & 0xffff_ffff
    )
}

/// Creates `count` PSA-`restricted` app namespaces, runs `body` with their
/// names, then restores the default install (so finalizers can be processed)
/// and deletes the namespaces, whether or not `body` panicked.
async fn with_app_namespaces<F, Fut>(count: usize, body: F)
where
    F: FnOnce(Client, Vec<String>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let client = client().await;
    let id = unique_id();
    let names: Vec<String> = (0..count)
        .map(|i| format!("secretspec-e2e-{id}-{i}"))
        .collect();
    let namespaces: Api<Namespace> = Api::all(client.clone());
    for ns in &names {
        namespaces
            .create(&PostParams::default(), &restricted_namespace(ns))
            .await
            .unwrap_or_else(|e| panic!("cannot create namespace {ns}: {e}"));
    }
    let outcome = tokio::spawn(body(client.clone(), names.clone())).await;
    // SecretSpecs with deletionPolicy Delete carry a finalizer: the operator
    // (watching all namespaces) must run while their namespace is deleted.
    let restored = tokio::spawn({
        let client = client.clone();
        async move {
            install(&client, &[]).await;
        }
    })
    .await;
    for ns in &names {
        if let Err(e) = namespaces.delete(ns, &DeleteParams::default()).await {
            eprintln!("warning: could not delete namespace {ns}: {e}");
        }
    }
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
    if let Err(e) = restored {
        std::panic::resume_unwind(e.into_panic());
    }
}

// --- operator pod ----------------------------------------------------------------

fn pod_ready(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod
            .status
            .as_ref()
            .and_then(|s| s.conditions.as_ref())
            .is_some_and(|conditions| {
                conditions
                    .iter()
                    .any(|c| c.type_ == "Ready" && c.status == "True")
            })
}

/// The release's pods (including terminating ones).
async fn operator_pods(client: &Client) -> Vec<Pod> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), RELEASE_NAMESPACE);
    pods.list(&ListParams::default().labels(&format!(
        "app.kubernetes.io/instance={RELEASE},app.kubernetes.io/name=secretspec-operator"
    )))
    .await
    .expect("list operator pods")
    .items
}

/// Waits until the release runs exactly one pod, Ready, and no other
/// (terminating) pod is left; returns it.
async fn wait_operator_ready(client: &Client) -> Pod {
    wait_until_for("the operator pod to be Ready", ROLLOUT, || async {
        let pods = operator_pods(client).await;
        (pods.len() == 1 && pod_ready(&pods[0])).then(|| pods[0].clone())
    })
    .await
}

async fn pod_logs(client: &Client, pod: &Pod) -> String {
    let pods: Api<Pod> = Api::namespaced(client.clone(), RELEASE_NAMESPACE);
    pods.logs(&pod.name_any(), &LogParams::default())
        .await
        .unwrap_or_else(|e| panic!("cannot read logs of {}: {e}", pod.name_any()))
}

/// Waits until `pod` logged a reconcile of every `ns/name`.
async fn wait_reconciled_by(client: &Client, pod: &Pod, ns: &str, names: &[String]) {
    wait_until_for(
        &format!("{} to reconcile every SecretSpec in {ns}", pod.name_any()),
        TIMEOUT,
        || async {
            let logs = pod_logs(client, pod).await;
            names
                .iter()
                .all(|name| {
                    let quoted = format!("\"{name}\"");
                    logs.lines().any(|line| {
                        line.contains("\"reconciled\"")
                            && line.contains(ns)
                            && line.contains(&quoted)
                    })
                })
                .then_some(())
        },
    )
    .await;
}

// --- SecretSpecs -------------------------------------------------------------------

fn secretspec_resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("secretspec.jabbas.eu", "v1alpha1", "SecretSpec"),
        "secretspecs",
    )
}

/// Every `examples/*.yaml` as `(file name, manifest)`, sorted by file name.
fn examples() -> Vec<(String, Value)> {
    let dir = workspace_path("examples");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no examples in {}", dir.display());
    files
        .into_iter()
        .map(|file| {
            let text = std::fs::read_to_string(&file).expect("read example");
            let value: Value = serde_norway::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: invalid YAML: {e}", file.display()));
            let name = file
                .file_name()
                .expect("file name")
                .to_string_lossy()
                .into_owned();
            (name, value)
        })
        .collect()
}

/// Creates `manifest` in `ns` exactly as written (plus the namespace), like
/// `kubectl apply -n <ns> -f`; returns its name.
async fn create_manifest(client: &Client, ns: &str, mut manifest: Value) -> String {
    manifest["metadata"]["namespace"] = Value::String(ns.to_owned());
    let object: DynamicObject = serde_json::from_value(manifest).expect("manifest is an object");
    let api: Api<DynamicObject> = Api::namespaced_with(client.clone(), ns, &secretspec_resource());
    api.create(&PostParams::default(), &object)
        .await
        .unwrap_or_else(|e| panic!("cannot create SecretSpec {}: {e}", object.name_any()))
        .name_any()
}

/// Creates every example in `ns`; returns their names.
async fn apply_examples(client: &Client, ns: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (_, manifest) in examples() {
        names.push(create_manifest(client, ns, manifest).await);
    }
    names
}

/// Waits until `ns/name` is Ready True for its current generation.
async fn wait_ready(client: &Client, ns: &str, name: &str) -> SecretSpec {
    let api: Api<SecretSpec> = Api::namespaced(client.clone(), ns);
    wait_until_for(
        &format!("SecretSpec {ns}/{name} to be Ready"),
        TIMEOUT,
        || {
            let api = api.clone();
            async move {
                api.get_opt(name)
                    .await
                    .expect("get SecretSpec")
                    .filter(|spec| {
                        ready(spec).is_some_and(|c| c.status == "True")
                            && spec.status.as_ref().and_then(|s| s.observed_generation)
                                == spec.metadata.generation
                    })
            }
        },
    )
    .await
}

async fn wait_all_ready(client: &Client, ns: &str, names: &[String]) {
    for name in names {
        wait_ready(client, ns, name).await;
    }
}

/// Every Secret named in `names`, keyed by name.
async fn secrets(client: &Client, ns: &str, names: &[String]) -> BTreeMap<String, Secret> {
    let api: Api<Secret> = Api::namespaced(client.clone(), ns);
    let mut out = BTreeMap::new();
    for name in names {
        let secret = api
            .get_opt(name)
            .await
            .expect("get Secret")
            .unwrap_or_else(|| panic!("Secret {ns}/{name} does not exist"));
        out.insert(name.clone(), secret);
    }
    out
}

/// Data and resourceVersion of every Secret, for before/after comparisons.
type Snapshot = BTreeMap<String, (Option<String>, BTreeMap<String, String>)>;

async fn snapshot(client: &Client, ns: &str, names: &[String]) -> Snapshot {
    secrets(client, ns, names)
        .await
        .into_iter()
        .map(|(name, secret)| (name, (secret.resource_version(), data(&secret))))
        .collect()
}

/// Asserts `after == before` without printing the values.
fn assert_unchanged(before: &Snapshot, after: &Snapshot, what: &str) {
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "{what}: set of Secrets changed"
    );
    for (name, (rv_before, data_before)) in before {
        let (rv_after, data_after) = &after[name];
        assert!(
            data_before == data_after,
            "{what}: data of Secret {name} changed"
        );
        assert_eq!(
            rv_before, rv_after,
            "{what}: resourceVersion of Secret {name} changed"
        );
    }
}

fn keys(secret: &Secret) -> Vec<&str> {
    secret
        .data
        .iter()
        .flatten()
        .map(|(k, _)| k.as_str())
        .collect()
}

// --- tests ---------------------------------------------------------------------------

/// The operator runs in a release namespace that enforces PSA `restricted`,
/// non-root, with a read-only root filesystem.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster with the image: set SECRETSPEC_E2E_KUBECONFIG and SECRETSPEC_E2E_IMAGE"]
async fn e2e_install_restricted_namespace() {
    let client = client().await;
    let pod = install(&client, &[]).await;

    let namespaces: Api<Namespace> = Api::all(client.clone());
    let ns = namespaces
        .get(RELEASE_NAMESPACE)
        .await
        .expect("get namespace");
    assert_eq!(
        ns.labels().get(PSA_ENFORCE).map(String::as_str),
        Some("restricted"),
        "release namespace must enforce PSA restricted"
    );

    let spec = pod.spec.as_ref().expect("pod spec");
    let container = &spec.containers[0];
    let image = image();
    assert_eq!(
        container.image.as_deref(),
        Some(format!("{}/{}:{}", image.registry, image.repository, image.tag).as_str())
    );
    let security = container
        .security_context
        .as_ref()
        .expect("container securityContext");
    assert_eq!(security.read_only_root_filesystem, Some(true));
    assert_eq!(security.allow_privilege_escalation, Some(false));
    let pod_security = spec.security_context.as_ref().expect("pod securityContext");
    assert_eq!(pod_security.run_as_non_root, Some(true));
    assert_eq!(pod_security.run_as_user, Some(65532));

    let restarts: i32 = pod
        .status
        .as_ref()
        .and_then(|s| s.container_statuses.as_ref())
        .map(|statuses| statuses.iter().map(|s| s.restart_count).sum())
        .unwrap_or_default();
    assert_eq!(restarts, 0, "operator container restarted");
}

/// Every example generates its Secret in a PSA-`restricted` app namespace.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster with the image: set SECRETSPEC_E2E_KUBECONFIG and SECRETSPEC_E2E_IMAGE"]
async fn e2e_examples_generate() {
    with_app_namespaces(1, |client, namespaces| async move {
        let ns = &namespaces[0];
        install(&client, &[]).await;
        let names = apply_examples(&client, ns).await;
        assert_eq!(names.len(), 7, "the 7 documented examples");
        wait_all_ready(&client, ns, &names).await;
        let secrets = secrets(&client, ns, &names).await;

        let expected: &[(&str, &[&str])] = &[
            ("app-admin", &["password", "username"]),
            ("dashboard-auth", &["password", "username", "users"]),
            ("deploy-key", &["ssh-privatekey", "ssh-publickey"]),
            ("fernet-key", &["fernet-key"]),
            (
                "garage",
                &["access-key-id", "rpc-secret", "secret-access-key"],
            ),
            ("rds-master", &["password", "username"]),
            ("shared-token", &["token"]),
        ];
        assert_eq!(
            secrets.keys().map(String::as_str).collect::<Vec<_>>(),
            expected.iter().map(|(n, _)| *n).collect::<Vec<_>>()
        );
        for (name, expected_keys) in expected {
            let secret = &secrets[*name];
            assert_eq!(keys(secret), *expected_keys, "keys of Secret {name}");
            assert_eq!(
                secret.labels().get(LABEL_MANAGED_BY).map(String::as_str),
                Some(MANAGED_BY_VALUE),
                "{name} is labelled as managed"
            );
        }

        let value = |name: &str, key: &str| data(&secrets[name])[key].clone();
        let is_hex = |s: &str| {
            s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        };
        assert_eq!(value("app-admin", "username"), "admin");
        assert_eq!(value("app-admin", "password").len(), 32);
        assert!(
            value("dashboard-auth", "users").starts_with("admin:$2y$12$"),
            "Traefik users line is a bcrypt htpasswd entry"
        );
        assert_eq!(
            secrets["deploy-key"].type_.as_deref(),
            Some("kubernetes.io/ssh-auth")
        );
        assert!(value("deploy-key", "ssh-publickey").starts_with("ssh-ed25519 "));
        assert!(
            value("deploy-key", "ssh-publickey")
                .trim_end()
                .ends_with(" deploy-key")
        );
        let fernet = value("fernet-key", "fernet-key");
        assert_eq!(fernet.len(), 44, "32 bytes, base64url padded");
        assert!(fernet.ends_with('='));
        let rpc = value("garage", "rpc-secret");
        assert!(
            rpc.len() == 64 && is_hex(&rpc),
            "rpc-secret is 32 bytes hex"
        );
        let key_id = value("garage", "access-key-id");
        assert!(
            key_id.len() == 26 && key_id.starts_with("GK") && is_hex(&key_id[2..]),
            "access-key-id is GK + 24 hex"
        );
        assert_eq!(value("garage", "secret-access-key").len(), 64);
        let rds = value("rds-master", "password");
        assert_eq!(rds.len(), 32);
        assert!(
            !rds.contains(['/', '@', '"', ' ']),
            "RDS-forbidden character"
        );
        assert_eq!(value("shared-token", "token").len(), 48);
        let shared = wait_ready(&client, ns, "shared-token").await;
        assert!(
            shared.status.and_then(|s| s.next_rotation_time).is_some(),
            "rotated token has a next rotation time"
        );

        // The operator stays Ready under PSA restricted after the work.
        let pod = wait_operator_ready(&client).await;
        assert!(pod_ready(&pod));
    })
    .await;
}

/// A `helm upgrade` that rolls the pod and a pod deletion change no value
/// and do not even rewrite a Secret.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster with the image: set SECRETSPEC_E2E_KUBECONFIG and SECRETSPEC_E2E_IMAGE"]
async fn e2e_upgrade_and_restart_keep_values() {
    with_app_namespaces(1, |client, namespaces| async move {
        let ns = &namespaces[0];
        let before_pod = install(&client, &[]).await;
        let names = apply_examples(&client, ns).await;
        wait_all_ready(&client, ns, &names).await;
        let before = snapshot(&client, ns, &names).await;

        let upgraded = install(&client, &["logLevel=debug", "resyncInterval=30m"]).await;
        assert_ne!(
            upgraded.uid(),
            before_pod.uid(),
            "changing logLevel and resyncInterval must roll the pod"
        );
        let env: BTreeMap<String, String> = upgraded.spec.as_ref().expect("pod spec").containers[0]
            .env
            .iter()
            .flatten()
            .filter_map(|e| Some((e.name.clone(), e.value.clone()?)))
            .collect();
        assert_eq!(env.get("LOG_LEVEL").map(String::as_str), Some("debug"));
        assert_eq!(env.get("RESYNC_INTERVAL").map(String::as_str), Some("30m"));
        wait_reconciled_by(&client, &upgraded, ns, &names).await;
        assert_unchanged(
            &before,
            &snapshot(&client, ns, &names).await,
            "helm upgrade",
        );

        let pods: Api<Pod> = Api::namespaced(client.clone(), RELEASE_NAMESPACE);
        pods.delete(&upgraded.name_any(), &DeleteParams::default())
            .await
            .expect("delete operator pod");
        let restarted = wait_operator_ready(&client).await;
        assert_ne!(
            restarted.uid(),
            upgraded.uid(),
            "a new pod replaced the deleted one"
        );
        wait_reconciled_by(&client, &restarted, ns, &names).await;
        wait_all_ready(&client, ns, &names).await;
        assert_unchanged(&before, &snapshot(&client, ns, &names).await, "pod restart");
    })
    .await;
}

/// `helm uninstall` keeps the CRD, every SecretSpec and every Secret; a
/// reinstall changes no value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster with the image: set SECRETSPEC_E2E_KUBECONFIG and SECRETSPEC_E2E_IMAGE"]
async fn e2e_uninstall_keeps_crd() {
    with_app_namespaces(1, |client, namespaces| async move {
        let ns = &namespaces[0];
        install(&client, &[]).await;
        let names = apply_examples(&client, ns).await;
        wait_all_ready(&client, ns, &names).await;
        let before = snapshot(&client, ns, &names).await;

        uninstall().await;
        wait_until_for("the operator pods to be gone", ROLLOUT, || async {
            operator_pods(&client).await.is_empty().then_some(())
        })
        .await;
        let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
        let crd = crds
            .get_opt(CRD_NAME)
            .await
            .expect("get CRD")
            .expect("CRD survives helm uninstall");
        assert!(
            crd.metadata.deletion_timestamp.is_none(),
            "CRD is not being deleted"
        );
        let specs: Api<SecretSpec> = Api::namespaced(client.clone(), ns);
        let remaining: Vec<String> = specs
            .list(&ListParams::default())
            .await
            .expect("list SecretSpecs")
            .items
            .iter()
            .map(ResourceExt::name_any)
            .collect();
        let mut expected = names.clone();
        expected.sort();
        assert_eq!(remaining, expected, "SecretSpecs survive helm uninstall");
        assert_unchanged(
            &before,
            &snapshot(&client, ns, &names).await,
            "helm uninstall",
        );

        let pod = install(&client, &[]).await;
        wait_reconciled_by(&client, &pod, ns, &names).await;
        wait_all_ready(&client, ns, &names).await;
        assert_unchanged(&before, &snapshot(&client, ns, &names).await, "reinstall");
    })
    .await;
}

/// With `watchNamespaces=[app]` the operator serves only that namespace, has
/// RBAC only there, and never hits a 403.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster with the image: set SECRETSPEC_E2E_KUBECONFIG and SECRETSPEC_E2E_IMAGE"]
async fn e2e_namespace_mode() {
    with_app_namespaces(2, |client, namespaces| async move {
        let (watched, other) = (&namespaces[0], &namespaces[1]);
        let pod = install(&client, &[&format!("watchNamespaces={{{watched}}}")]).await;

        let (_, manifest) = examples()
            .into_iter()
            .find(|(file, _)| file == "rotated-shared-token.yaml")
            .expect("rotated-shared-token example");
        let ignored = create_manifest(&client, other, manifest.clone()).await;
        let served = create_manifest(&client, watched, manifest).await;
        wait_ready(&client, watched, &served).await;

        // Give the operator ample time to (wrongly) touch the other namespace.
        tokio::time::sleep(Duration::from_secs(15)).await;
        let specs: Api<SecretSpec> = Api::namespaced(client.clone(), other);
        let spec = specs.get(&ignored).await.expect("get ignored SecretSpec");
        assert!(
            spec.status.is_none(),
            "SecretSpec outside watchNamespaces got a status"
        );
        assert!(
            spec.finalizers().is_empty(),
            "SecretSpec outside watchNamespaces got a finalizer"
        );
        let other_secrets: Api<Secret> = Api::namespaced(client.clone(), other);
        assert!(
            other_secrets
                .get_opt(&ignored)
                .await
                .expect("get Secret")
                .is_none(),
            "Secret generated outside watchNamespaces"
        );

        let logs = pod_logs(&client, &pod).await;
        for needle in ["403", "Forbidden"] {
            assert!(!logs.contains(needle), "operator logged {needle}:\n{logs}");
        }
    })
    .await;
}

/// The operator's own logs (at debug) never contain a generated value, raw
/// or base64.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a cluster with the image: set SECRETSPEC_E2E_KUBECONFIG and SECRETSPEC_E2E_IMAGE"]
async fn e2e_logs_contain_no_values() {
    with_app_namespaces(1, |client, namespaces| async move {
        let ns = &namespaces[0];
        let pod = install(&client, &["logLevel=debug"]).await;
        let names = apply_examples(&client, ns).await;
        wait_all_ready(&client, ns, &names).await;
        wait_reconciled_by(&client, &pod, ns, &names).await;

        let logs = pod_logs(&client, &pod).await;
        assert!(!logs.is_empty(), "the operator logged nothing");
        for secret in secrets(&client, ns, &names).await.values() {
            assert_not_logged(secret, &logs);
        }
    })
    .await;
}
