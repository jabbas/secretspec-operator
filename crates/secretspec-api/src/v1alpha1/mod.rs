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
//
// No type here uses `#[serde(deny_unknown_fields)]`, on purpose. The API server
// already prunes fields the installed CRD does not know; a field the Rust types
// do not know can therefore only come from a CRD newer than the running image
// (an upgrade or rollback in progress). Rejecting it would make every watch
// event of every object fail to deserialize. Tests stay strict instead: the
// fixture loader (`tests/common`) rejects keys the generated CRD does not know.

use std::collections::BTreeMap;

use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::constants::{
    MAX_DATA_ENTRIES, MAX_DATA_VALUE_LEN, MAX_FIELDS, MAX_INTERVAL_LEN, MAX_METADATA_ENTRIES,
    MAX_METADATA_VALUE_LEN, MAX_TYPE_LEN, MIN_FIELDS, MIN_TYPE_LEN,
};

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
    doc = "A SecretSpec declares one Kubernetes Secret that the secretspec operator \
           generates and keeps in sync. The operator creates a Secret with the same \
           name in the same namespace and fills it with generated values: exactly one \
           of spec.fields (random strings and tokens), spec.sshKeyPair (an SSH key \
           pair) or spec.basicAuth (a username, a password and an htpasswd line) must \
           be set. Generated values stay stable until their generation parameters \
           change or a scheduled rotation is due. The name must be at most 63 \
           characters because it is also used as a label value.",
    validation = rules::name_length(),
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
#[serde(rename_all = "camelCase")]
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
    /// `username`). Keys are Secret key names: letters, digits, `-`,
    /// `_` and `.` (at most 253 characters), but not `.` itself and not
    /// starting with `..`. They must not collide with a generated key. At
    /// most 64 entries.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(transform = bounds::string_map(MAX_DATA_ENTRIES, MAX_DATA_VALUE_LEN))]
    #[schemars(transform = bounds::no_schema_default)]
    #[x_kube(validation = rules::data_key_format())]
    pub data: BTreeMap<String, String>,

    /// Generated strings and tokens, one Secret key per entry. Mutually
    /// exclusive with `sshKeyPair` and `basicAuth`. 1 to 32 entries;
    /// `name` is the list key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = MIN_FIELDS, max = MAX_FIELDS))]
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
#[serde(rename_all = "camelCase")]
pub struct RotationSpec {
    /// How often to regenerate every generated value: a positive integer
    /// followed by `m` (minutes), `h` (hours) or `d` (days). Minimum `1h`,
    /// maximum `3650d`. Examples: `90d`, `24h`.
    #[schemars(length(max = MAX_INTERVAL_LEN), regex(pattern = r"^[1-9][0-9]*(m|h|d)$"))]
    #[x_kube(validation = rules::rotation_range())]
    pub interval: String,
}

/// Metadata of the generated Secret.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecretTemplate {
    /// Kubernetes Secret type. Default `Opaque`. Allowed: `Opaque`,
    /// `kubernetes.io/basic-auth`, `kubernetes.io/ssh-auth`, or a custom type
    /// (1-253 characters) that does not start with `kubernetes.io/` and is
    /// not `bootstrap.kubernetes.io/token`; other built-in types such as
    /// service-account tokens are rejected. Immutable after creation because
    /// the Secret type is immutable. `kubernetes.io/basic-auth` requires
    /// `basicAuth` with the default `username` and `password` keys;
    /// `kubernetes.io/ssh-auth` requires `sshKeyPair` with the default
    /// `ssh-privatekey` private key.
    #[serde(rename = "type", default = "default_secret_type")]
    #[schemars(length(min = MIN_TYPE_LEN, max = MAX_TYPE_LEN))]
    #[x_kube(validation = rules::secret_type_allowed())]
    #[x_kube(validation = rules::type_immutable())]
    pub type_: String,

    /// Extra labels for the Secret. Keys under `secretspec.jabbas.eu/` and
    /// `app.kubernetes.io/managed-by` are reserved. Keys are at most 253
    /// characters. At most 32 entries.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(transform = bounds::string_map(MAX_METADATA_ENTRIES, MAX_METADATA_VALUE_LEN))]
    #[schemars(transform = bounds::no_schema_default)]
    #[x_kube(validation = rules::reserved_metadata())]
    pub labels: BTreeMap<String, String>,

    /// Extra annotations for the Secret. Keys under `secretspec.jabbas.eu/`
    /// and `app.kubernetes.io/managed-by` are reserved. Keys are at most 253
    /// characters. At most 32 entries.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(transform = bounds::string_map(MAX_METADATA_ENTRIES, MAX_METADATA_VALUE_LEN))]
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
