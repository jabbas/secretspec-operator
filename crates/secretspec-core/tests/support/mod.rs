// SPDX-License-Identifier: Apache-2.0

//! Helpers shared by the integration tests.

#![allow(dead_code)]
// Test helpers report through panics.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;

use jiff::Timestamp;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRngCore, SeedableRng};
use secretspec_api::v1alpha1::{SecretSpec, SecretSpecSpec};

/// A deterministic cryptographic RNG for reproducible tests.
pub fn seeded_rng(seed: u64) -> impl CryptoRngCore {
    ChaCha20Rng::seed_from_u64(seed)
}

/// Name of the `SecretSpec` built by [`secret_spec`].
pub const SPEC_NAME: &str = "db";
/// Namespace of the `SecretSpec` built by [`secret_spec`].
pub const SPEC_NAMESPACE: &str = "apps";
/// UID of the `SecretSpec` built by [`secret_spec`].
pub const SPEC_UID: &str = "11111111-2222-3333-4444-555555555555";

/// A `SecretSpec` named [`SPEC_NAME`] in [`SPEC_NAMESPACE`] with UID
/// [`SPEC_UID`] and generation 1, whose `spec` is the given YAML.
///
/// # Panics
/// The YAML is not a valid `SecretSpecSpec`.
pub fn secret_spec(yaml: &str) -> SecretSpec {
    let body: SecretSpecSpec = serde_norway::from_str(yaml).expect("manifest deserializes");
    let mut spec = SecretSpec::new(SPEC_NAME, body);
    spec.metadata.namespace = Some(SPEC_NAMESPACE.to_owned());
    spec.metadata.uid = Some(SPEC_UID.to_owned());
    spec.metadata.generation = Some(1);
    spec
}

/// The fixed "now" of the planner tests.
///
/// # Panics
/// Never: the literal is a valid timestamp.
pub fn now() -> Timestamp {
    "2026-10-19T12:00:00Z".parse().expect("timestamp")
}

/// A string map from pairs.
pub fn string_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// A Secret named [`SPEC_NAME`] with the given labels and string data.
pub fn foreign_secret(labels: &[(&str, &str)], data: &[(&str, &str)]) -> Secret {
    Secret {
        metadata: ObjectMeta {
            name: Some(SPEC_NAME.to_owned()),
            namespace: Some(SPEC_NAMESPACE.to_owned()),
            labels: Some(string_map(labels)),
            ..ObjectMeta::default()
        },
        data: Some(
            data.iter()
                .map(|(k, v)| {
                    (
                        (*k).to_owned(),
                        k8s_openapi::ByteString(v.as_bytes().to_vec()),
                    )
                })
                .collect(),
        ),
        ..Secret::default()
    }
}
