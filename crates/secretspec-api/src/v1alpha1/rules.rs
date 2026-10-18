// SPDX-License-Identifier: Apache-2.0

//! The CEL validation rules (`x-kubernetes-validations`) of the CRD.
//!
//! Each function returns one rule; the type definitions attach them with
//! `#[x_kube(validation = rules::...())]` at the narrowest schema node that has
//! everything the rule reads. Messages are the frozen `MSG_*` constants so the
//! controller can report the same text for the same rejection.
//!
//! Conventions that keep the rules cheap for the API server's cost estimator:
//! rules over maps and lists iterate only collections that carry
//! `maxProperties` / `maxItems`, and compare only strings that carry
//! `maxLength`. Rules assume schema defaults were applied (the API server does
//! that before validation), so `secret`, `algorithm` and `keys` are present.

use kube::core::Rule;

use crate::constants::{
    self, DEFAULT_LENGTH, MSG_ALPHABET_WITH_CUSTOM, MSG_BASIC_AUTH_TYPE, MSG_BYTES_EXCLUSIVE,
    MSG_CURVE_ONLY_ECDSA, MSG_DATA_COLLISION, MSG_DATA_KEY_FORMAT, MSG_ENCODING_WITH_BYTES,
    MSG_EXACTLY_ONE_KIND, MSG_KEYS_DISTINCT, MSG_REQUIRE_SUM, MSG_RESERVED_METADATA,
    MSG_ROTATION_RANGE, MSG_RSA_BITS_ONLY_RSA, MSG_SSH_AUTH_TYPE, MSG_TYPE_IMMUTABLE,
};

/// `spec`: exactly one of `fields`, `sshKeyPair`, `basicAuth`.
pub(crate) fn exactly_one_kind() -> Rule {
    Rule::new(
        "(has(self.fields) ? 1 : 0) + (has(self.sshKeyPair) ? 1 : 0) \
         + (has(self.basicAuth) ? 1 : 0) == 1",
    )
    .message(MSG_EXACTLY_ONE_KIND)
}

/// `fields[]`: `bytes` excludes every characters-mode option.
pub(crate) fn bytes_exclusive() -> Rule {
    Rule::new(
        "!has(self.bytes) || (!has(self.length) && !has(self.charset) \
         && !has(self.alphabet) && !has(self.exclude) && !has(self.require))",
    )
    .message(MSG_BYTES_EXCLUSIVE)
}

/// `fields[]`: `encoding` is present exactly when `bytes` is.
pub(crate) fn encoding_with_bytes() -> Rule {
    Rule::new("has(self.encoding) == has(self.bytes)").message(MSG_ENCODING_WITH_BYTES)
}

/// `fields[]` and `basicAuth.password`: `alphabet` exactly when `charset: custom`.
pub(crate) fn alphabet_with_custom() -> Rule {
    Rule::new("has(self.alphabet) == (has(self.charset) && self.charset == 'custom')")
        .message(MSG_ALPHABET_WITH_CUSTOM)
}

/// `fields[]` and `basicAuth.password`: the `require` counts fit into the
/// effective length (`length`, or [`DEFAULT_LENGTH`] when unset).
pub(crate) fn require_sum() -> Rule {
    let class = |name: &str| format!("(has(self.require.{name}) ? self.require.{name} : 0)");
    Rule::new(format!(
        "!has(self.require) || {} + {} + {} + {} <= (has(self.length) ? self.length : {DEFAULT_LENGTH})",
        class("lower"),
        class("upper"),
        class("digits"),
        class("symbols"),
    ))
    .message(MSG_REQUIRE_SUM)
}

/// `rotation.interval`: 60m..=5256000m, 1h..=87600h or 1d..=3650d. The unit
/// letter and digits themselves are constrained by the schema pattern.
pub(crate) fn rotation_range() -> Rule {
    let n = "int(self.substring(0, size(self) - 1))";
    Rule::new(format!(
        "(self.endsWith('m') && {n} >= 60 && {n} <= 5256000) \
         || (self.endsWith('h') && {n} >= 1 && {n} <= 87600) \
         || (self.endsWith('d') && {n} >= 1 && {n} <= 3650)"
    ))
    .message(MSG_ROTATION_RANGE)
}

/// `sshKeyPair`: `rsaBits` only with `algorithm: rsa`.
pub(crate) fn rsa_bits_only_rsa() -> Rule {
    Rule::new("!has(self.rsaBits) || self.algorithm == 'rsa'").message(MSG_RSA_BITS_ONLY_RSA)
}

/// `sshKeyPair`: `ecdsaCurve` only with `algorithm: ecdsa`.
pub(crate) fn curve_only_ecdsa() -> Rule {
    Rule::new("!has(self.ecdsaCurve) || self.algorithm == 'ecdsa'").message(MSG_CURVE_ONLY_ECDSA)
}

/// `sshKeyPair.keys`: the two roles use different Secret keys.
pub(crate) fn ssh_keys_distinct() -> Rule {
    Rule::new("self.privateKey != self.publicKey").message(MSG_KEYS_DISTINCT)
}

/// `basicAuth.keys`: the three roles use different Secret keys.
pub(crate) fn basic_auth_keys_distinct() -> Rule {
    Rule::new(
        "self.username != self.password && self.username != self.htpasswd \
         && self.password != self.htpasswd",
    )
    .message(MSG_KEYS_DISTINCT)
}

/// `spec`: no literal `data` key equals a generated Secret key.
pub(crate) fn data_collision() -> Rule {
    Rule::new(
        "!has(self.data) || ( \
           (!has(self.fields) || !self.fields.exists(f, f.name in self.data)) \
           && (!has(self.sshKeyPair) || !(self.sshKeyPair.keys.privateKey in self.data \
                || self.sshKeyPair.keys.publicKey in self.data)) \
           && (!has(self.basicAuth) || !(self.basicAuth.keys.username in self.data \
                || self.basicAuth.keys.password in self.data \
                || self.basicAuth.keys.htpasswd in self.data)))",
    )
    .message(MSG_DATA_COLLISION)
}

/// `data`: every key is a valid Secret key. Map keys cannot be bounded in an
/// `OpenAPI` schema, so the length bound is part of the rule.
pub(crate) fn data_key_format() -> Rule {
    Rule::new("self.all(k, size(k) <= 253 && k.matches('^[-._a-zA-Z0-9]+$'))")
        .message(MSG_DATA_KEY_FORMAT)
}

/// `secret.labels` and `secret.annotations`: operator-owned keys are reserved.
pub(crate) fn reserved_metadata() -> Rule {
    Rule::new(format!(
        "self.all(k, size(k) <= 253 && !k.startsWith('{}') && k != '{}')",
        constants::RESERVED_PREFIX,
        constants::LABEL_MANAGED_BY,
    ))
    .message(MSG_RESERVED_METADATA)
}

/// `spec`: `kubernetes.io/basic-auth` needs `basicAuth` with the exact keys the
/// type requires.
pub(crate) fn basic_auth_type() -> Rule {
    Rule::new(
        "self.secret.type != 'kubernetes.io/basic-auth' || (has(self.basicAuth) \
         && self.basicAuth.keys.username == 'username' \
         && self.basicAuth.keys.password == 'password')",
    )
    .message(MSG_BASIC_AUTH_TYPE)
}

/// `spec`: `kubernetes.io/ssh-auth` needs `sshKeyPair` with the exact private
/// key name the type requires.
pub(crate) fn ssh_auth_type() -> Rule {
    Rule::new(
        "self.secret.type != 'kubernetes.io/ssh-auth' || (has(self.sshKeyPair) \
         && self.sshKeyPair.keys.privateKey == 'ssh-privatekey')",
    )
    .message(MSG_SSH_AUTH_TYPE)
}

/// `secret.type`: transition rule; a Secret's type cannot change.
pub(crate) fn type_immutable() -> Rule {
    Rule::new("self == oldSelf").message(MSG_TYPE_IMMUTABLE)
}
