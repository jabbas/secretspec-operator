// SPDX-License-Identifier: Apache-2.0

//! `spec.sshKeyPair`.

use std::borrow::Cow;

use kube::KubeSchema;

use super::{bounds, rules};
use crate::constants::{KEY_NAME_PATTERN, MAX_COMMENT_LEN, MAX_KEY_NAME_LEN};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// Generates an unencrypted OpenSSH key pair. The private key is in OpenSSH
/// format; the public key is a single `authorized_keys` line including the
/// comment. The comment is excluded from the fingerprint: changing it only
/// rewrites the public-key line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = rules::rsa_bits_only_rsa())]
#[x_kube(validation = rules::curve_only_ecdsa())]
pub struct SshKeyPairSpec {
    /// Key algorithm: `ed25519` (default), `rsa` or `ecdsa`.
    #[serde(default)]
    pub algorithm: SshAlgorithm,

    /// RSA modulus size in bits (2048, 3072 or 4096); only valid with
    /// `algorithm: rsa`. When unset the operator uses 4096 (not written into
    /// the object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(transform = bounds::plain_enum)]
    pub rsa_bits: Option<RsaBits>,

    /// ECDSA curve (`P-256`, `P-384` or `P-521`); only valid with
    /// `algorithm: ecdsa`. When unset the operator uses `P-256` (not written
    /// into the object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(transform = bounds::plain_enum)]
    pub ecdsa_curve: Option<EcdsaCurve>,

    /// Comment in the public key line. When unset it is `<name>@<namespace>`
    /// of the `SecretSpec`; `""` means no comment. Printable ASCII, at most
    /// 256 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_COMMENT_LEN), regex(pattern = r"^[ -~]*$"))]
    pub comment: Option<String>,

    /// Names of the Secret keys that hold the key pair; they must be distinct
    /// valid Secret key names (not `.`, not starting with `..`).
    /// Renaming a key moves the existing value; it does not regenerate it.
    #[serde(default)]
    pub keys: SshKeys,
}

// SSH key algorithm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SshAlgorithm {
    /// Ed25519 (default).
    #[default]
    Ed25519,
    /// RSA; see `rsaBits`.
    Rsa,
    /// ECDSA; see `ecdsaCurve`.
    Ecdsa,
}

/// RSA modulus size in bits: 2048, 3072 or 4096. Serialised as an integer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub enum RsaBits {
    /// 2048 bits.
    B2048,
    /// 3072 bits.
    B3072,
    /// 4096 bits.
    B4096,
}

impl From<RsaBits> for u16 {
    fn from(bits: RsaBits) -> Self {
        match bits {
            RsaBits::B2048 => 2048,
            RsaBits::B3072 => 3072,
            RsaBits::B4096 => 4096,
        }
    }
}

impl TryFrom<u16> for RsaBits {
    type Error = String;

    fn try_from(bits: u16) -> Result<Self, Self::Error> {
        match bits {
            2048 => Ok(Self::B2048),
            3072 => Ok(Self::B3072),
            4096 => Ok(Self::B4096),
            other => Err(format!(
                "invalid rsaBits {other}: expected 2048, 3072 or 4096"
            )),
        }
    }
}

impl JsonSchema for RsaBits {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "RsaBits".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "integer", "enum": [2048, 3072, 4096] })
    }
}

// ECDSA curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum EcdsaCurve {
    /// NIST P-256.
    #[serde(rename = "P-256")]
    P256,
    /// NIST P-384.
    #[serde(rename = "P-384")]
    P384,
    /// NIST P-521.
    #[serde(rename = "P-521")]
    P521,
}

/// Secret key names for the SSH key pair.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = rules::ssh_keys_distinct())]
pub struct SshKeys {
    /// Secret key holding the private key. Default `ssh-privatekey`.
    #[serde(default = "default_private_key")]
    #[schemars(length(max = MAX_KEY_NAME_LEN), regex(pattern = KEY_NAME_PATTERN))]
    pub private_key: String,

    /// Secret key holding the public key. Default `ssh-publickey`.
    #[serde(default = "default_public_key")]
    #[schemars(length(max = MAX_KEY_NAME_LEN), regex(pattern = KEY_NAME_PATTERN))]
    pub public_key: String,
}

fn default_private_key() -> String {
    "ssh-privatekey".to_owned()
}

fn default_public_key() -> String {
    "ssh-publickey".to_owned()
}

impl Default for SshKeys {
    fn default() -> Self {
        Self {
            private_key: default_private_key(),
            public_key: default_public_key(),
        }
    }
}
