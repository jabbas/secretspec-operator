// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask chart-test`: lints and renders the Helm chart for several
//! value sets and checks the rendered objects.
//!
//! Needs `helm` (3.x or 4.x) on `PATH`, or its path in `HELM`. A missing
//! `helm` is a failure, not a skip.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{Container, EnvVar, PodSpec, Probe};
use k8s_openapi::api::rbac::v1::{PolicyRule, RoleRef, Subject};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_norway::Value;

use crate::{BoxError, CRD_PATH, workspace_root};

/// Chart directory, relative to the workspace root.
pub const CHART_DIR: &str = "charts/secretspec-operator";
/// Release name used for every render.
const RELEASE: &str = "secretspec-operator";
/// Release namespace used for every render.
const NAMESPACE: &str = "secretspec-system";
/// A syntactically valid image digest for the digest renders.
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// Registry override for the mirror renders.
const MIRROR: &str = "mirror.example.com";

/// Value set: namespace mode watching `a` and `b`.
const NAMESPACE_MODE: &[&str] = &["watchNamespaces={a,b}"];

/// Kinds that are cluster-scoped among those the chart may render.
const CLUSTER_SCOPED: &[&str] = &[
    "CustomResourceDefinition",
    "ClusterRole",
    "ClusterRoleBinding",
];

/// Parses a multi-document YAML stream, dropping empty documents.
///
/// # Errors
///
/// Returns the YAML error of the first malformed document.
pub fn parse_documents(yaml: &str) -> Result<Vec<Value>, serde_norway::Error> {
    let mut docs = Vec::new();
    for doc in serde_norway::Deserializer::from_str(yaml) {
        let value = Value::deserialize(doc)?;
        if !value.is_null() {
            docs.push(value);
        }
    }
    Ok(docs)
}

/// Fails with `message` unless `condition` holds.
fn ensure(condition: bool, message: impl FnOnce() -> String) -> Result<(), String> {
    if condition { Ok(()) } else { Err(message()) }
}

/// Fails with an expected/actual message unless `actual == expected`.
fn ensure_eq<T: PartialEq + Debug>(what: &str, actual: &T, expected: &T) -> Result<(), String> {
    ensure(actual == expected, || {
        format!("{what}:\n  expected: {expected:?}\n  actual:   {actual:?}")
    })
}

/// A string field at `path` (mapping keys) inside `value`.
fn str_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(value, |v, key| v.get(*key))
        .and_then(Value::as_str)
}

fn kind_of(value: &Value) -> &str {
    str_at(value, &["kind"]).unwrap_or("")
}

fn name_of(value: &Value) -> &str {
    str_at(value, &["metadata", "name"]).unwrap_or("")
}

fn namespace_of(value: &Value) -> Option<&str> {
    str_at(value, &["metadata", "namespace"])
}

/// The objects of one `helm template` run.
#[derive(Debug)]
pub struct Rendered {
    objects: Vec<Value>,
}

impl Rendered {
    fn of_kind<'a, 'k>(&'a self, kind: &'k str) -> impl Iterator<Item = &'a Value> + use<'a, 'k> {
        self.objects.iter().filter(move |o| kind_of(o) == kind)
    }

    fn one(&self, kind: &str) -> Result<&Value, String> {
        let found: Vec<&Value> = self.of_kind(kind).collect();
        match found.as_slice() {
            [only] => Ok(only),
            _ => Err(format!(
                "expected exactly one {kind}, found {}",
                found.len()
            )),
        }
    }

    fn typed<T: DeserializeOwned>(value: &Value) -> Result<T, String> {
        serde_norway::from_value(value.clone())
            .map_err(|e| format!("cannot decode {} {}: {e}", kind_of(value), name_of(value)))
    }

    fn deployment(&self) -> Result<Deployment, String> {
        Self::typed(self.one("Deployment")?)
    }

    fn pod_spec(&self) -> Result<PodSpec, String> {
        self.deployment()?
            .spec
            .and_then(|s| s.template.spec)
            .ok_or_else(|| "Deployment has no pod spec".to_owned())
    }

    fn container(&self) -> Result<Container, String> {
        let pod = self.pod_spec()?;
        match pod.containers.as_slice() {
            [only] => Ok(only.clone()),
            other => Err(format!("expected one container, found {}", other.len())),
        }
    }

    /// Every image referenced by the Deployment's containers.
    fn images(&self) -> Result<Vec<String>, String> {
        let pod = self.pod_spec()?;
        Ok(pod
            .containers
            .iter()
            .chain(pod.init_containers.iter().flatten())
            .map(|c| c.image.clone().unwrap_or_default())
            .collect())
    }

    /// `(kind, name)` of every cluster-scoped object.
    fn cluster_scoped(&self) -> BTreeSet<(String, String)> {
        self.objects
            .iter()
            .filter(|o| CLUSTER_SCOPED.contains(&kind_of(o)))
            .map(|o| (kind_of(o).to_owned(), name_of(o).to_owned()))
            .collect()
    }
}

/// Runs `helm` against the chart in the working tree.
struct Helm {
    program: OsString,
    chart: PathBuf,
}

impl Helm {
    fn new(root: &Path) -> Self {
        Self {
            program: env::var_os("HELM").unwrap_or_else(|| "helm".into()),
            chart: root.join(CHART_DIR),
        }
    }

    /// Runs helm with `args`; `Ok(stdout)` on success, `Err(stderr)` when
    /// helm exits non-zero. Failing to start helm is a hard error.
    fn run(&self, args: &[&str]) -> Result<Result<String, String>, BoxError> {
        let output = Command::new(&self.program)
            .args(args)
            .output()
            .map_err(|e| {
                format!(
                    "cannot run `{}` ({e}); install Helm 3.x or 4.x or set HELM",
                    self.program.to_string_lossy()
                )
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Ok(if output.status.success() {
            Ok(stdout)
        } else {
            Err(format!("{stderr}{stdout}"))
        })
    }

    fn version(&self) -> Result<String, BoxError> {
        self.run(&["version", "--short"])?
            .map(|v| v.trim().to_owned())
            .map_err(|e| format!("`helm version` failed: {e}").into())
    }

    fn chart_arg(&self) -> Result<&str, BoxError> {
        self.chart
            .to_str()
            .ok_or_else(|| "chart path is not valid UTF-8".into())
    }

    fn with_sets<'a>(mut args: Vec<&'a str>, sets: &[&'a str]) -> Vec<&'a str> {
        for set in sets {
            args.push("--set");
            args.push(set);
        }
        args
    }

    fn lint(&self, sets: &[&str]) -> Result<Result<String, String>, BoxError> {
        let args = Self::with_sets(vec!["lint", "--strict", self.chart_arg()?], sets);
        self.run(&args)
    }

    /// `helm template` with `sets`; `Ok(Err(stderr))` when helm rejects them.
    fn template(&self, sets: &[&str]) -> Result<Result<Rendered, String>, BoxError> {
        let args = Self::with_sets(
            vec![
                "template",
                RELEASE,
                self.chart_arg()?,
                "--namespace",
                NAMESPACE,
            ],
            sets,
        );
        match self.run(&args)? {
            Ok(stdout) => {
                let objects = parse_documents(&stdout)
                    .map_err(|e| format!("cannot parse rendered chart: {e}"))?;
                Ok(Ok(Rendered { objects }))
            }
            Err(stderr) => Ok(Err(stderr)),
        }
    }

    /// `helm template` that must succeed.
    fn render(&self, sets: &[&str]) -> Result<Rendered, String> {
        match self.template(sets) {
            Ok(Ok(rendered)) => Ok(rendered),
            Ok(Err(stderr)) => Err(format!("helm template {sets:?} failed:\n{stderr}")),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Shared state of the checks: helm, the chart metadata and the renders of
/// the standard value sets.
struct Ctx {
    root: PathBuf,
    helm: Helm,
    app_version: String,
    default: Rendered,
    namespace_mode: Rendered,
    mirror_digest: Rendered,
    service_monitor: Rendered,
    crds_off: Rendered,
}

impl Ctx {
    fn new(root: PathBuf, helm: Helm) -> Result<Self, String> {
        let chart_yaml = fs::read_to_string(helm.chart.join("Chart.yaml"))
            .map_err(|e| format!("cannot read {CHART_DIR}/Chart.yaml: {e}"))?;
        let chart: Value = serde_norway::from_str(&chart_yaml)
            .map_err(|e| format!("cannot parse Chart.yaml: {e}"))?;
        let app_version = str_at(&chart, &["appVersion"])
            .ok_or("Chart.yaml has no appVersion")?
            .to_owned();
        let mirror_sets = [
            format!("global.imageRegistry={MIRROR}"),
            format!("image.digest={DIGEST}"),
            "imagePullSecrets[0].name=mirror-pull".to_owned(),
        ];
        let mirror_sets: Vec<&str> = mirror_sets.iter().map(String::as_str).collect();
        Ok(Self {
            default: helm.render(&[])?,
            namespace_mode: helm.render(NAMESPACE_MODE)?,
            mirror_digest: helm.render(&mirror_sets)?,
            service_monitor: helm.render(&["serviceMonitor.enabled=true"])?,
            crds_off: helm.render(&["crds.install=false"])?,
            root,
            helm,
            app_version,
        })
    }

    fn standard_renders(&self) -> [(&'static str, &Rendered); 5] {
        [
            ("default", &self.default),
            ("namespace mode", &self.namespace_mode),
            ("digest + global registry", &self.mirror_digest),
            ("serviceMonitor on", &self.service_monitor),
            ("crds off", &self.crds_off),
        ]
    }
}

/// A policy rule normalised for order-independent comparison.
type NormalRule = (
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Option<Vec<String>>,
    Option<Vec<String>>,
);

/// Normalises rules: each list sorted, then the rules themselves sorted.
pub fn normalize_rules(rules: &[PolicyRule]) -> Vec<NormalRule> {
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v
    };
    let mut out: Vec<NormalRule> = rules
        .iter()
        .map(|r| {
            (
                sorted(r.api_groups.as_deref().unwrap_or_default()),
                sorted(r.resources.as_deref().unwrap_or_default()),
                sorted(&r.verbs),
                r.resource_names.clone(),
                r.non_resource_urls.clone(),
            )
        })
        .collect();
    out.sort();
    out
}

fn rule(group: &str, resources: &[&str], verbs: &[&str]) -> PolicyRule {
    PolicyRule {
        api_groups: Some(vec![group.to_owned()]),
        resources: Some(resources.iter().map(|r| (*r).to_owned()).collect()),
        verbs: verbs.iter().map(|v| (*v).to_owned()).collect(),
        ..PolicyRule::default()
    }
}

/// Design §9: every operator permission except leases.
fn expected_operator_rules() -> Vec<PolicyRule> {
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
        rule(
            "",
            &["secrets"],
            &["get", "list", "watch", "create", "patch", "update"],
        ),
        rule("events.k8s.io", &["events"], &["create", "patch"]),
    ]
}

/// Design §9: the lease permissions (operator namespace only).
fn expected_lease_rules() -> Vec<PolicyRule> {
    vec![rule(
        "coordination.k8s.io",
        &["leases"],
        &["get", "create", "update"],
    )]
}

fn rules_of(value: &Value) -> Result<Vec<PolicyRule>, String> {
    let rules = value.get("rules").cloned().unwrap_or(Value::Null);
    if rules.is_null() {
        return Ok(Vec::new());
    }
    serde_norway::from_value(rules)
        .map_err(|e| format!("cannot decode rules of {}: {e}", name_of(value)))
}

fn ensure_rules(what: &str, value: &Value, expected: &[PolicyRule]) -> Result<(), String> {
    ensure_eq(
        &format!("rules of {what}"),
        &normalize_rules(&rules_of(value)?),
        &normalize_rules(expected),
    )
}

/// The `ServiceAccount` the Deployment runs as, checked against the rendered
/// `ServiceAccount`.
fn service_account(rendered: &Rendered) -> Result<String, String> {
    let sa = rendered.one("ServiceAccount")?;
    ensure_eq(
        "ServiceAccount namespace",
        &namespace_of(sa),
        &Some(NAMESPACE),
    )?;
    let pod_sa = rendered
        .pod_spec()?
        .service_account_name
        .unwrap_or_default();
    ensure_eq("pod serviceAccountName", &pod_sa.as_str(), &name_of(sa))?;
    Ok(pod_sa)
}

fn expected_subject(sa: &str) -> Vec<Subject> {
    vec![Subject {
        kind: "ServiceAccount".to_owned(),
        name: sa.to_owned(),
        namespace: Some(NAMESPACE.to_owned()),
        api_group: None,
    }]
}

/// Checks a (Cluster)RoleBinding: binds `role_kind`/`role_name` to the
/// operator `ServiceAccount`.
fn ensure_binding(
    binding: &Value,
    role_kind: &str,
    role_name: &str,
    sa: &str,
) -> Result<(), String> {
    let what = format!("{} {}", kind_of(binding), name_of(binding));
    let field = |key: &str| binding.get(key).cloned().unwrap_or(Value::Null);
    let role_ref: RoleRef = serde_norway::from_value(field("roleRef"))
        .map_err(|e| format!("{what}: cannot decode roleRef: {e}"))?;
    let subjects: Option<Vec<Subject>> = serde_norway::from_value(field("subjects"))
        .map_err(|e| format!("{what}: cannot decode subjects: {e}"))?;
    ensure_eq(
        &format!("{what} roleRef"),
        &(
            role_ref.api_group.as_str(),
            role_ref.kind.as_str(),
            role_ref.name.as_str(),
        ),
        &("rbac.authorization.k8s.io", role_kind, role_name),
    )?;
    let subjects: Vec<Subject> = subjects
        .unwrap_or_default()
        .into_iter()
        .map(|mut s| {
            // `apiGroup: ""` and an absent apiGroup are equivalent.
            s.api_group = s.api_group.filter(|g| !g.is_empty());
            s
        })
        .collect();
    ensure_eq(
        &format!("{what} subjects"),
        &subjects,
        &expected_subject(sa),
    )
}

/// The lease Role and its binding live in the release namespace.
fn ensure_lease_rbac(rendered: &Rendered, sa: &str) -> Result<(), String> {
    let leases: Vec<&Value> = rendered
        .of_kind("Role")
        .filter(|r| {
            rules_of(r).is_ok_and(|rules| {
                rules
                    .iter()
                    .any(|rule| rule.resources.iter().flatten().any(|res| res == "leases"))
            })
        })
        .collect();
    let [lease] = leases.as_slice() else {
        return Err(format!("expected one lease Role, found {}", leases.len()));
    };
    ensure_eq(
        "lease Role namespace",
        &namespace_of(lease),
        &Some(NAMESPACE),
    )?;
    ensure_rules("lease Role", lease, &expected_lease_rules())?;
    let bindings: Vec<&Value> = rendered
        .of_kind("RoleBinding")
        .filter(|b| str_at(b, &["roleRef", "name"]) == Some(name_of(lease)))
        .collect();
    let [binding] = bindings.as_slice() else {
        return Err(format!(
            "expected one RoleBinding for the lease Role, found {}",
            bindings.len()
        ));
    };
    ensure_eq(
        "lease RoleBinding namespace",
        &namespace_of(binding),
        &Some(NAMESPACE),
    )?;
    ensure_binding(binding, "Role", name_of(lease), sa)
}

fn chart_rbac_cluster_mode(ctx: &Ctx) -> Result<(), String> {
    let r = &ctx.default;
    let sa = service_account(r)?;
    let role = r.one("ClusterRole")?;
    ensure_rules("ClusterRole", role, &expected_operator_rules())?;
    let binding = r.one("ClusterRoleBinding")?;
    ensure_binding(binding, "ClusterRole", name_of(role), &sa)?;
    ensure_lease_rbac(r, &sa)?;
    ensure_eq(
        "Roles in cluster mode (lease only)",
        &r.of_kind("Role").count(),
        &1,
    )?;
    ensure_eq(
        "RoleBindings in cluster mode",
        &r.of_kind("RoleBinding").count(),
        &1,
    )
}

fn chart_rbac_namespace_mode(ctx: &Ctx) -> Result<(), String> {
    let r = &ctx.namespace_mode;
    let sa = service_account(r)?;
    let cluster: BTreeSet<String> = r.cluster_scoped().into_iter().map(|(k, _)| k).collect();
    ensure_eq(
        "cluster-scoped kinds in namespace mode",
        &cluster,
        &BTreeSet::from(["CustomResourceDefinition".to_owned()]),
    )?;
    ensure_lease_rbac(r, &sa)?;
    let mut namespaces = Vec::new();
    for role in r.of_kind("Role") {
        let rules = rules_of(role)?;
        if rules
            .iter()
            .any(|rule| rule.resources.iter().flatten().any(|res| res == "leases"))
        {
            continue;
        }
        let ns = namespace_of(role).unwrap_or_default();
        ensure_rules(&format!("Role in {ns}"), role, &expected_operator_rules())?;
        let bindings: Vec<&Value> = r
            .of_kind("RoleBinding")
            .filter(|b| {
                namespace_of(b) == Some(ns)
                    && str_at(b, &["roleRef", "name"]) == Some(name_of(role))
            })
            .collect();
        let [binding] = bindings.as_slice() else {
            return Err(format!(
                "expected one RoleBinding for the Role in {ns}, found {}",
                bindings.len()
            ));
        };
        ensure_binding(binding, "Role", name_of(role), &sa)?;
        namespaces.push(ns.to_owned());
    }
    namespaces.sort();
    ensure_eq(
        "operator Role namespaces",
        &namespaces,
        &vec!["a".to_owned(), "b".to_owned()],
    )?;
    ensure_eq(
        "RoleBindings in namespace mode",
        &r.of_kind("RoleBinding").count(),
        &3,
    )
}

fn single_image(r: &Rendered) -> Result<String, String> {
    match r.images()?.as_slice() {
        [only] => Ok(only.clone()),
        other => Err(format!("expected one image, found {other:?}")),
    }
}

fn chart_image_reference(ctx: &Ctx) -> Result<(), String> {
    let repo = "jabbas/secretspec-operator";
    // Image tags carry a `v` prefix; the chart appVersion does not.
    ensure_eq(
        "default image",
        &single_image(&ctx.default)?,
        &format!("ghcr.io/{repo}:v{}", ctx.app_version),
    )?;
    ensure_eq(
        "digest + global registry image",
        &single_image(&ctx.mirror_digest)?,
        &format!("{MIRROR}/{repo}@{DIGEST}"),
    )?;
    let pull_secrets: Vec<String> = ctx
        .mirror_digest
        .pod_spec()?
        .image_pull_secrets
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.name)
        .collect();
    ensure_eq(
        "imagePullSecrets",
        &pull_secrets,
        &vec!["mirror-pull".to_owned()],
    )?;
    ensure_eq(
        "default imagePullSecrets",
        &ctx.default
            .pod_spec()?
            .image_pull_secrets
            .unwrap_or_default()
            .len(),
        &0,
    )?;

    let digest_set = format!("image.digest={DIGEST}");
    let digest_only = ctx.helm.render(&[&digest_set, "image.tag=v9.9.9"])?;
    ensure_eq(
        "digest wins over tag",
        &single_image(&digest_only)?,
        &format!("ghcr.io/{repo}@{DIGEST}"),
    )?;
    let mirror_set = format!("global.imageRegistry={MIRROR}");
    let mirror_only = ctx.helm.render(&[
        &mirror_set,
        "image.registry=registry.example.org",
        "image.tag=v1.2.3",
    ])?;
    ensure_eq(
        "global.imageRegistry wins over image.registry",
        &single_image(&mirror_only)?,
        &format!("{MIRROR}/{repo}:v1.2.3"),
    )?;
    let registry_only = ctx.helm.render(&["image.registry=registry.example.org"])?;
    ensure_eq(
        "image.registry override",
        &single_image(&registry_only)?,
        &format!("registry.example.org/{repo}:v{}", ctx.app_version),
    )?;
    ensure_eq(
        "imagePullPolicy",
        &ctx.default.container()?.image_pull_policy,
        &Some("IfNotPresent".to_owned()),
    )
}

fn chart_security_context_restricted(ctx: &Ctx) -> Result<(), String> {
    for (label, r) in ctx.standard_renders() {
        let pod = r.pod_spec()?;
        let psc = pod.security_context.clone().unwrap_or_default();
        ensure_eq(
            &format!("{label}: runAsNonRoot"),
            &psc.run_as_non_root,
            &Some(true),
        )?;
        ensure_eq(
            &format!("{label}: runAsUser"),
            &psc.run_as_user,
            &Some(65532),
        )?;
        ensure_eq(
            &format!("{label}: runAsGroup"),
            &psc.run_as_group,
            &Some(65532),
        )?;
        ensure_eq(&format!("{label}: fsGroup"), &psc.fs_group, &Some(65532))?;
        ensure_eq(
            &format!("{label}: seccompProfile"),
            &psc.seccomp_profile.map(|s| s.type_),
            &Some("RuntimeDefault".to_owned()),
        )?;
        ensure_eq(
            &format!("{label}: automountServiceAccountToken"),
            &pod.automount_service_account_token,
            &Some(true),
        )?;
        ensure(
            pod.host_network != Some(true) && pod.host_pid != Some(true),
            || format!("{label}: pod uses host namespaces"),
        )?;
        for c in pod
            .containers
            .iter()
            .chain(pod.init_containers.iter().flatten())
        {
            let sc = c.security_context.clone().unwrap_or_default();
            let what = format!("{label}: container {}", c.name);
            ensure_eq(
                &format!("{what} allowPrivilegeEscalation"),
                &sc.allow_privilege_escalation,
                &Some(false),
            )?;
            ensure_eq(
                &format!("{what} readOnlyRootFilesystem"),
                &sc.read_only_root_filesystem,
                &Some(true),
            )?;
            ensure(sc.privileged != Some(true), || {
                format!("{what} is privileged")
            })?;
            let caps = sc.capabilities.unwrap_or_default();
            ensure_eq(
                &format!("{what} capabilities.drop"),
                &caps.drop,
                &Some(vec!["ALL".to_owned()]),
            )?;
            ensure(caps.add.unwrap_or_default().is_empty(), || {
                format!("{what} adds capabilities")
            })?;
        }
    }
    Ok(())
}

fn env_map(container: &Container) -> Vec<(String, EnvVar)> {
    container
        .env
        .iter()
        .flatten()
        .map(|e| (e.name.clone(), e.clone()))
        .collect()
}

fn env_value(env: &[(String, EnvVar)], name: &str) -> Result<String, String> {
    env.iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, e)| e.value.clone())
        .ok_or_else(|| format!("env {name} missing or not a plain value"))
}

fn env_field_ref(env: &[(String, EnvVar)], name: &str) -> Result<String, String> {
    env.iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, e)| e.value_from.as_ref())
        .and_then(|v| v.field_ref.as_ref())
        .map(|f| f.field_path.clone())
        .ok_or_else(|| format!("env {name} missing or not a downward-API fieldRef"))
}

fn chart_downward_api_env(ctx: &Ctx) -> Result<(), String> {
    let env = env_map(&ctx.default.container()?);
    ensure_eq(
        "POD_NAME fieldRef",
        &env_field_ref(&env, "POD_NAME")?,
        &"metadata.name".to_owned(),
    )?;
    ensure_eq(
        "POD_NAMESPACE fieldRef",
        &env_field_ref(&env, "POD_NAMESPACE")?,
        &"metadata.namespace".to_owned(),
    )?;
    let expected = [
        ("WATCH_NAMESPACES", ""),
        ("LEADER_ELECTION", "true"),
        ("LEASE_NAME", RELEASE),
        ("RESYNC_INTERVAL", "1h"),
        ("HTTP_BIND", "0.0.0.0:8080"),
        ("LOG_LEVEL", "info"),
        ("LOG_FORMAT", "json"),
        ("CONCURRENCY", "4"),
    ];
    for (name, value) in expected {
        ensure_eq(
            &format!("env {name}"),
            &env_value(&env, name)?,
            &value.to_owned(),
        )?;
    }
    let names: BTreeSet<&str> = env.iter().map(|(n, _)| n.as_str()).collect();
    ensure_eq("env var count (no duplicates)", &names.len(), &env.len())?;

    let ns_env = env_map(&ctx.namespace_mode.container()?);
    ensure_eq(
        "namespace mode WATCH_NAMESPACES",
        &env_value(&ns_env, "WATCH_NAMESPACES")?,
        &"a,b".to_owned(),
    )?;

    let custom = ctx.helm.render(&[
        "http.port=9090",
        "logFormat=text",
        "logLevel=debug",
        "resyncInterval=10m",
        "concurrency=8",
    ])?;
    let custom_env = env_map(&custom.container()?);
    for (name, value) in [
        ("HTTP_BIND", "0.0.0.0:9090"),
        ("LOG_FORMAT", "text"),
        ("LOG_LEVEL", "debug"),
        ("RESYNC_INTERVAL", "10m"),
        ("CONCURRENCY", "8"),
    ] {
        ensure_eq(
            &format!("custom env {name}"),
            &env_value(&custom_env, name)?,
            &value.to_owned(),
        )?;
    }
    let port = custom
        .container()?
        .ports
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.name.as_deref() == Some("http"))
        .map(|p| p.container_port);
    ensure_eq("custom containerPort http", &port, &Some(9090))
}

fn http_get(probe: Option<&Probe>, what: &str) -> Result<(String, String), String> {
    let get = probe
        .and_then(|p| p.http_get.as_ref())
        .ok_or_else(|| format!("{what} is missing or not an httpGet probe"))?;
    let port = match &get.port {
        k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::String(s) => s.clone(),
        k8s_openapi::apimachinery::pkg::util::intstr::IntOrString::Int(i) => i.to_string(),
    };
    Ok((get.path.clone().unwrap_or_default(), port))
}

fn chart_probes(ctx: &Ctx) -> Result<(), String> {
    let c = ctx.default.container()?;
    let http = ("/healthz".to_owned(), "http".to_owned());
    ensure_eq(
        "startupProbe",
        &http_get(c.startup_probe.as_ref(), "startupProbe")?,
        &http,
    )?;
    ensure_eq(
        "livenessProbe",
        &http_get(c.liveness_probe.as_ref(), "livenessProbe")?,
        &http,
    )?;
    ensure_eq(
        "readinessProbe",
        &http_get(c.readiness_probe.as_ref(), "readinessProbe")?,
        &("/readyz".to_owned(), "http".to_owned()),
    )?;
    let port = c
        .ports
        .unwrap_or_default()
        .into_iter()
        .find(|p| p.name.as_deref() == Some("http"))
        .map(|p| {
            (
                p.container_port,
                p.protocol.unwrap_or_else(|| "TCP".to_owned()),
            )
        });
    ensure_eq("containerPort http", &port, &Some((8080, "TCP".to_owned())))
}

fn chart_metrics_service(ctx: &Ctx) -> Result<(), String> {
    let svc = ctx.default.one("Service")?;
    ensure_eq("Service namespace", &namespace_of(svc), &Some(NAMESPACE))?;
    let svc_port = svc
        .get("spec")
        .and_then(|s| s.get("ports"))
        .and_then(Value::as_sequence)
        .and_then(|p| p.first())
        .ok_or("Service has no ports")?;
    ensure_eq(
        "Service port",
        &(
            str_at(svc_port, &["name"]),
            str_at(svc_port, &["targetPort"]),
        ),
        &(Some("http"), Some("http")),
    )?;
    let no_svc = ctx.helm.render(&["metrics.service.enabled=false"])?;
    ensure_eq(
        "Services with metrics.service.enabled=false",
        &no_svc.of_kind("Service").count(),
        &0,
    )?;
    ensure_eq(
        "ServiceMonitors by default",
        &ctx.default.of_kind("ServiceMonitor").count(),
        &0,
    )?;

    let sm = ctx.service_monitor.one("ServiceMonitor")?;
    ensure_eq(
        "ServiceMonitor namespace",
        &namespace_of(sm),
        &Some(NAMESPACE),
    )?;
    let endpoint = sm
        .get("spec")
        .and_then(|s| s.get("endpoints"))
        .and_then(Value::as_sequence)
        .and_then(|e| e.first())
        .ok_or("ServiceMonitor has no endpoints")?;
    ensure_eq(
        "ServiceMonitor endpoint",
        &(
            str_at(endpoint, &["port"]),
            str_at(endpoint, &["path"]),
            str_at(endpoint, &["interval"]),
        ),
        &(Some("http"), Some("/metrics"), Some("30s")),
    )?;
    let svc = ctx.service_monitor.one("Service")?;
    let selector = sm
        .get("spec")
        .and_then(|s| s.get("selector"))
        .and_then(|s| s.get("matchLabels"));
    let labels = svc.get("metadata").and_then(|m| m.get("labels"));
    let selector = selector
        .and_then(Value::as_mapping)
        .ok_or("ServiceMonitor has no matchLabels")?;
    let labels = labels
        .and_then(Value::as_mapping)
        .ok_or("Service has no labels")?;
    ensure(
        selector.iter().all(|(k, v)| labels.get(k) == Some(v)),
        || "ServiceMonitor selector does not match the Service labels".to_owned(),
    )?;
    match ctx
        .helm
        .template(&[
            "serviceMonitor.enabled=true",
            "metrics.service.enabled=false",
        ])
        .map_err(|e| e.to_string())?
    {
        Ok(_) => Err("serviceMonitor without the metrics Service must fail".to_owned()),
        Err(_) => Ok(()),
    }
}

fn crd_from_file(root: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(root.join(CRD_PATH))
        .map_err(|e| format!("cannot read {CRD_PATH}: {e}"))?;
    let docs = parse_documents(&text).map_err(|e| format!("cannot parse {CRD_PATH}: {e}"))?;
    match docs.as_slice() {
        [only] => Ok(only.clone()),
        other => Err(format!(
            "{CRD_PATH}: expected one document, found {}",
            other.len()
        )),
    }
}

/// Path and values of the first difference between `actual` and `expected`.
pub fn first_difference(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    match (actual, expected) {
        (Value::Mapping(a), Value::Mapping(e)) => {
            for (key, ev) in e {
                let child = format!("{path}.{}", key.as_str().unwrap_or("?"));
                match a.get(key) {
                    Some(av) => {
                        if let Some(diff) = first_difference(av, ev, &child) {
                            return Some(diff);
                        }
                    }
                    None => return Some(format!("{child}: missing in rendered output")),
                }
            }
            a.keys()
                .find(|k| !e.contains_key(*k))
                .map(|k| format!("{path}.{}: unexpected key", k.as_str().unwrap_or("?")))
        }
        (Value::Sequence(a), Value::Sequence(e)) => {
            if a.len() != e.len() {
                return Some(format!("{path}: length {} != {}", a.len(), e.len()));
            }
            a.iter()
                .zip(e)
                .enumerate()
                .find_map(|(i, (av, ev))| first_difference(av, ev, &format!("{path}[{i}]")))
        }
        // Helm's YAML round trip writes `1024.0` as `1024`; the API server
        // reads both as the same JSON number.
        (Value::Number(a), Value::Number(e)) if a.as_f64() == e.as_f64() => None,
        _ if actual == expected => None,
        _ => Some(format!(
            "{path}:\n  expected: {expected:?}\n  actual:   {actual:?}"
        )),
    }
}

fn chart_crd_keep_annotation(ctx: &Ctx) -> Result<(), String> {
    let crd = ctx.default.one("CustomResourceDefinition")?;
    ensure_eq(
        "CRD helm.sh/resource-policy (crds.keep=true)",
        &str_at(crd, &["metadata", "annotations", "helm.sh/resource-policy"]),
        &Some("keep"),
    )?;
    let no_keep = ctx.helm.render(&["crds.keep=false"])?;
    let crd = no_keep.one("CustomResourceDefinition")?;
    ensure_eq(
        "CRD helm.sh/resource-policy (crds.keep=false)",
        &str_at(crd, &["metadata", "annotations", "helm.sh/resource-policy"]),
        &None,
    )?;
    ensure_eq(
        "CRDs with crds.install=false",
        &ctx.crds_off.of_kind("CustomResourceDefinition").count(),
        &0,
    )
}

fn chart_crd_matches_generated_file(ctx: &Ctx) -> Result<(), String> {
    let file = crd_from_file(&ctx.root)?;
    let rendered = ctx.default.one("CustomResourceDefinition")?;
    for field in ["apiVersion", "kind"] {
        ensure_eq(field, &rendered.get(field), &file.get(field))?;
    }
    ensure_eq("CRD name", &name_of(rendered), &name_of(&file))?;
    if let Some(diff) = first_difference(
        rendered.get("spec").unwrap_or(&Value::Null),
        file.get("spec").unwrap_or(&Value::Null),
        "spec",
    ) {
        return Err(format!(
            "rendered CRD spec differs from {CRD_PATH}:\n{diff}"
        ));
    }
    ensure_eq(
        "CRD app.kubernetes.io/managed-by label",
        &str_at(
            rendered,
            &["metadata", "labels", "app.kubernetes.io/managed-by"],
        ),
        &Some("Helm"),
    )?;
    ensure_eq("CRD namespace", &namespace_of(rendered), &None)
}

fn chart_rejects_multi_replica_without_leader_election(ctx: &Ctx) -> Result<(), String> {
    match ctx
        .helm
        .template(&["leaderElection.enabled=false", "replicas=2"])
        .map_err(|e| e.to_string())?
    {
        Ok(_) => return Err("replicas=2 without leader election rendered".to_owned()),
        Err(stderr) => ensure(stderr.contains("leaderElection.enabled"), || {
            format!("failure message does not mention leaderElection.enabled:\n{stderr}")
        })?,
    }
    let ha = ctx.helm.render(&["replicas=2"])?;
    ensure_eq(
        "replicas with leader election",
        &ha.deployment()?.spec.and_then(|s| s.replicas),
        &Some(2),
    )?;
    let single = ctx.helm.render(&["leaderElection.enabled=false"])?;
    let env = env_map(&single.container()?);
    ensure_eq(
        "LEADER_ELECTION (disabled)",
        &env_value(&env, "LEADER_ELECTION")?,
        &"false".to_owned(),
    )?;
    ensure_eq(
        "Roles without leader election",
        &single.of_kind("Role").count(),
        &0,
    )
}

fn chart_no_latest_tag(ctx: &Ctx) -> Result<(), String> {
    for (label, r) in ctx.standard_renders() {
        for image in r.images()? {
            let last = image.rsplit('/').next().unwrap_or_default();
            ensure(last.contains('@') || last.contains(':'), || {
                format!("{label}: image {image} has neither tag nor digest")
            })?;
            ensure(!image.ends_with(":latest"), || {
                format!("{label}: image {image} uses latest")
            })?;
        }
    }
    match ctx
        .helm
        .template(&["image.tag=latest"])
        .map_err(|e| e.to_string())?
    {
        Ok(_) => Err("image.tag=latest must be rejected".to_owned()),
        Err(_) => Ok(()),
    }
}

fn chart_version_matches_workspace(ctx: &Ctx) -> Result<(), String> {
    let chart_yaml =
        fs::read_to_string(ctx.helm.chart.join("Chart.yaml")).map_err(|e| e.to_string())?;
    let chart: Value = serde_norway::from_str(&chart_yaml).map_err(|e| e.to_string())?;
    let workspace = env!("CARGO_PKG_VERSION");
    ensure_eq(
        "Chart version",
        &str_at(&chart, &["version"]),
        &Some(workspace),
    )?;
    ensure_eq("Chart appVersion", &ctx.app_version.as_str(), &workspace)
}

type Check = fn(&Ctx) -> Result<(), String>;

/// Every check run by `cargo xtask chart-test`.
const CHECKS: &[(&str, Check)] = &[
    ("chart_rbac_cluster_mode", chart_rbac_cluster_mode),
    ("chart_rbac_namespace_mode", chart_rbac_namespace_mode),
    ("chart_image_reference", chart_image_reference),
    (
        "chart_security_context_restricted",
        chart_security_context_restricted,
    ),
    ("chart_downward_api_env", chart_downward_api_env),
    ("chart_probes", chart_probes),
    ("chart_metrics_service", chart_metrics_service),
    ("chart_crd_keep_annotation", chart_crd_keep_annotation),
    (
        "chart_crd_matches_generated_file",
        chart_crd_matches_generated_file,
    ),
    (
        "chart_rejects_multi_replica_without_leader_election",
        chart_rejects_multi_replica_without_leader_election,
    ),
    ("chart_no_latest_tag", chart_no_latest_tag),
    (
        "chart_version_matches_workspace",
        chart_version_matches_workspace,
    ),
];

/// Entry point of `cargo xtask chart-test`.
pub fn chart_test() -> Result<ExitCode, BoxError> {
    let root = workspace_root()?;
    let helm = Helm::new(&root);
    println!("using {}", helm.version()?);

    let mut failed = 0usize;
    for (label, sets) in [
        ("default values", &[][..]),
        ("namespace mode", NAMESPACE_MODE),
    ] {
        match helm.lint(sets)? {
            Ok(_) => println!("ok   helm lint --strict ({label})"),
            Err(out) => {
                failed += 1;
                println!("FAIL helm lint --strict ({label})\n{out}");
            }
        }
    }

    let ctx = match Ctx::new(root, helm) {
        Ok(ctx) => ctx,
        Err(message) => {
            println!("FAIL render standard value sets\n{message}");
            return Ok(ExitCode::FAILURE);
        }
    };
    for (name, check) in CHECKS {
        match check(&ctx) {
            Ok(()) => println!("ok   {name}"),
            Err(message) => {
                failed += 1;
                println!("FAIL {name}\n  {}", message.replace('\n', "\n  "));
            }
        }
    }
    let total = CHECKS.len() + 2;
    println!("chart-test: {} passed, {failed} failed", total - failed);
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_documents_skips_empty_and_comment_only_documents() {
        let yaml = "---\n# Source: x\n---\nkind: A\n---\n\n---\nkind: B\n";
        let docs = parse_documents(yaml).expect("parse");
        let kinds: Vec<&str> = docs.iter().map(kind_of).collect();
        assert_eq!(kinds, ["A", "B"]);
    }

    #[test]
    fn parse_documents_reports_malformed_yaml() {
        assert!(parse_documents("kind: [unclosed\n").is_err());
    }

    #[test]
    fn normalize_rules_ignores_order() {
        let a = vec![
            rule("", &["secrets"], &["get", "list"]),
            rule("events.k8s.io", &["events"], &["create"]),
        ];
        let b = vec![
            rule("events.k8s.io", &["events"], &["create"]),
            rule("", &["secrets"], &["list", "get"]),
        ];
        assert_eq!(normalize_rules(&a), normalize_rules(&b));
        let c = vec![rule("", &["secrets"], &["get", "list", "delete"])];
        assert_ne!(normalize_rules(&a), normalize_rules(&c));
    }

    #[test]
    fn expected_rules_match_design_section_9() {
        let verbs: Vec<(String, Vec<String>)> = expected_operator_rules()
            .into_iter()
            .chain(expected_lease_rules())
            .map(|r| (r.resources.unwrap_or_default().join(","), r.verbs))
            .collect();
        assert!(
            verbs
                .iter()
                .all(|(_, v)| !v.iter().any(|verb| verb == "delete" || verb == "*")),
            "no delete or wildcard verbs: {verbs:?}"
        );
        assert_eq!(verbs.len(), 6);
    }
}
