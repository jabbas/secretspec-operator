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

/// Annotation holding the JSON map of role -> Secret key currently holding
/// that role's value, so that renaming `keys.*` moves the value instead of
/// regenerating it.
pub const ANNOTATION_KEYS: &str = "secretspec.jabbas.eu/keys";

/// Event reason: a Secret was generated.
pub const EVENT_GENERATED: &str = "Generated";
/// Event reason: values were regenerated.
pub const EVENT_REGENERATED: &str = "Regenerated";
/// Event reason: scheduled rotation ran.
pub const EVENT_ROTATED: &str = "Rotated";
/// Event reason: an existing Secret was adopted.
pub const EVENT_ADOPTED: &str = "Adopted";
/// Event reason: the target Secret is not operator-managed.
pub const EVENT_CONFLICT: &str = "Conflict";
/// Event reason: the spec failed validation.
pub const EVENT_INVALID_SPEC: &str = "InvalidSpec";
/// Event reason: a generated value has low entropy.
pub const EVENT_LOW_ENTROPY: &str = "LowEntropy";
/// Event reason: the fingerprint annotation was unreadable and was reset.
pub const EVENT_FINGERPRINTS_RESET: &str = "FingerprintsReset";
/// Event reason: the rotation anchor was unreadable and was reset.
pub const EVENT_ANCHOR_RESET: &str = "AnchorReset";
/// Event reason: applying the Secret failed.
pub const EVENT_APPLY_FAILED: &str = "ApplyFailed";

/// Name of the status condition reporting readiness.
pub const CONDITION_READY: &str = "Ready";
/// `Ready` reason: the Secret is generated and up to date.
pub const REASON_GENERATED: &str = "Generated";
/// `Ready` reason: the target Secret is not operator-managed.
pub const REASON_CONFLICT: &str = "Conflict";
/// `Ready` reason: the spec failed validation.
pub const REASON_INVALID_SPEC: &str = "InvalidSpec";
/// `Ready` reason: applying the Secret failed.
pub const REASON_APPLY_FAILED: &str = "ApplyFailed";
/// `Ready` reason: waiting for the previous owner's garbage collection.
pub const REASON_WAITING_FOR_GC: &str = "WaitingForGarbageCollection";

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

// Schema bounds. The `#[schemars(...)]` attributes of the API types use these
// where the attribute accepts a path, `resolve()` checks the same limits, and
// the `schema_bounds_match_resolve_constants` test pins every one of them
// against the generated CRD.

/// Lower bound of `length` and `bytes`.
pub const MIN_RANDOM_LENGTH: u16 = 1;
/// Upper bound of `length` and `bytes`.
pub const MAX_RANDOM_LENGTH: u16 = 1024;
/// Upper bound of each `require` class count.
pub const MAX_REQUIRE_COUNT: u16 = 1024;
/// Minimum number of `fields` entries.
pub const MIN_FIELDS: usize = 1;
/// Maximum number of `fields` entries.
pub const MAX_FIELDS: usize = 32;
/// Maximum number of `data` entries.
pub const MAX_DATA_ENTRIES: usize = 64;
/// Maximum length of a `data` value.
pub const MAX_DATA_VALUE_LEN: usize = 65_536;
/// Maximum number of `secret.labels` / `secret.annotations` entries.
pub const MAX_METADATA_ENTRIES: usize = 32;
/// Maximum length of a label or annotation value.
pub const MAX_METADATA_VALUE_LEN: usize = 4_096;
/// Maximum length of a label or annotation key.
pub const MAX_METADATA_KEY_LEN: usize = 253;
/// Maximum length of a Secret key name (`data` keys, `fields[].name`, `keys.*`).
pub const MAX_KEY_NAME_LEN: usize = 253;
/// Minimum length of `secret.type`.
pub const MIN_TYPE_LEN: usize = 1;
/// Maximum length of `secret.type`.
pub const MAX_TYPE_LEN: usize = 253;
/// Maximum length of the `SecretSpec` name: it is the value of the
/// [`LABEL_SPEC_NAME`] label, and label values are at most 63 characters.
pub const MAX_NAME_LEN: usize = 63;
/// Secret types the operator may create besides custom types (see
/// [`MSG_SECRET_TYPE_ALLOWED`]).
pub const ALLOWED_BUILTIN_SECRET_TYPES: [&str; 3] = [
    "Opaque",
    "kubernetes.io/basic-auth",
    "kubernetes.io/ssh-auth",
];
/// Types under this prefix are reserved by Kubernetes; only the
/// [`ALLOWED_BUILTIN_SECRET_TYPES`] among them are allowed.
pub const KUBERNETES_TYPE_PREFIX: &str = "kubernetes.io/";
/// The bootstrap token Secret type, which is not allowed.
pub const BOOTSTRAP_TOKEN_TYPE: &str = "bootstrap.kubernetes.io/token";
/// Minimum length of `basicAuth.username`.
pub const MIN_USERNAME_LEN: usize = 1;
/// Maximum length of `basicAuth.username`.
pub const MAX_USERNAME_LEN: usize = 253;
/// Maximum length of `alphabet`.
pub const MAX_ALPHABET_LEN: usize = 256;
/// Maximum length of `exclude`.
pub const MAX_EXCLUDE_LEN: usize = 256;
/// Maximum length of `prefix` and `suffix`.
pub const MAX_AFFIX_LEN: usize = 64;
/// Maximum length of `rotation.interval`.
pub const MAX_INTERVAL_LEN: usize = 8;
/// Maximum length of `sshKeyPair.comment`.
pub const MAX_COMMENT_LEN: usize = 256;

/// Schema pattern (and CEL regex for `data` keys) of a Secret key name.
///
/// Kubernetes accepts `[-._a-zA-Z0-9]+` except `.` and anything starting with
/// `..`. RE2 has no look-ahead, so the rule is phrased positively: an optional
/// single leading dot, then a character that is not a dot, then anything. The
/// dot is written as `[.]` so the same text is valid inside a CEL string.
pub const KEY_NAME_PATTERN: &str = "^[.]?[-_a-zA-Z0-9][-._a-zA-Z0-9]*$";

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
    MSG_DATA_KEY_FORMAT = "data keys must be valid Secret key names: [-._a-zA-Z0-9]+, at most 253 characters, not '.' and not starting with '..'";
    /// `secret.labels` and `secret.annotations`: key length and operator-owned keys.
    MSG_RESERVED_METADATA = "label and annotation keys must be at most 253 characters; keys under secretspec.jabbas.eu/ and app.kubernetes.io/managed-by are reserved";
    /// root: the name is copied into the `secretspec.jabbas.eu/spec-name` label.
    MSG_NAME_LENGTH = "metadata.name must be at most 63 characters because it is used as a label value";
    /// `secret.type`: the Secret types the operator may create.
    MSG_SECRET_TYPE_ALLOWED = "secret.type must be Opaque, kubernetes.io/basic-auth, kubernetes.io/ssh-auth or a custom type outside kubernetes.io/ (bootstrap tokens are not allowed)";
    /// `spec`: the `kubernetes.io/basic-auth` type contract.
    MSG_BASIC_AUTH_TYPE = r#"type kubernetes.io/basic-auth requires basicAuth with keys.username "username" and keys.password "password""#;
    /// `spec`: the `kubernetes.io/ssh-auth` type contract.
    MSG_SSH_AUTH_TYPE = r#"type kubernetes.io/ssh-auth requires sshKeyPair with keys.privateKey "ssh-privatekey""#;
    /// `secret.type`: a Secret's type cannot change.
    MSG_TYPE_IMMUTABLE = "secret.type is immutable";
}
