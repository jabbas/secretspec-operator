// SPDX-License-Identifier: Apache-2.0

//! A read-only view of an existing managed Secret.

use std::collections::BTreeMap;
use std::str::FromStr;

use jiff::Timestamp;
use k8s_openapi::api::core::v1::Secret;
use secretspec_api::constants::{
    ANNOTATION_FINGERPRINTS, ANNOTATION_GENERATED_AT, ANNOTATION_KEYS,
};

use crate::fingerprint::{FingerprintMap, ParsedAnnotation};
use crate::plan::keys::KeyMap;

/// The Secret type assumed when the field is empty.
const DEFAULT_SECRET_TYPE: &str = "Opaque";

/// The rotation anchor as recorded on the Secret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// The `generated-at` annotation is missing.
    Absent,
    /// The annotation is present but not a valid RFC 3339 timestamp.
    Invalid,
    /// The time of the last full generation.
    At(Timestamp),
}

/// What the planner knows about the Secret that currently exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    /// Secret data with empty values removed (an empty value counts as missing).
    pub data: BTreeMap<String, Vec<u8>>,
    /// The fingerprints annotation; `None` when it is absent or not a JSON object.
    pub fingerprints: Option<ParsedAnnotation>,
    /// The role -> key map from the keys annotation (tolerant).
    pub keys: KeyMap,
    /// The rotation anchor.
    pub generated_at: Anchor,
    /// The Secret `type`; empty is read as `Opaque`.
    pub secret_type: String,
}

/// Reads `secret` into an [`Inventory`]. Never fails: unreadable annotations
/// degrade to "nothing recorded".
#[must_use]
pub fn inventory(secret: &Secret) -> Inventory {
    let data = secret
        .data
        .iter()
        .flatten()
        .filter(|(_, value)| !value.0.is_empty())
        .map(|(key, value)| (key.clone(), value.0.clone()))
        .collect();
    let annotations = secret.metadata.annotations.as_ref();
    let annotation = |name: &str| {
        annotations
            .and_then(|map| map.get(name))
            .map(String::as_str)
    };

    let fingerprints = annotation(ANNOTATION_FINGERPRINTS)
        .and_then(|text| FingerprintMap::from_annotation(text).ok());
    let keys = annotation(ANNOTATION_KEYS)
        .map(KeyMap::from_annotation)
        .unwrap_or_default();
    let generated_at = annotation(ANNOTATION_GENERATED_AT).map_or(Anchor::Absent, |text| {
        Timestamp::from_str(text).map_or(Anchor::Invalid, Anchor::At)
    });
    let secret_type = match secret.type_.as_deref() {
        None | Some("") => DEFAULT_SECRET_TYPE.to_owned(),
        Some(other) => other.to_owned(),
    };

    Inventory {
        data,
        fingerprints,
        keys,
        generated_at,
        secret_type,
    }
}

#[cfg(test)]
mod tests {
    // Tests report through panics.
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use k8s_openapi::ByteString;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;

    use super::*;
    use crate::plan::keys::Role;

    const FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn secret(annotations: &[(&str, &str)], data: &[(&str, &[u8])]) -> Secret {
        Secret {
            metadata: ObjectMeta {
                annotations: Some(
                    annotations
                        .iter()
                        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                        .collect(),
                ),
                ..ObjectMeta::default()
            },
            data: Some(
                data.iter()
                    .map(|(k, v)| ((*k).to_owned(), ByteString(v.to_vec())))
                    .collect(),
            ),
            ..Secret::default()
        }
    }

    #[test]
    fn drops_empty_values() {
        let inv = inventory(&secret(&[], &[("a", b"x"), ("b", b"")]));
        assert_eq!(inv.data.len(), 1);
        assert_eq!(inv.data["a"], b"x");
    }

    #[test]
    fn bare_secret_has_nothing_recorded() {
        let inv = inventory(&Secret::default());
        assert!(inv.data.is_empty());
        assert!(inv.fingerprints.is_none());
        assert_eq!(inv.keys, KeyMap::default());
        assert_eq!(inv.generated_at, Anchor::Absent);
        assert_eq!(inv.secret_type, "Opaque");
    }

    #[test]
    fn parses_all_annotations() {
        let fingerprints = format!(r#"{{"basic-auth/password":"{FP}"}}"#);
        let inv = inventory(&secret(
            &[
                (ANNOTATION_FINGERPRINTS, &fingerprints),
                (ANNOTATION_KEYS, r#"{"basic-auth/username":"user"}"#),
                (ANNOTATION_GENERATED_AT, "2026-10-19T12:00:00Z"),
            ],
            &[],
        ));
        let parsed = inv.fingerprints.expect("object annotation");
        assert_eq!(parsed.map.slots.len(), 1);
        assert_eq!(
            inv.keys.0.get(&Role::BasicAuthUsername).map(String::as_str),
            Some("user")
        );
        assert_eq!(
            inv.generated_at,
            Anchor::At("2026-10-19T12:00:00Z".parse().unwrap())
        );
    }

    #[test]
    fn garbage_annotations_degrade() {
        let inv = inventory(&secret(
            &[
                (ANNOTATION_FINGERPRINTS, "not json"),
                (ANNOTATION_KEYS, "[1,2]"),
                (ANNOTATION_GENERATED_AT, "yesterday"),
            ],
            &[],
        ));
        assert!(inv.fingerprints.is_none());
        assert_eq!(inv.keys, KeyMap::default());
        assert_eq!(inv.generated_at, Anchor::Invalid);
    }

    #[test]
    fn non_object_fingerprints_json_is_none() {
        let inv = inventory(&secret(&[(ANNOTATION_FINGERPRINTS, "[]")], &[]));
        assert!(inv.fingerprints.is_none());
    }

    #[test]
    fn generated_at_accepts_fractions_and_offsets() {
        for text in [
            "2026-10-19T12:00:00.123456789Z",
            "2026-10-19T14:00:00+02:00",
        ] {
            let inv = inventory(&secret(&[(ANNOTATION_GENERATED_AT, text)], &[]));
            assert!(matches!(inv.generated_at, Anchor::At(_)), "{text}");
        }
        let offset = inventory(&secret(
            &[(ANNOTATION_GENERATED_AT, "2026-10-19T14:00:00+02:00")],
            &[],
        ));
        assert_eq!(
            offset.generated_at,
            Anchor::At("2026-10-19T12:00:00Z".parse().unwrap())
        );
    }

    #[test]
    fn secret_type_defaults_and_passes_through() {
        let mut s = Secret {
            type_: Some(String::new()),
            ..Secret::default()
        };
        assert_eq!(inventory(&s).secret_type, "Opaque");
        s.type_ = Some("kubernetes.io/basic-auth".to_owned());
        assert_eq!(inventory(&s).secret_type, "kubernetes.io/basic-auth");
    }
}
