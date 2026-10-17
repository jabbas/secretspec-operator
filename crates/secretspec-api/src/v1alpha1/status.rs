// SPDX-License-Identifier: Apache-2.0

//! `SecretSpec` status.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::KubeSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Observed state of a `SecretSpec`, written only by the operator.
///
/// The `Ready` condition summarises health: `True`/`Generated` when the Secret
/// matches the spec; `False` with reason `Conflict` (a Secret with the target
/// name exists and is not managed by the operator), `InvalidSpec` (runtime
/// validation failed; the message says why) or `ApplyFailed` (the write to the
/// API server failed).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecretSpecStatus {
    /// The `metadata.generation` last reconciled by the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,

    /// Which kind of secret this spec produces: `Fields`, `SSHKeyPair` or
    /// `BasicAuth`. Shown in the `Kind` printer column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SecretKind>,

    /// Name of the generated Secret (same as the `SecretSpec`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_name: Option<String>,

    /// Last time any value was (re)generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_generated_time: Option<Time>,

    /// Time of the last scheduled rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_rotation_time: Option<Time>,

    /// Time of the next scheduled rotation. Absent when `spec.rotation` is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_rotation_time: Option<Time>,

    /// Standard Kubernetes conditions; currently only `Ready`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<Condition>,
}

// The kind of secret a `SecretSpec` produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum SecretKind {
    /// Generated strings and tokens (`spec.fields`).
    #[serde(rename = "Fields")]
    Fields,
    /// An SSH key pair (`spec.sshKeyPair`).
    #[serde(rename = "SSHKeyPair")]
    SshKeyPair,
    /// A username/password pair with an htpasswd line (`spec.basicAuth`).
    #[serde(rename = "BasicAuth")]
    BasicAuth,
}
