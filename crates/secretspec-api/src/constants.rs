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
/// Upper bound of `length` and `bytes`.
pub const MAX_RANDOM_LENGTH: u16 = 1024;
