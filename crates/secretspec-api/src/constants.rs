// SPDX-License-Identifier: Apache-2.0

//! Frozen names and defaults shared by the API types and the controller.
//!
//! Values marked "frozen" are part of the `v1alpha1` contract: changing one
//! would change resolved generation parameters (and therefore fingerprints),
//! which rotates credentials. See the design document, section 4.5.

use crate::v1alpha1::EcdsaCurve;

/// API group of the `SecretSpec` resource.
pub const GROUP: &str = "secretspec.jabbas.eu";

/// Label marking Secrets owned by the operator.
pub const LABEL_MANAGED_BY: &str = "app.kubernetes.io/managed-by";
/// Value of [`LABEL_MANAGED_BY`] on operator-owned Secrets.
pub const MANAGED_BY_VALUE: &str = "secretspec-operator";
/// Label carrying the name of the owning `SecretSpec`.
pub const LABEL_SPEC_NAME: &str = "secretspec.jabbas.eu/spec-name";

/// Annotation holding the JSON map of generation slot -> parameter fingerprint.
pub const ANNOTATION_FINGERPRINTS: &str = "secretspec.jabbas.eu/fingerprints";
/// Annotation holding the RFC 3339 time of the last full generation.
pub const ANNOTATION_GENERATED_AT: &str = "secretspec.jabbas.eu/generated-at";
/// Annotation holding the UID of the `SecretSpec` that owns the Secret.
pub const ANNOTATION_SPEC_UID: &str = "secretspec.jabbas.eu/spec-uid";

/// Label and annotation keys under this prefix are reserved for the operator.
pub const RESERVED_PREFIX: &str = "secretspec.jabbas.eu/";

/// Frozen: length of a generated string when `length` is unset.
pub const DEFAULT_LENGTH: u16 = 32;
/// Frozen: RSA modulus size when `algorithm: rsa` and `rsaBits` is unset.
pub const DEFAULT_RSA_BITS: u16 = 4096;
/// Frozen: curve when `algorithm: ecdsa` and `ecdsaCurve` is unset.
pub const DEFAULT_ECDSA_CURVE: EcdsaCurve = EcdsaCurve::P256;
/// Frozen: bcrypt cost of the generated htpasswd line.
pub const BCRYPT_COST: u32 = 12;
/// Upper bound of `length` and `bytes`.
pub const MAX_RANDOM_LENGTH: u16 = 1024;

/// Declares the validation messages once so that the CEL rules (which embed
/// them in the CRD), `resolve()` (which reports the same rejections) and the
/// tests all share the exact strings. `ALL_MESSAGES` lists every message.
macro_rules! messages {
    ($($(#[$meta:meta])* $name:ident = $text:expr;)*) => {
        $($(#[$meta])* pub const $name: &str = $text;)*
        /// Every `MSG_*` validation message, in declaration order.
        pub const ALL_MESSAGES: &[&str] = &[$($name),*];
    };
}

// Frozen: these strings are part of the public API (users see them in
// `kubectl apply` errors and may match on them).
messages! {
    /// `spec`: exactly one of the three generator kinds.
    MSG_EXACTLY_ONE_KIND = "exactly one of fields, sshKeyPair or basicAuth must be set";
    /// `fields[]`: random-bytes mode excludes characters mode.
    MSG_BYTES_EXCLUSIVE = "bytes cannot be combined with length, charset, alphabet, exclude or require";
    /// `fields[]`: `encoding` goes with `bytes` and only with it.
    MSG_ENCODING_WITH_BYTES = "encoding is required with bytes and only allowed with bytes";
    /// `fields[]` and `basicAuth.password`: `alphabet` iff `charset: custom`.
    MSG_ALPHABET_WITH_CUSTOM = "alphabet is required when charset is custom and only allowed then";
    /// `fields[]` and `basicAuth.password`: `require` fits into the length.
    MSG_REQUIRE_SUM = "the sum of require counts must not exceed length (default 32)";
    /// `rotation.interval`: between 1h and 3650d.
    MSG_ROTATION_RANGE = "rotation.interval must be between 1h and 3650d";
    /// `sshKeyPair`: `rsaBits` needs `algorithm: rsa`.
    MSG_RSA_BITS_ONLY_RSA = "rsaBits is only allowed with algorithm rsa";
    /// `sshKeyPair`: `ecdsaCurve` needs `algorithm: ecdsa`.
    MSG_CURVE_ONLY_ECDSA = "ecdsaCurve is only allowed with algorithm ecdsa";
    /// `sshKeyPair.keys` and `basicAuth.keys`: no two roles share a key.
    MSG_KEYS_DISTINCT = "secret key names in keys must be distinct";
    /// `spec`: literal `data` keys must not shadow generated keys.
    MSG_DATA_COLLISION = "data keys must not collide with generated keys";
    /// `data`: valid Secret key names.
    MSG_DATA_KEY_FORMAT = "data keys must match [-._a-zA-Z0-9]+ and be at most 253 characters";
    /// `secret.labels` and `secret.annotations`: operator-owned keys.
    MSG_RESERVED_METADATA = "labels and annotations under secretspec.jabbas.eu/ and app.kubernetes.io/managed-by are reserved";
    /// `spec`: the `kubernetes.io/basic-auth` type contract.
    MSG_BASIC_AUTH_TYPE = r#"type kubernetes.io/basic-auth requires basicAuth with keys.username "username" and keys.password "password""#;
    /// `spec`: the `kubernetes.io/ssh-auth` type contract.
    MSG_SSH_AUTH_TYPE = r#"type kubernetes.io/ssh-auth requires sshKeyPair with keys.privateKey "ssh-privatekey""#;
    /// `secret.type`: a Secret's type cannot change.
    MSG_TYPE_IMMUTABLE = "secret.type is immutable";
}
