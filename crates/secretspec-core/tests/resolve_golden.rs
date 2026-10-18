// SPDX-License-Identifier: Apache-2.0

//! End-to-end golden tests: a minimal manifest goes through `resolve()` (which
//! applies the frozen defaults) and `expected_fingerprints()`, and must land on
//! the pinned golden vectors of `docs/fingerprints.md`.
//!
//! This catches a changed *default* as well as a changed canonical format:
//! either would rotate every credential created from such a manifest. The hex
//! literals were computed independently with `printf '<canonical>' | shasum -a
//! 256`. NEVER update them to make a test pass.
// Tests report through panics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use secretspec_api::resolve;
use secretspec_api::v1alpha1::SecretSpecSpec;
use secretspec_core::{Slot, expected_fingerprints};

fn fingerprints_of(yaml: &str) -> Vec<(String, String)> {
    let spec: SecretSpecSpec = serde_norway::from_str(yaml).expect("manifest deserializes");
    let resolved = resolve(&spec, "name", "namespace").unwrap_or_else(|e| panic!("{yaml}: {e}"));
    expected_fingerprints(&resolved)
        .into_iter()
        .map(|(slot, fingerprint)| (slot.to_string(), fingerprint.as_str().to_owned()))
        .collect()
}

fn pinned(slot: &Slot, hex: &str) -> Vec<(String, String)> {
    vec![(slot.to_string(), hex.to_owned())]
}

#[test]
fn default_field_matches_golden_vector_1() {
    assert_eq!(
        fingerprints_of("fields: [{name: password}]"),
        pinned(
            &Slot::Field("password".to_owned()),
            "6819313d1058c21d17f2c11aa042b85e5b39f5b1bb9f505d9eeb90f013bb9266"
        )
    );
}

#[test]
fn default_ssh_key_pair_matches_golden_vector_6() {
    assert_eq!(
        fingerprints_of("sshKeyPair: {}"),
        pinned(
            &Slot::SshKeyPair,
            "b62ed2e39a6976cc6626f17f758b49eccf548c2132f73d98ca25c2aa1f89f746"
        )
    );
}

#[test]
fn default_rsa_key_pair_matches_golden_vector_7() {
    assert_eq!(
        fingerprints_of("sshKeyPair: {algorithm: rsa}"),
        pinned(
            &Slot::SshKeyPair,
            "33454c99221f4015946a46dd2760c404084c058d7a4544047aed390bfe099940"
        )
    );
}

#[test]
fn default_basic_auth_password_matches_golden_vector_10() {
    assert_eq!(
        fingerprints_of("basicAuth: {username: admin}"),
        pinned(
            &Slot::BasicAuthPassword,
            "0e8fee5d13ec3fe16a5615c6b3c29a303047c378d6558fc859ef3c7234b7f4c4"
        )
    );
}
