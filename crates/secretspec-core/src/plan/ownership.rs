// SPDX-License-Identifier: Apache-2.0

//! Who owns the Secret that already exists under a `SecretSpec`'s name.

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use secretspec_api::constants::{
    ANNOTATION_SPEC_UID, GROUP, LABEL_MANAGED_BY, LABEL_SPEC_NAME, MANAGED_BY_VALUE,
};
use secretspec_api::v1alpha1::{DeletionPolicy, SecretSpec};

/// How the existing Secret (if any) relates to a `SecretSpec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ownership {
    /// There is no Secret.
    Absent,
    /// The Secret is ours: right labels, and either its controller reference
    /// or its `spec-uid` annotation names this `SecretSpec`'s UID.
    Owned,
    /// The Secret carries our labels for this spec name but is bound to no
    /// controller and not to this UID (for example a recreated `SecretSpec`,
    /// or a `Retain`-detached Secret): it may be taken over.
    Adoptable,
    /// The Secret carries our labels but a controller reference points at a
    /// different UID: wait for garbage collection to remove it.
    WaitForGc,
    /// The Secret is not managed by this `SecretSpec`; the message says so.
    Conflict(String),
}

/// Classifies `secret` against `spec`. Precedence, first match wins:
/// no Secret → `Absent`; wrong `managed-by` or `spec-name` label → `Conflict`;
/// a controller reference to another UID → `WaitForGc`; a controller
/// reference to this UID → `Owned`; no controller reference and a matching
/// `spec-uid` annotation → `Owned`; otherwise `Adoptable`. A spec without a
/// UID is never `Owned`: matching labels give `Adoptable`.
#[must_use]
pub fn ownership(secret: Option<&Secret>, spec: &SecretSpec) -> Ownership {
    let Some(secret) = secret else {
        return Ownership::Absent;
    };
    let meta = &secret.metadata;
    let label = |name: &str| {
        meta.labels
            .as_ref()
            .and_then(|map| map.get(name))
            .map(String::as_str)
    };

    let ours = label(LABEL_MANAGED_BY) == Some(MANAGED_BY_VALUE)
        && spec
            .metadata
            .name
            .as_deref()
            .is_some_and(|name| label(LABEL_SPEC_NAME) == Some(name));
    if !ours {
        let name = meta
            .name
            .as_deref()
            .or(spec.metadata.name.as_deref())
            .unwrap_or_default();
        return Ownership::Conflict(format!(
            "Secret {name} exists and is not managed by this SecretSpec"
        ));
    }

    let Some(spec_uid) = spec.metadata.uid.as_deref() else {
        return Ownership::Adoptable;
    };
    let controller = meta
        .owner_references
        .iter()
        .flatten()
        .find(|reference| reference.controller == Some(true));
    match controller {
        Some(reference) if reference.uid == spec_uid => Ownership::Owned,
        Some(_) => Ownership::WaitForGc,
        None => {
            let recorded = meta
                .annotations
                .as_ref()
                .and_then(|map| map.get(ANNOTATION_SPEC_UID))
                .map(String::as_str);
            if recorded == Some(spec_uid) {
                Ownership::Owned
            } else {
                Ownership::Adoptable
            }
        }
    }
}

/// The owner references the generated Secret should carry: under `Delete`, one
/// controller reference to the `SecretSpec` (so garbage collection removes the
/// Secret with it); under `Retain`, none. `None` also when the spec lacks a
/// name or UID, which the API server never allows.
#[must_use]
pub fn owner_references(spec: &SecretSpec, policy: DeletionPolicy) -> Option<Vec<OwnerReference>> {
    match policy {
        DeletionPolicy::Retain => None,
        DeletionPolicy::Delete => {
            let name = spec.metadata.name.clone()?;
            let uid = spec.metadata.uid.clone()?;
            Some(vec![OwnerReference {
                api_version: format!("{GROUP}/v1alpha1"),
                kind: "SecretSpec".to_owned(),
                name,
                uid,
                controller: Some(true),
                block_owner_deletion: Some(true),
            }])
        }
    }
}

#[cfg(test)]
mod tests {
    // Tests report through panics.
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use secretspec_api::v1alpha1::SecretSpecSpec;

    use super::*;

    fn spec(uid: Option<&str>) -> SecretSpec {
        let body: SecretSpecSpec = serde_json::from_str(r#"{"basicAuth":{"username":"admin"}}"#)
            .expect("spec deserializes");
        let mut spec = SecretSpec::new("db", body);
        spec.metadata.uid = uid.map(str::to_owned);
        spec
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn secret(
        labels: &[(&str, &str)],
        annotations: &[(&str, &str)],
        owners: Option<Vec<OwnerReference>>,
    ) -> Secret {
        Secret {
            metadata: ObjectMeta {
                name: Some("db".to_owned()),
                labels: Some(map(labels)),
                annotations: Some(map(annotations)),
                owner_references: owners,
                ..ObjectMeta::default()
            },
            ..Secret::default()
        }
    }

    const OURS: [(&str, &str); 2] = [
        (LABEL_MANAGED_BY, MANAGED_BY_VALUE),
        (LABEL_SPEC_NAME, "db"),
    ];

    fn controller(uid: &str, controller: Option<bool>) -> OwnerReference {
        OwnerReference {
            api_version: "secretspec.jabbas.eu/v1alpha1".to_owned(),
            kind: "SecretSpec".to_owned(),
            name: "db".to_owned(),
            uid: uid.to_owned(),
            controller,
            block_owner_deletion: Some(true),
        }
    }

    #[test]
    fn no_secret_is_absent() {
        assert_eq!(ownership(None, &spec(Some("u1"))), Ownership::Absent);
    }

    #[test]
    fn missing_labels_conflict() {
        let got = ownership(Some(&secret(&[], &[], None)), &spec(Some("u1")));
        assert_eq!(
            got,
            Ownership::Conflict(
                "Secret db exists and is not managed by this SecretSpec".to_owned()
            )
        );
    }

    #[test]
    fn other_managed_by_value_conflicts() {
        let s = secret(
            &[(LABEL_MANAGED_BY, "helm"), (LABEL_SPEC_NAME, "db")],
            &[],
            None,
        );
        assert!(matches!(
            ownership(Some(&s), &spec(Some("u1"))),
            Ownership::Conflict(_)
        ));
    }

    #[test]
    fn other_spec_name_conflicts() {
        let s = secret(
            &[
                (LABEL_MANAGED_BY, MANAGED_BY_VALUE),
                (LABEL_SPEC_NAME, "other"),
            ],
            &[],
            None,
        );
        assert!(matches!(
            ownership(Some(&s), &spec(Some("u1"))),
            Ownership::Conflict(_)
        ));
    }

    #[test]
    fn controller_ref_to_this_uid_is_owned() {
        let s = secret(&OURS, &[], Some(vec![controller("u1", Some(true))]));
        assert_eq!(ownership(Some(&s), &spec(Some("u1"))), Ownership::Owned);
    }

    #[test]
    fn controller_ref_to_other_uid_waits_for_gc() {
        let s = secret(
            &OURS,
            &[(ANNOTATION_SPEC_UID, "u1")],
            Some(vec![controller("old", Some(true))]),
        );
        assert_eq!(ownership(Some(&s), &spec(Some("u1"))), Ownership::WaitForGc);
    }

    #[test]
    fn non_controller_reference_is_ignored() {
        let s = secret(&OURS, &[], Some(vec![controller("old", None)]));
        assert_eq!(ownership(Some(&s), &spec(Some("u1"))), Ownership::Adoptable);
    }

    #[test]
    fn spec_uid_annotation_owns_a_retained_secret() {
        let s = secret(&OURS, &[(ANNOTATION_SPEC_UID, "u1")], None);
        assert_eq!(ownership(Some(&s), &spec(Some("u1"))), Ownership::Owned);
    }

    #[test]
    fn differing_or_absent_spec_uid_is_adoptable() {
        let differing = secret(&OURS, &[(ANNOTATION_SPEC_UID, "old")], None);
        assert_eq!(
            ownership(Some(&differing), &spec(Some("u1"))),
            Ownership::Adoptable
        );
        let absent = secret(&OURS, &[], None);
        assert_eq!(
            ownership(Some(&absent), &spec(Some("u1"))),
            Ownership::Adoptable
        );
    }

    #[test]
    fn spec_without_uid_is_never_owned() {
        let s = secret(
            &OURS,
            &[(ANNOTATION_SPEC_UID, "u1")],
            Some(vec![controller("u1", Some(true))]),
        );
        assert_eq!(ownership(Some(&s), &spec(None)), Ownership::Adoptable);
        let foreign = secret(&[], &[], None);
        assert!(matches!(
            ownership(Some(&foreign), &spec(None)),
            Ownership::Conflict(_)
        ));
    }

    #[test]
    fn delete_policy_yields_one_controller_reference() {
        let refs = owner_references(&spec(Some("u1")), DeletionPolicy::Delete).expect("refs");
        assert_eq!(refs, vec![controller("u1", Some(true))]);
        assert_eq!(refs[0].api_version, "secretspec.jabbas.eu/v1alpha1");
    }

    #[test]
    fn retain_policy_yields_none() {
        assert!(owner_references(&spec(Some("u1")), DeletionPolicy::Retain).is_none());
    }

    #[test]
    fn delete_policy_without_uid_yields_none() {
        assert!(owner_references(&spec(None), DeletionPolicy::Delete).is_none());
    }
}
