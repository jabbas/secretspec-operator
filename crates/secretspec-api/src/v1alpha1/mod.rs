// SPDX-License-Identifier: Apache-2.0

//! `secretspec.jabbas.eu/v1alpha1` API types.

mod basic_auth;
mod bounds;
mod fields;
mod rules;
mod ssh;
mod status;

// Doc-comment convention: enums use plain `//` comments at type level. A `///`
// on an enum becomes its schema description and overrides the description of
// every field that uses it in the generated CRD, hiding the field's own docs.

use std::collections::BTreeMap;

use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use basic_auth::{BasicAuthKeys, BasicAuthPassword, BasicAuthSpec};
pub use fields::{Charset, Encoding, FieldSpec, RequireSpec};
pub use ssh::{EcdsaCurve, RsaBits, SshAlgorithm, SshKeyPairSpec, SshKeys};
pub use status::{SecretKind, SecretSpecStatus};

/// Describes one Kubernetes Secret that the operator generates and keeps in
/// sync. The Secret has the same name and namespace as the `SecretSpec`.
///
/// Exactly one of `fields`, `sshKeyPair` or `basicAuth` must be set. An
/// application that needs both a password and an SSH key uses two
/// `SecretSpec` objects.
///
/// Example:
///
/// ```yaml
/// spec:
///   deletionPolicy: Retain
///   rotation: { interval: 90d }
///   data: { username: app }
///   fields:
///     - name: password
/// ```
#[derive(CustomResource, Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
// `type_` repeats once per printer column; clippy misreads that as a duplicate.
#[allow(clippy::duplicated_attributes)]
#[kube(
    group = "secretspec.jabbas.eu",
    version = "v1alpha1",
    kind = "SecretSpec",
    plural = "secretspecs",
    shortname = "ss",
    namespaced,
    status = "SecretSpecStatus",
    derive = "PartialEq",
    printcolumn(
        name = "Ready",
        type_ = "string",
        json_path = r#".status.conditions[?(@.type=="Ready")].status"#
    ),
    printcolumn(name = "Kind", type_ = "string", json_path = ".status.kind"),
    printcolumn(
        name = "Next Rotation",
        type_ = "date",
        json_path = ".status.nextRotationTime"
    ),
    printcolumn(
        name = "Age",
        type_ = "date",
        json_path = ".metadata.creationTimestamp"
    )
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[x_kube(validation = rules::exactly_one_kind())]
#[x_kube(validation = rules::data_collision())]
#[x_kube(validation = rules::basic_auth_type())]
#[x_kube(validation = rules::ssh_auth_type())]
pub struct SecretSpecSpec {
    /// What happens to the generated Secret when this `SecretSpec` is deleted.
    /// `Delete` (default) removes it; `Retain` leaves it in place, detached
    /// from this object.
    #[serde(default)]
    pub deletion_policy: DeletionPolicy,

    /// Optional scheduled rotation. Without it the generated values are
    /// stable until the spec changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<RotationSpec>,

    /// Metadata of the generated Secret: its type, extra labels and extra
    /// annotations. Defaults to `{type: Opaque}`.
    #[serde(default)]
    pub secret: SecretTemplate,

    /// Literal values copied verbatim into the Secret (for example a fixed
    /// `username`). Keys must match `^[-._a-zA-Z0-9]+$` (at most 253
    /// characters) and must not collide with a generated key. At most 64
    /// entries.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(transform = bounds::string_map(64, 65536))]
    #[schemars(transform = bounds::no_schema_default)]
    #[x_kube(validation = rules::data_key_format())]
    pub data: BTreeMap<String, String>,

    /// Generated strings and tokens, one Secret key per entry. Mutually
    /// exclusive with `sshKeyPair` and `basicAuth`. At most 32 entries;
    /// `name` is the list key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 32))]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["name"]))]
    pub fields: Option<Vec<FieldSpec>>,

    /// Generates an SSH key pair. Mutually exclusive with `fields` and
    /// `basicAuth`. Example: `sshKeyPair: {}` produces an ed25519 key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key_pair: Option<SshKeyPairSpec>,

    /// Generates a username/password pair plus an htpasswd line. Mutually
    /// exclusive with `fields` and `sshKeyPair`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basic_auth: Option<BasicAuthSpec>,
}

// What happens to the generated Secret when the `SecretSpec` is deleted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DeletionPolicy {
    /// Delete the Secret together with the `SecretSpec`.
    #[default]
    Delete,
    /// Keep the Secret; it is detached from the deleted `SecretSpec`.
    Retain,
}

/// Scheduled rotation of all generated values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotationSpec {
    /// How often to regenerate every generated value: a positive integer
    /// followed by `m` (minutes), `h` (hours) or `d` (days). Minimum `1h`,
    /// maximum `3650d`. Examples: `90d`, `24h`.
    #[schemars(length(max = 8), regex(pattern = r"^[1-9][0-9]*(m|h|d)$"))]
    #[x_kube(validation = rules::rotation_range())]
    pub interval: String,
}

/// Metadata of the generated Secret.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SecretTemplate {
    /// Kubernetes Secret type. Default `Opaque`. Immutable after creation
    /// because the Secret type is immutable. `kubernetes.io/basic-auth`
    /// requires `basicAuth` with the default `username` and `password` keys;
    /// `kubernetes.io/ssh-auth` requires `sshKeyPair` with the default
    /// `ssh-privatekey` private key.
    #[serde(rename = "type", default = "default_secret_type")]
    #[schemars(length(max = 253))]
    #[x_kube(validation = rules::type_immutable())]
    pub type_: String,

    /// Extra labels for the Secret. Keys under `secretspec.jabbas.eu/` and
    /// `app.kubernetes.io/managed-by` are reserved. Keys are at most 253
    /// characters. At most 32 entries.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(transform = bounds::string_map(32, 4096))]
    #[schemars(transform = bounds::no_schema_default)]
    #[x_kube(validation = rules::reserved_metadata())]
    pub labels: BTreeMap<String, String>,

    /// Extra annotations for the Secret. Keys under `secretspec.jabbas.eu/`
    /// and `app.kubernetes.io/managed-by` are reserved. Keys are at most 253
    /// characters. At most 32 entries.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(transform = bounds::string_map(32, 4096))]
    #[schemars(transform = bounds::no_schema_default)]
    #[x_kube(validation = rules::reserved_metadata())]
    pub annotations: BTreeMap<String, String>,
}

fn default_secret_type() -> String {
    "Opaque".to_owned()
}

impl Default for SecretTemplate {
    fn default() -> Self {
        Self {
            type_: default_secret_type(),
            labels: BTreeMap::new(),
            annotations: BTreeMap::new(),
        }
    }
}
