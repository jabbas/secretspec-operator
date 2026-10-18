// SPDX-License-Identifier: Apache-2.0

//! Pure, cluster-independent logic for the operator.

pub mod fingerprint;
pub mod generate;
pub mod plan;
pub mod value;

pub use fingerprint::{
    Fingerprint, FingerprintMap, FingerprintParseError, ParsedAnnotation, Slot, canonical_bytes,
    canonical_chars, canonical_htpasswd, canonical_ssh, expected_fingerprints, fingerprint_of,
};
pub use value::SecretValue;
