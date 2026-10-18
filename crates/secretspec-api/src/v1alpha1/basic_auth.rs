// SPDX-License-Identifier: Apache-2.0

//! `spec.basicAuth`.

use kube::KubeSchema;
use serde::{Deserialize, Serialize};

use super::{Charset, RequireSpec, rules};
use crate::constants::{
    KEY_NAME_PATTERN, MAX_AFFIX_LEN, MAX_ALPHABET_LEN, MAX_EXCLUDE_LEN, MAX_KEY_NAME_LEN,
    MAX_RANDOM_LENGTH, MAX_USERNAME_LEN, MIN_RANDOM_LENGTH, MIN_USERNAME_LEN,
};

/// Generates a username/password pair plus an htpasswd line
/// (`<username>:<bcrypt hash>`, bcrypt `$2y$`, cost 12).
///
/// Example for Traefik: `{username: admin, keys: {htpasswd: users}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BasicAuthSpec {
    /// The user name, stored verbatim; there is no implicit default.
    /// Visible ASCII without `:` and without spaces, 1-253 characters.
    #[schemars(length(min = MIN_USERNAME_LEN, max = MAX_USERNAME_LEN), regex(pattern = r"^[!-9;-~]+$"))]
    pub username: String,

    /// How the password is generated; same options as `fields` characters
    /// mode. Defaults to 32 alphanumeric characters.
    #[serde(default)]
    pub password: BasicAuthPassword,

    /// Names of the Secret keys that hold the values; they must be distinct
    /// valid Secret key names (not `.`, not starting with `..`).
    #[serde(default)]
    pub keys: BasicAuthKeys,
}

/// Password generation options: the characters-mode options of a `fields`
/// entry, without `name`, `bytes` and `encoding`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[x_kube(validation = rules::alphabet_with_custom())]
#[x_kube(validation = rules::require_sum())]
pub struct BasicAuthPassword {
    /// Number of random characters, 1-1024. When unset the operator uses 32
    /// (not written into the object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = MIN_RANDOM_LENGTH, max = MAX_RANDOM_LENGTH))]
    pub length: Option<u16>,

    /// The alphabet to draw from: `alphanumeric` (a-z, A-Z, 0-9), `ascii`
    /// (adds punctuation), `hex` (0-9a-f) or `custom` (uses `alphabet`). When
    /// unset the operator uses `alphanumeric` (not written into the object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(transform = super::bounds::plain_enum)]
    pub charset: Option<Charset>,

    /// Custom alphabet, required if and only if `charset: custom`. Printable
    /// ASCII without spaces, at most 256 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_ALPHABET_LEN), regex(pattern = r"^[!-~]+$"))]
    pub alphabet: Option<String>,

    /// Characters removed from the effective alphabet. Printable ASCII (space
    /// allowed), at most 256 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_EXCLUDE_LEN), regex(pattern = r"^[ -~]*$"))]
    pub exclude: Option<String>,

    /// Minimum number of characters per class. The sum must not exceed the
    /// effective `length`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require: Option<RequireSpec>,

    /// Text prepended verbatim to the password. Printable ASCII without
    /// spaces, at most 64 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_AFFIX_LEN), regex(pattern = r"^[!-~]*$"))]
    pub prefix: Option<String>,

    /// Text appended verbatim to the password. Printable ASCII without
    /// spaces, at most 64 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_AFFIX_LEN), regex(pattern = r"^[!-~]*$"))]
    pub suffix: Option<String>,
}

/// Secret key names for the basic-auth values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[x_kube(validation = rules::basic_auth_keys_distinct())]
pub struct BasicAuthKeys {
    /// Secret key holding the user name. Default `username`.
    #[serde(default = "default_username_key")]
    #[schemars(length(max = MAX_KEY_NAME_LEN), regex(pattern = KEY_NAME_PATTERN))]
    pub username: String,

    /// Secret key holding the password. Default `password`.
    #[serde(default = "default_password_key")]
    #[schemars(length(max = MAX_KEY_NAME_LEN), regex(pattern = KEY_NAME_PATTERN))]
    pub password: String,

    /// Secret key holding the htpasswd line. Default `auth`; use `users` for
    /// Traefik's `BasicAuth` middleware.
    #[serde(default = "default_htpasswd_key")]
    #[schemars(length(max = MAX_KEY_NAME_LEN), regex(pattern = KEY_NAME_PATTERN))]
    pub htpasswd: String,
}

fn default_username_key() -> String {
    "username".to_owned()
}

fn default_password_key() -> String {
    "password".to_owned()
}

fn default_htpasswd_key() -> String {
    "auth".to_owned()
}

impl Default for BasicAuthKeys {
    fn default() -> Self {
        Self {
            username: default_username_key(),
            password: default_password_key(),
            htpasswd: default_htpasswd_key(),
        }
    }
}
