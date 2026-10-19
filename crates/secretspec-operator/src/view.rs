// SPDX-License-Identifier: Apache-2.0

//! The existing Secret as the planner should see it.

use std::collections::BTreeSet;

use k8s_openapi::api::core::v1::Secret;
use secretspec_api::constants::{ANNOTATION_KEYS, FIELD_MANAGER};
use secretspec_api::resolve;
use secretspec_api::resolve::{ResolvedKind, ResolvedSpec};
use secretspec_api::v1alpha1::SecretSpec;
use secretspec_core::plan::keys::KeyMap;

/// The existing Secret as the planner should see it: data keys owned only by other
/// field managers are removed, so they never make the Secret look out of date.
///
/// A key is hidden only if BOTH: (a) the spec does not name it (literal key, field
/// name, `sshKeyPair.keys.*`, `basicAuth.keys.*`, or a key recorded in
/// [`ANNOTATION_KEYS`]) and (b) the `secretspec-operator` Apply entry in
/// `managedFields` exists and does not list it under `f:data`. If our entry is
/// absent or unparseable, or the spec does not resolve, nothing is hidden. Only
/// `data` differs from `existing` in the result.
#[must_use]
pub fn owned_view(existing: &Secret, spec: &SecretSpec) -> Secret {
    let mut view = existing.clone();
    let Some(owned) = apply_owned_data_keys(existing) else {
        return view;
    };
    let name = spec.metadata.name.as_deref().unwrap_or_default();
    let namespace = spec.metadata.namespace.as_deref().unwrap_or_default();
    let Ok(resolved) = resolve(&spec.spec, name, namespace) else {
        return view;
    };
    let named = named_keys(&resolved, existing);
    if let Some(data) = view.data.as_mut() {
        data.retain(|key, _| owned.contains(key) || named.contains(key));
    }
    view
}

/// The `f:data` keys of our server-side-apply entry; `None` when there is no
/// such entry or any matching entry cannot be read.
fn apply_owned_data_keys(secret: &Secret) -> Option<BTreeSet<String>> {
    let mut entries = secret
        .metadata
        .managed_fields
        .iter()
        .flatten()
        .filter(|entry| {
            entry.manager.as_deref() == Some(FIELD_MANAGER)
                && entry.operation.as_deref() == Some("Apply")
                && entry.subresource.is_none()
        })
        .peekable();
    entries.peek()?;
    let mut owned = BTreeSet::new();
    for entry in entries {
        let fields = entry.fields_v1.as_ref()?.0.as_object()?;
        let Some(data) = fields.get("f:data") else {
            continue;
        };
        for field in data.as_object()?.keys() {
            if let Some(key) = field.strip_prefix("f:") {
                owned.insert(key.to_owned());
            }
        }
    }
    Some(owned)
}

/// Every Secret key the spec names, plus those the keys annotation records.
fn named_keys(resolved: &ResolvedSpec, existing: &Secret) -> BTreeSet<String> {
    let mut named: BTreeSet<String> = resolved.data.keys().cloned().collect();
    match &resolved.kind {
        ResolvedKind::Fields(fields) => {
            named.extend(fields.iter().map(|field| field.name.clone()));
        }
        ResolvedKind::SshKeyPair(ssh) => {
            named.insert(ssh.private_key_key.clone());
            named.insert(ssh.public_key_key.clone());
        }
        ResolvedKind::BasicAuth(basic) => {
            named.insert(basic.username_key.clone());
            named.insert(basic.password_key.clone());
            named.insert(basic.htpasswd_key.clone());
        }
    }
    if let Some(recorded) = existing
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(ANNOTATION_KEYS))
    {
        named.extend(KeyMap::from_annotation(recorded).0.into_values());
    }
    named
}

#[cfg(test)]
mod tests {
    // Tests report through panics.
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use k8s_openapi::ByteString;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{FieldsV1, ManagedFieldsEntry};
    use secretspec_api::constants::{ANNOTATION_KEYS, FIELD_MANAGER};
    use secretspec_api::v1alpha1::SecretSpecSpec;
    use serde_json::{Value, json};

    use super::*;

    fn spec(body: &Value) -> SecretSpec {
        let body: SecretSpecSpec = serde_json::from_value(body.clone()).expect("spec");
        let mut spec = SecretSpec::new("db", body);
        spec.metadata.namespace = Some("apps".to_owned());
        spec
    }

    fn password_spec() -> SecretSpec {
        spec(&json!({"fields": [{"name": "password"}]}))
    }

    fn entry(manager: &str, operation: &str, fields: Option<Value>) -> ManagedFieldsEntry {
        ManagedFieldsEntry {
            manager: Some(manager.to_owned()),
            operation: Some(operation.to_owned()),
            fields_v1: fields.map(FieldsV1),
            ..ManagedFieldsEntry::default()
        }
    }

    fn ours(keys: &[&str]) -> ManagedFieldsEntry {
        let data: serde_json::Map<String, Value> = keys
            .iter()
            .map(|key| (format!("f:{key}"), json!({})))
            .collect();
        entry(
            FIELD_MANAGER,
            "Apply",
            Some(json!({"f:data": data, "f:metadata": {"f:labels": {}}})),
        )
    }

    fn secret(keys: &[&str], managed: Option<Vec<ManagedFieldsEntry>>) -> Secret {
        let data = keys
            .iter()
            .map(|key| ((*key).to_owned(), ByteString(b"v".to_vec())))
            .collect::<BTreeMap<_, _>>();
        let mut secret = Secret {
            data: Some(data),
            ..Secret::default()
        };
        secret.metadata.managed_fields = managed;
        secret
    }

    fn keys_of(secret: &Secret) -> Vec<String> {
        secret
            .data
            .iter()
            .flat_map(|data| data.keys().cloned())
            .collect()
    }

    #[test]
    fn foreign_keys_are_ignored() {
        let existing = secret(&["password", "extra"], Some(vec![ours(&["password"])]));
        let view = owned_view(&existing, &password_spec());
        assert_eq!(keys_of(&view), ["password"]);
    }

    #[test]
    fn spec_named_keys_are_never_hidden() {
        let existing = secret(&["password", "extra"], Some(vec![ours(&["other"])]));
        let view = owned_view(&existing, &password_spec());
        assert_eq!(keys_of(&view), ["password"]);
    }

    #[test]
    fn literal_data_keys_are_never_hidden() {
        let spec = spec(&json!({"fields": [{"name": "password"}], "data": {"user": "admin"}}));
        let existing = secret(&["password", "user"], Some(vec![ours(&["password"])]));
        let view = owned_view(&existing, &spec);
        assert_eq!(keys_of(&view), ["password", "user"]);
    }

    #[test]
    fn missing_or_garbage_managed_fields_hides_nothing() {
        let cases: Vec<Option<Vec<ManagedFieldsEntry>>> = vec![
            None,
            Some(vec![]),
            // Only a foreign manager.
            Some(vec![entry("kubectl", "Apply", Some(json!({"f:data": {}})))]),
            // Our manager, but not an Apply entry.
            Some(vec![entry(
                FIELD_MANAGER,
                "Update",
                Some(json!({"f:data": {}})),
            )]),
            // Our Apply entry without fieldsV1.
            Some(vec![entry(FIELD_MANAGER, "Apply", None)]),
            // Our Apply entry with unparseable fieldsV1.
            Some(vec![entry(FIELD_MANAGER, "Apply", Some(json!("garbage")))]),
            Some(vec![entry(
                FIELD_MANAGER,
                "Apply",
                Some(json!({"f:data": 7})),
            )]),
        ];
        for managed in cases {
            let existing = secret(&["password", "extra"], managed.clone());
            let view = owned_view(&existing, &password_spec());
            assert_eq!(keys_of(&view), ["extra", "password"], "{managed:?}");
        }
    }

    #[test]
    fn invalid_spec_hides_nothing() {
        // No generator kind: `resolve` fails.
        let spec = spec(&json!({}));
        let existing = secret(&["password", "extra"], Some(vec![ours(&["password"])]));
        let view = owned_view(&existing, &spec);
        assert_eq!(keys_of(&view), ["extra", "password"]);
    }

    #[test]
    fn recorded_role_key_is_kept() {
        let spec =
            spec(&json!({"basicAuth": {"username": "admin", "keys": {"htpasswd": "users"}}}));
        let mut existing = secret(
            &["auth", "extra", "password", "username"],
            Some(vec![ours(&["password", "username", "users"])]),
        );
        existing.metadata.annotations = Some(BTreeMap::from([(
            ANNOTATION_KEYS.to_owned(),
            r#"{"basic-auth/htpasswd":"auth"}"#.to_owned(),
        )]));
        let view = owned_view(&existing, &spec);
        assert_eq!(keys_of(&view), ["auth", "password", "username"]);
    }

    #[test]
    fn only_data_is_changed() {
        let existing = secret(&["password", "extra"], Some(vec![ours(&["password"])]));
        let view = owned_view(&existing, &password_spec());
        let mut expected = existing.clone();
        expected.data = view.data.clone();
        assert_eq!(view, expected);
        assert_eq!(
            view.metadata.managed_fields,
            existing.metadata.managed_fields
        );
    }
}
