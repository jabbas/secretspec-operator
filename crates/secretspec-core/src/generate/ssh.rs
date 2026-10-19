// SPDX-License-Identifier: Apache-2.0

//! SSH key pair generation.
//!
//! Private keys are unencrypted OpenSSH PEM; public keys are one
//! `authorized_keys` line. Error messages name the failed step only and never
//! include key material.

use rand_core::CryptoRngCore;
use secretspec_api::resolve::SshAlgorithmParams;
use secretspec_api::v1alpha1::EcdsaCurve;
use ssh_key::LineEnding;
use ssh_key::PrivateKey;
use ssh_key::private::{EcdsaKeypair, Ed25519Keypair, KeypairData, RsaKeypair};

use super::GenerateError;
use crate::value::SecretValue;

/// A generated SSH key pair.
pub struct SshKeyPair {
    /// OpenSSH PEM private key (unencrypted).
    pub private_key: SecretValue,
    /// `authorized_keys` line, terminated by a newline.
    pub public_key: String,
}

fn ssh_error(step: &str) -> GenerateError {
    GenerateError::Ssh(step.to_owned())
}

fn ssh_curve(curve: EcdsaCurve) -> ssh_key::EcdsaCurve {
    match curve {
        EcdsaCurve::P256 => ssh_key::EcdsaCurve::NistP256,
        EcdsaCurve::P384 => ssh_key::EcdsaCurve::NistP384,
        EcdsaCurve::P521 => ssh_key::EcdsaCurve::NistP521,
    }
}

/// Generates a key pair for `alg`, with `comment` on both halves.
///
/// # Errors
///
/// [`GenerateError::Ssh`] if the key cannot be generated or encoded (for
/// example an unsupported RSA size).
pub fn generate_ssh<R: CryptoRngCore + ?Sized>(
    alg: SshAlgorithmParams,
    comment: &str,
    rng: &mut R,
) -> Result<SshKeyPair, GenerateError> {
    // `&mut R` is itself a `CryptoRngCore`, which lets the unsized `R` reach
    // ssh-key's `&mut impl CryptoRngCore` parameters.
    let mut rng = rng;
    let key_data = match alg {
        SshAlgorithmParams::Ed25519 => KeypairData::from(Ed25519Keypair::random(&mut rng)),
        SshAlgorithmParams::Rsa { bits } => RsaKeypair::random(&mut rng, usize::from(bits))
            .map(KeypairData::from)
            .map_err(|_| ssh_error("rsa key generation failed"))?,
        SshAlgorithmParams::Ecdsa { curve } => EcdsaKeypair::random(&mut rng, ssh_curve(curve))
            .map(KeypairData::from)
            .map_err(|_| ssh_error("ecdsa key generation failed"))?,
    };
    let key = PrivateKey::new(key_data, comment).map_err(|_| ssh_error("key assembly failed"))?;
    let pem = key
        .to_openssh(LineEnding::LF)
        .map_err(|_| ssh_error("private key encoding failed"))?;
    Ok(SshKeyPair {
        private_key: SecretValue::new(pem.as_bytes().to_vec()),
        public_key: authorized_keys_line(&key)?,
    })
}

fn authorized_keys_line(key: &PrivateKey) -> Result<String, GenerateError> {
    let mut line = key
        .public_key()
        .to_openssh()
        .map_err(|_| ssh_error("public key encoding failed"))?;
    line.push('\n');
    Ok(line)
}

fn parse_private(private_key: &[u8]) -> Result<PrivateKey, GenerateError> {
    PrivateKey::from_openssh(private_key).map_err(|_| ssh_error("private key does not parse"))
}

/// Re-derives the public `authorized_keys` line (newline-terminated) from an
/// existing private key, with `comment`.
///
/// # Errors
///
/// [`GenerateError::Ssh`] if `private_key` is not a parseable OpenSSH private
/// key.
pub fn public_key_line(private_key: &[u8], comment: &str) -> Result<String, GenerateError> {
    let mut key = parse_private(private_key)?;
    key.set_comment(comment);
    authorized_keys_line(&key)
}

/// Bit length of an RSA modulus.
fn modulus_bits(n: &ssh_key::Mpint) -> Option<usize> {
    let (first, rest) = n.as_positive_bytes()?.split_first()?;
    let used = 8 - u8::try_from(first.leading_zeros()).ok()?;
    Some(rest.len() * 8 + usize::from(used))
}

/// True if `private_key` parses and its algorithm and size equal `alg`.
pub fn private_key_matches(private_key: &[u8], alg: SshAlgorithmParams) -> bool {
    let Ok(key) = parse_private(private_key) else {
        return false;
    };
    match (key.key_data(), alg) {
        (KeypairData::Ed25519(_), SshAlgorithmParams::Ed25519) => true,
        (KeypairData::Rsa(pair), SshAlgorithmParams::Rsa { bits }) => {
            modulus_bits(&pair.public.n) == Some(usize::from(bits))
        }
        (KeypairData::Ecdsa(pair), SshAlgorithmParams::Ecdsa { curve }) => {
            pair.curve() == ssh_curve(curve)
        }
        _ => false,
    }
}
