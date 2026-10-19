// SPDX-License-Identifier: Apache-2.0

//! The role -> Secret key map recorded in the `secretspec.jabbas.eu/keys`
//! annotation.
//!
//! Role-based generators (`sshKeyPair`, `basicAuth`) store each value under a
//! user-chosen Secret key. The annotation remembers which key currently holds
//! which role, so renaming `keys.*` moves the value instead of regenerating it.

use std::collections::BTreeMap;

use secretspec_api::resolve::{ResolvedKind, ResolvedSpec};

use crate::fingerprint::is_key_name;

/// What a value in a role-based Secret is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// The SSH private key.
    SshPrivateKey,
    /// The SSH public key.
    SshPublicKey,
    /// The basic-auth user name.
    BasicAuthUsername,
    /// The basic-auth password.
    BasicAuthPassword,
    /// The basic-auth htpasswd line.
    BasicAuthHtpasswd,
}

impl Role {
    /// The stable wire string used in the annotation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SshPrivateKey => "ssh/private-key",
            Self::SshPublicKey => "ssh/public-key",
            Self::BasicAuthUsername => "basic-auth/username",
            Self::BasicAuthPassword => "basic-auth/password",
            Self::BasicAuthHtpasswd => "basic-auth/htpasswd",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "ssh/private-key" => Some(Self::SshPrivateKey),
            "ssh/public-key" => Some(Self::SshPublicKey),
            "basic-auth/username" => Some(Self::BasicAuthUsername),
            "basic-auth/password" => Some(Self::BasicAuthPassword),
            "basic-auth/htpasswd" => Some(Self::BasicAuthHtpasswd),
            _ => None,
        }
    }
}

/// Role -> Secret key name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyMap(pub BTreeMap<Role, String>);

impl KeyMap {
    /// The key map a spec asks for; empty for `fields`.
    #[must_use]
    pub fn for_spec(spec: &ResolvedSpec) -> Self {
        let entries: Vec<(Role, &str)> = match &spec.kind {
            ResolvedKind::Fields(_) => Vec::new(),
            ResolvedKind::SshKeyPair(ssh) => vec![
                (Role::SshPrivateKey, ssh.private_key_key.as_str()),
                (Role::SshPublicKey, ssh.public_key_key.as_str()),
            ],
            ResolvedKind::BasicAuth(basic) => vec![
                (Role::BasicAuthUsername, basic.username_key.as_str()),
                (Role::BasicAuthPassword, basic.password_key.as_str()),
                (Role::BasicAuthHtpasswd, basic.htpasswd_key.as_str()),
            ],
        };
        Self(
            entries
                .into_iter()
                .map(|(role, key)| (role, key.to_owned()))
                .collect(),
        )
    }

    /// Compact JSON object with entries sorted by wire string.
    #[must_use]
    pub fn to_annotation(&self) -> String {
        // `Role`'s derived order is declaration order, which is not the wire
        // order, so sort through a map keyed by the wire string.
        let by_wire: BTreeMap<&str, &str> = self
            .0
            .iter()
            .map(|(role, key)| (role.as_str(), key.as_str()))
            .collect();
        // Serializing string-to-string maps cannot fail; `{}` is the safe
        // reading of "nothing recorded" anyway.
        serde_json::to_string(&by_wire).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Tolerant reader: invalid JSON gives an empty map; unknown roles,
    /// invalid key names and non-string values are ignored.
    #[must_use]
    pub fn from_annotation(s: &str) -> Self {
        let Ok(serde_json::Value::Object(object)) = serde_json::from_str(s) else {
            return Self::default();
        };
        Self(
            object
                .into_iter()
                .filter_map(|(wire, value)| {
                    let role = Role::parse(&wire)?;
                    let serde_json::Value::String(key) = value else {
                        return None;
                    };
                    is_key_name(&key).then_some((role, key))
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    // Tests report through panics.
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use secretspec_api::resolve::resolve;
    use secretspec_api::v1alpha1::SecretSpecSpec;

    use super::*;

    fn keymap_of(yaml: &str) -> KeyMap {
        let spec: SecretSpecSpec = serde_norway::from_str(yaml).expect("manifest deserializes");
        let resolved = resolve(&spec, "n", "ns").expect("valid spec");
        KeyMap::for_spec(&resolved)
    }

    fn map(entries: &[(Role, &str)]) -> KeyMap {
        KeyMap(entries.iter().map(|(r, k)| (*r, (*k).to_owned())).collect())
    }

    #[test]
    fn for_spec_ssh_defaults() {
        assert_eq!(
            keymap_of("sshKeyPair: {}"),
            map(&[
                (Role::SshPrivateKey, "ssh-privatekey"),
                (Role::SshPublicKey, "ssh-publickey"),
            ])
        );
    }

    #[test]
    fn for_spec_basic_auth_traefik() {
        assert_eq!(
            keymap_of("basicAuth: {username: u, keys: {htpasswd: users}}"),
            map(&[
                (Role::BasicAuthUsername, "username"),
                (Role::BasicAuthPassword, "password"),
                (Role::BasicAuthHtpasswd, "users"),
            ])
        );
    }

    #[test]
    fn for_spec_fields_is_empty() {
        assert_eq!(keymap_of("fields: [{name: password}]"), KeyMap::default());
    }

    #[test]
    fn annotation_round_trip_sorted_compact() {
        // Inserted in the opposite order of the wire strings.
        let keys = map(&[(Role::SshPublicKey, "b"), (Role::SshPrivateKey, "a")]);
        let annotation = keys.to_annotation();
        assert_eq!(
            annotation,
            r#"{"ssh/private-key":"a","ssh/public-key":"b"}"#
        );
        assert_eq!(KeyMap::from_annotation(&annotation), keys);

        let all = map(&[
            (Role::BasicAuthHtpasswd, "h"),
            (Role::BasicAuthPassword, "p"),
            (Role::BasicAuthUsername, "u"),
        ]);
        assert_eq!(
            all.to_annotation(),
            r#"{"basic-auth/htpasswd":"h","basic-auth/password":"p","basic-auth/username":"u"}"#
        );
        assert_eq!(KeyMap::default().to_annotation(), "{}");
    }

    #[test]
    fn from_annotation_is_tolerant() {
        assert_eq!(KeyMap::from_annotation("x"), KeyMap::default());
        assert_eq!(KeyMap::from_annotation(""), KeyMap::default());
        assert_eq!(KeyMap::from_annotation("[]"), KeyMap::default());
        assert_eq!(
            KeyMap::from_annotation(
                r#"{"ssh/private-key":"a","bogus/role":"x","ssh/public-key":"..","basic-auth/username":7,"basic-auth/password":"p"}"#
            ),
            map(&[(Role::SshPrivateKey, "a"), (Role::BasicAuthPassword, "p"),])
        );
    }
}
