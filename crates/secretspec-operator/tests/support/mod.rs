// SPDX-License-Identifier: Apache-2.0

//! Harness for the in-process operator integration tests.
//!
//! Every test runs in its own pair of namespaces (`secretspec-it-<id>-a` and
//! `-b`) with its own ServiceAccount, RBAC and Lease name. The operator under
//! test runs on a tokio task with a client that impersonates that
//! ServiceAccount, so it has exactly the RBAC of the design (§9) and nothing
//! more. The admin client (from `SECRETSPEC_TEST_KUBECONFIG`) sets up and
//! inspects the cluster.

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use k8s_openapi::api::coordination::v1::Lease;
use k8s_openapi::api::core::v1::{Namespace, Secret, ServiceAccount};
use k8s_openapi::api::events::v1::Event;
use k8s_openapi::api::rbac::v1::{
    ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding, RoleRef, Subject,
};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, ObjectMeta};
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams, PostParams};
use kube::config::{KubeConfigOptions, Kubeconfig};
use kube::runtime::wait::{await_condition, conditions};
use kube::{Client, Resource, ResourceExt};
use secretspec_api::constants::CONDITION_READY;
use secretspec_api::v1alpha1::SecretSpec;
use secretspec_operator::config::{Config, LogFormat};
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinHandle};

/// Field manager of every write the harness makes.
pub const TEST_MANAGER: &str = "secretspec-it";
/// Name of the operator's ServiceAccount in namespace A.
pub const SERVICE_ACCOUNT: &str = "secretspec-operator";
/// Upper bound of every wait.
pub const TIMEOUT: Duration = Duration::from_secs(60);
/// Polling period of every wait.
pub const POLL: Duration = Duration::from_millis(250);
/// The verbs the operator has on Secrets (design §9).
pub const SECRET_VERBS: &[&str] = &["get", "list", "watch", "create", "patch", "update"];

const CRD_NAME: &str = "secretspecs.secretspec.jabbas.eu";
const ROLE_OPERATOR: &str = "secretspec-operator";
const ROLE_LEASE: &str = "secretspec-operator-lease";

// --- log capture -------------------------------------------------------------

/// Everything the operator logged in this process. Set
/// `SECRETSPEC_TEST_LOG=1` to also echo it to stderr.
fn log_buffer() -> &'static Mutex<Vec<u8>> {
    static LOGS: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
    LOGS.get_or_init(|| Mutex::new(Vec::new()))
}

struct CaptureWriter;

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        log_buffer()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        if std::env::var_os("SECRETSPEC_TEST_LOG").is_some() {
            let _ = std::io::Write::write_all(&mut std::io::stderr(), buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The capture's log level: the operator's own logs at debug (a user may set
/// that in production) and quieter transport crates; built by the
/// production filter builder, so `kube_client` is silenced exactly as in
/// production.
const LOG_LEVEL: &str =
    "secretspec_operator=debug,info,kube=warn,hyper=warn,tower=warn,rustls=warn";

fn init_logging() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let (filter, ignored) =
            secretspec_operator::logging::filter(LOG_LEVEL).expect("valid test log level");
        assert!(ignored.is_empty(), "test log level dropped {ignored:?}");
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(|| CaptureWriter)
            .try_init();
    });
}

/// A position in the captured log; see [`logs_since`].
pub fn log_mark() -> usize {
    log_buffer()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len()
}

/// The log captured after `mark`.
pub fn logs_since(mark: usize) -> String {
    let logs = log_buffer()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    String::from_utf8_lossy(&logs[mark.min(logs.len())..]).into_owned()
}

/// The `reconciled` log lines for `namespace/name` after `mark`.
pub fn reconciled_lines(mark: usize, namespace: &str, name: &str) -> Vec<String> {
    let quoted = format!("name=\"{name}\"");
    logs_since(mark)
        .lines()
        .filter(|line| {
            line.contains(" reconciled") && line.contains(namespace) && line.contains(&quoted)
        })
        .map(str::to_owned)
        .collect()
}

// --- cluster access ----------------------------------------------------------

fn kubeconfig_path() -> String {
    std::env::var("SECRETSPEC_TEST_KUBECONFIG").unwrap_or_else(|_| {
        panic!(
            "SECRETSPEC_TEST_KUBECONFIG is not set: point it at the kubeconfig of a disposable \
             cluster (see docs/development.md), e.g. a kind cluster"
        )
    })
}

async fn kube_config() -> kube::Config {
    let path = kubeconfig_path();
    let kubeconfig = Kubeconfig::read_from(&path)
        .unwrap_or_else(|e| panic!("cannot read kubeconfig {path}: {e}"));
    kube::Config::from_custom_kubeconfig(kubeconfig, &KubeConfigOptions::default())
        .await
        .unwrap_or_else(|e| panic!("invalid kubeconfig {path}: {e}"))
}

/// A client acting as `system:serviceaccount:<namespace>:<SERVICE_ACCOUNT>`.
async fn impersonating_client(namespace: &str) -> Client {
    let mut config = kube_config().await;
    config.auth_info.impersonate = Some(format!(
        "system:serviceaccount:{namespace}:{SERVICE_ACCOUNT}"
    ));
    config.auth_info.impersonate_groups = Some(vec![
        "system:serviceaccounts".to_owned(),
        format!("system:serviceaccounts:{namespace}"),
        "system:authenticated".to_owned(),
    ]);
    Client::try_from(config).expect("build impersonating client")
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
    crds.patch(
        CRD_NAME,
        &PatchParams::apply(TEST_MANAGER).force(),
        &Patch::Apply(&crd),
    )
    .await
    .unwrap_or_else(|e| panic!("API server rejected the generated CRD: {e}"));
    tokio::time::timeout(
        TIMEOUT,
        await_condition(crds, CRD_NAME, conditions::is_crd_established()),
    )
    .await
    .unwrap_or_else(|_| panic!("CRD {CRD_NAME} not Established within {TIMEOUT:?}"))
    .expect("watch CRD");
}

fn unique_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    // 10 hex digits keep every derived name well below 63 characters.
    format!(
        "{:010x}",
        (nanos ^ u128::from(std::process::id())) & 0xff_ffff_ffff
    )
}

fn meta(name: &str, namespace: Option<&str>) -> ObjectMeta {
    ObjectMeta {
        name: Some(name.to_owned()),
        namespace: namespace.map(str::to_owned),
        ..ObjectMeta::default()
    }
}

fn rule(group: &str, resources: &[&str], verbs: &[&str]) -> PolicyRule {
    PolicyRule {
        api_groups: Some(vec![group.to_owned()]),
        resources: Some(resources.iter().map(|r| (*r).to_owned()).collect()),
        verbs: verbs.iter().map(|v| (*v).to_owned()).collect(),
        ..PolicyRule::default()
    }
}

/// The operator's rules of design §9 except leases, with `secret_verbs` on
/// Secrets.
fn operator_rules(secret_verbs: &[&str]) -> Vec<PolicyRule> {
    vec![
        rule(
            "secretspec.jabbas.eu",
            &["secretspecs"],
            &["get", "list", "watch"],
        ),
        rule(
            "secretspec.jabbas.eu",
            &["secretspecs/status"],
            &["get", "patch", "update"],
        ),
        rule(
            "secretspec.jabbas.eu",
            &["secretspecs/finalizers"],
            &["update"],
        ),
        rule("", &["secrets"], secret_verbs),
        rule("events.k8s.io", &["events"], &["create", "patch"]),
    ]
}

fn lease_rules() -> Vec<PolicyRule> {
    vec![rule(
        "coordination.k8s.io",
        &["leases"],
        &["get", "create", "update"],
    )]
}

async fn apply<K>(api: &Api<K>, name: &str, object: &K)
where
    K: Resource
        + Clone
        + k8s_openapi::serde::Serialize
        + k8s_openapi::serde::de::DeserializeOwned
        + std::fmt::Debug,
{
    api.patch(
        name,
        &PatchParams::apply(TEST_MANAGER).force(),
        &Patch::Apply(object),
    )
    .await
    .unwrap_or_else(|e| panic!("cannot apply {name}: {e}"));
}

// --- environment -------------------------------------------------------------

/// How the operator's ServiceAccount is authorised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rbac {
    /// A ClusterRole (all namespaces) plus the lease Role in namespace A.
    Cluster,
    /// Roles in namespace A only (operator rules and leases); nothing in B and
    /// nothing cluster-wide.
    Namespace,
}

/// One test's namespaces, RBAC and clients.
pub struct Env {
    /// Cluster-admin client from the kubeconfig.
    pub admin: Client,
    /// Client impersonating the operator's ServiceAccount.
    pub operator_client: Client,
    /// Random id shared by every name of this test.
    pub id: String,
    /// The operator namespace (ServiceAccount, Lease) and first test namespace.
    pub ns_a: String,
    /// A second namespace, for isolation checks.
    pub ns_b: String,
    /// How the ServiceAccount is authorised.
    pub rbac: Rbac,
    operators: Mutex<Vec<AbortHandle>>,
    started: AtomicUsize,
}

/// Sets up an [`Env`], runs `body`, then stops every operator and deletes the
/// namespaces (and cluster-scoped RBAC), whether or not `body` panicked.
pub async fn with_env<F, Fut>(rbac: Rbac, body: F)
where
    F: FnOnce(Arc<Env>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    init_logging();
    let admin = Client::try_from(kube_config().await).expect("build admin client");
    ensure_crd(&admin).await;

    let id = unique_id();
    let ns_a = format!("secretspec-it-{id}-a");
    let ns_b = format!("secretspec-it-{id}-b");
    let namespaces: Api<Namespace> = Api::all(admin.clone());
    for ns in [&ns_a, &ns_b] {
        namespaces
            .create(
                &PostParams::default(),
                &Namespace {
                    metadata: meta(ns, None),
                    ..Namespace::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("cannot create namespace {ns}: {e}"));
    }

    let env = Arc::new(Env {
        operator_client: impersonating_client(&ns_a).await,
        admin: admin.clone(),
        id,
        ns_a,
        ns_b,
        rbac,
        operators: Mutex::new(Vec::new()),
        started: AtomicUsize::new(0),
    });
    // RBAC setup runs inside the guarded task too, so a failure there still
    // deletes the namespaces.
    let outcome = {
        let env = Arc::clone(&env);
        tokio::spawn(async move {
            env.setup_rbac().await;
            body(env).await;
        })
        .await
    };
    env.teardown().await;
    if let Err(e) = outcome {
        std::panic::resume_unwind(e.into_panic());
    }
}

impl Env {
    async fn setup_rbac(&self) {
        let sas: Api<ServiceAccount> = Api::namespaced(self.admin.clone(), &self.ns_a);
        apply(
            &sas,
            SERVICE_ACCOUNT,
            &ServiceAccount {
                metadata: meta(SERVICE_ACCOUNT, Some(&self.ns_a)),
                ..ServiceAccount::default()
            },
        )
        .await;
        self.apply_role(ROLE_LEASE, lease_rules()).await;
        self.set_secret_verbs(SECRET_VERBS).await;
        match self.rbac {
            Rbac::Cluster => {
                let bindings: Api<ClusterRoleBinding> = Api::all(self.admin.clone());
                let name = self.cluster_role_name();
                apply(
                    &bindings,
                    &name,
                    &ClusterRoleBinding {
                        metadata: meta(&name, None),
                        role_ref: RoleRef {
                            api_group: "rbac.authorization.k8s.io".to_owned(),
                            kind: "ClusterRole".to_owned(),
                            name: name.clone(),
                        },
                        subjects: Some(vec![self.subject()]),
                    },
                )
                .await;
            }
            Rbac::Namespace => self.bind_role(ROLE_OPERATOR).await,
        }
        self.bind_role(ROLE_LEASE).await;

        // RBAC is enforced from an informer cache; wait until it took effect.
        let specs: Api<SecretSpec> = Api::namespaced(self.operator_client.clone(), &self.ns_a);
        let leases: Api<Lease> = Api::namespaced(self.operator_client.clone(), &self.ns_a);
        wait_until("operator RBAC to take effect", || {
            let specs = specs.clone();
            let leases = leases.clone();
            async move {
                let ok = specs.list(&ListParams::default()).await.is_ok()
                    && leases.get_opt("probe").await.is_ok();
                ok.then_some(())
            }
        })
        .await;
    }

    fn cluster_role_name(&self) -> String {
        format!("secretspec-it-{}", self.id)
    }

    fn subject(&self) -> Subject {
        Subject {
            kind: "ServiceAccount".to_owned(),
            name: SERVICE_ACCOUNT.to_owned(),
            namespace: Some(self.ns_a.clone()),
            api_group: None,
        }
    }

    async fn apply_role(&self, name: &str, rules: Vec<PolicyRule>) {
        let api: Api<Role> = Api::namespaced(self.admin.clone(), &self.ns_a);
        apply(
            &api,
            name,
            &Role {
                metadata: meta(name, Some(&self.ns_a)),
                rules: Some(rules),
            },
        )
        .await;
    }

    async fn bind_role(&self, name: &str) {
        let bindings: Api<RoleBinding> = Api::namespaced(self.admin.clone(), &self.ns_a);
        apply(
            &bindings,
            name,
            &RoleBinding {
                metadata: meta(name, Some(&self.ns_a)),
                role_ref: RoleRef {
                    api_group: "rbac.authorization.k8s.io".to_owned(),
                    kind: "Role".to_owned(),
                    name: name.to_owned(),
                },
                subjects: Some(vec![self.subject()]),
            },
        )
        .await;
    }

    /// Replaces the operator's rules, with `verbs` on Secrets: the
    /// ClusterRole in cluster mode, the Role in namespace A otherwise.
    pub async fn set_secret_verbs(&self, verbs: &[&str]) {
        match self.rbac {
            Rbac::Cluster => {
                let roles: Api<ClusterRole> = Api::all(self.admin.clone());
                let name = self.cluster_role_name();
                apply(
                    &roles,
                    &name,
                    &ClusterRole {
                        metadata: meta(&name, None),
                        rules: Some(operator_rules(verbs)),
                        aggregation_rule: None,
                    },
                )
                .await;
            }
            Rbac::Namespace => self.apply_role(ROLE_OPERATOR, operator_rules(verbs)).await,
        }
    }

    async fn teardown(&self) {
        for task in self
            .operators
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
        {
            task.abort();
        }
        let namespaces: Api<Namespace> = Api::all(self.admin.clone());
        for ns in [&self.ns_a, &self.ns_b] {
            if let Err(e) = namespaces.delete(ns, &DeleteParams::default()).await {
                eprintln!("warning: could not delete namespace {ns}: {e}");
            }
        }
        if self.rbac == Rbac::Cluster {
            let name = self.cluster_role_name();
            let bindings: Api<ClusterRoleBinding> = Api::all(self.admin.clone());
            let roles: Api<ClusterRole> = Api::all(self.admin.clone());
            if let Err(e) = bindings.delete(&name, &DeleteParams::default()).await {
                eprintln!("warning: could not delete ClusterRoleBinding {name}: {e}");
            }
            if let Err(e) = roles.delete(&name, &DeleteParams::default()).await {
                eprintln!("warning: could not delete ClusterRole {name}: {e}");
            }
        }
    }

    /// This test's Lease name.
    pub fn lease_name(&self) -> String {
        format!("secretspec-it-{}", self.id)
    }

    /// Operator configuration for this test: leader election off, all
    /// namespaces in cluster mode and namespace A in namespace mode, a unique
    /// identity and Lease, an ephemeral HTTP port.
    pub fn config(&self) -> Config {
        let n = self.started.fetch_add(1, Ordering::SeqCst);
        Config {
            watch_namespaces: match self.rbac {
                Rbac::Cluster => Vec::new(),
                Rbac::Namespace => vec![self.ns_a.clone()],
            },
            leader_election: false,
            lease_namespace: Some(self.ns_a.clone()),
            pod_name: Some(format!("it-{}-{n}", self.id)),
            lease_name: self.lease_name(),
            resync_interval: Duration::from_secs(3600),
            http_bind: "127.0.0.1:0".parse().expect("socket address"),
            log_level: "info".to_owned(),
            log_format: LogFormat::Text,
            concurrency: 4,
        }
    }

    /// Starts the operator in-process with `config` and the impersonating
    /// client.
    pub fn start(&self, config: Config) -> Operator {
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(secretspec_operator::run(
            config,
            self.operator_client.clone(),
            async move {
                let _ = stopped.await;
            },
        ));
        self.operators
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(task.abort_handle());
        Operator {
            stop: Some(stop),
            task,
        }
    }

    /// Starts the operator with [`Env::config`].
    pub fn start_default(&self) -> Operator {
        self.start(self.config())
    }

    /// `SecretSpec`s in `ns`, as admin.
    pub fn specs(&self, ns: &str) -> Api<SecretSpec> {
        Api::namespaced(self.admin.clone(), ns)
    }

    /// Secrets in `ns`, as admin.
    pub fn secrets(&self, ns: &str) -> Api<Secret> {
        Api::namespaced(self.admin.clone(), ns)
    }

    /// Creates `spec` (a manifest, `metadata.namespace` is overwritten).
    pub async fn create_spec(&self, ns: &str, mut manifest: Value) -> SecretSpec {
        manifest["metadata"]["namespace"] = Value::String(ns.to_owned());
        let spec: SecretSpec = serde_json::from_value(manifest).expect("valid SecretSpec");
        self.specs(ns)
            .create(&PostParams::default(), &spec)
            .await
            .unwrap_or_else(|e| panic!("cannot create SecretSpec {}: {e}", spec.name_any()))
    }

    /// Creates the valid fixture `file` in `ns`.
    pub async fn create_fixture(&self, ns: &str, file: &str) -> SecretSpec {
        self.create_spec(ns, fixture(file)).await
    }

    /// Waits until `ns/name` is Ready True for its current generation.
    pub async fn wait_ready(&self, ns: &str, name: &str) -> SecretSpec {
        self.wait_spec(ns, name, "Ready True for the current generation", |spec| {
            ready(spec).is_some_and(|c| c.status == "True")
                && spec.status.as_ref().and_then(|s| s.observed_generation)
                    == spec.metadata.generation
        })
        .await
    }

    /// Waits until `ns/name` has Ready False with `reason`.
    pub async fn wait_not_ready(&self, ns: &str, name: &str, reason: &str) -> SecretSpec {
        self.wait_spec(ns, name, &format!("Ready False {reason}"), |spec| {
            ready(spec).is_some_and(|c| c.status == "False" && c.reason == reason)
        })
        .await
    }

    /// Waits until `ns/name` exists and satisfies `check`.
    pub async fn wait_spec(
        &self,
        ns: &str,
        name: &str,
        what: &str,
        check: impl Fn(&SecretSpec) -> bool,
    ) -> SecretSpec {
        let api = self.specs(ns);
        let check = &check;
        wait_until(&format!("SecretSpec {ns}/{name}: {what}"), || {
            let api = api.clone();
            async move {
                api.get_opt(name)
                    .await
                    .expect("get SecretSpec")
                    .filter(|spec| check(spec))
            }
        })
        .await
    }

    /// Waits until Secret `ns/name` exists and satisfies `check`.
    pub async fn wait_secret(
        &self,
        ns: &str,
        name: &str,
        what: &str,
        check: impl Fn(&Secret) -> bool,
    ) -> Secret {
        let api = self.secrets(ns);
        let check = &check;
        wait_until(&format!("Secret {ns}/{name}: {what}"), || {
            let api = api.clone();
            async move {
                api.get_opt(name)
                    .await
                    .expect("get Secret")
                    .filter(|secret| check(secret))
            }
        })
        .await
    }

    /// Waits until Secret `ns/name` does not exist.
    pub async fn wait_secret_gone(&self, ns: &str, name: &str) {
        let api = self.secrets(ns);
        wait_until(&format!("Secret {ns}/{name} to be deleted"), || {
            let api = api.clone();
            async move {
                api.get_opt(name)
                    .await
                    .expect("get Secret")
                    .is_none()
                    .then_some(())
            }
        })
        .await;
    }

    /// Events regarding the `SecretSpec` `ns/name`, optionally only those of
    /// the object with `uid`.
    pub async fn events(&self, ns: &str, name: &str, uid: Option<&str>) -> Vec<Event> {
        let api: Api<Event> = Api::namespaced(self.admin.clone(), ns);
        let mut events: Vec<Event> = api
            .list(&ListParams::default())
            .await
            .expect("list events")
            .items
            .into_iter()
            .filter(|event| {
                event.regarding.as_ref().is_some_and(|r| {
                    r.kind.as_deref() == Some("SecretSpec")
                        && r.name.as_deref() == Some(name)
                        && uid.is_none_or(|uid| r.uid.as_deref() == Some(uid))
                })
            })
            .collect();
        events.sort_by_key(|event| event.event_time.as_ref().map(|t| t.0));
        events
    }

    /// Reasons of [`Env::events`].
    pub async fn event_reasons(&self, ns: &str, name: &str, uid: Option<&str>) -> Vec<String> {
        self.events(ns, name, uid)
            .await
            .into_iter()
            .filter_map(|event| event.reason)
            .collect()
    }

    /// Waits for an event with `reason` regarding `ns/name` (and `uid`).
    pub async fn wait_event(&self, ns: &str, name: &str, uid: Option<&str>, reason: &str) -> Event {
        wait_until(
            &format!("event {reason} on SecretSpec {ns}/{name}"),
            || async move {
                self.events(ns, name, uid)
                    .await
                    .into_iter()
                    .find(|event| event.reason.as_deref() == Some(reason))
            },
        )
        .await
    }

    /// The holder of this test's Lease, if any.
    pub async fn lease_holder(&self) -> Option<String> {
        let api: Api<Lease> = Api::namespaced(self.admin.clone(), &self.ns_a);
        api.get_opt(&self.lease_name())
            .await
            .expect("get Lease")
            .and_then(|lease| lease.spec)
            .and_then(|spec| spec.holder_identity)
            .filter(|holder| !holder.is_empty())
    }
}

/// The operator running in-process.
pub struct Operator {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<anyhow::Result<()>>,
}

impl Operator {
    /// Requests a graceful shutdown and waits (at most [`TIMEOUT`]) for it.
    pub async fn stop(mut self) -> anyhow::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        tokio::time::timeout(TIMEOUT, &mut self.task)
            .await
            .unwrap_or_else(|_| panic!("operator did not stop within {TIMEOUT:?}"))
            .expect("operator task panicked")
    }

    /// Whether the operator task has ended (normally or with an error).
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

// --- helpers -----------------------------------------------------------------

/// The valid fixture `file` of `secretspec-api` as JSON.
pub fn fixture(file: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../secretspec-api/tests/fixtures/valid")
        .join(file);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
    serde_norway::from_str(&text).unwrap_or_else(|e| panic!("fixture {file} parses: {e}"))
}

/// The `Ready` condition.
pub fn ready(spec: &SecretSpec) -> Option<&Condition> {
    spec.status
        .as_ref()?
        .conditions
        .iter()
        .find(|c| c.type_ == CONDITION_READY)
}

/// Polls `probe` every [`POLL`] until it yields `Some`, at most [`TIMEOUT`].
pub async fn wait_until<T, F, Fut>(what: &str, probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    wait_until_for(what, TIMEOUT, probe).await
}

/// [`wait_until`] with an explicit timeout.
pub async fn wait_until_for<T, F, Fut>(what: &str, timeout: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out after {timeout:?} waiting for {what}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The Secret's data as UTF-8 strings (lossy), for comparisons.
pub fn data(secret: &Secret) -> std::collections::BTreeMap<String, String> {
    secret
        .data
        .iter()
        .flatten()
        .map(|(k, v)| (k.clone(), String::from_utf8_lossy(&v.0).into_owned()))
        .collect()
}
