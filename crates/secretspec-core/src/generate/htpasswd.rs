// SPDX-License-Identifier: Apache-2.0

//! htpasswd line generation (bcrypt, `$2y$`).

use bcrypt::Version;
use rand_core::CryptoRngCore;
use secretspec_api::constants::BCRYPT_COST;

use super::GenerateError;
use crate::value::SecretValue;

/// Builds `<username>:<$2y$12$…>` (no trailing newline) for `password`.
///
/// The 16-byte salt is drawn from `rng`. The line is the HMAC key of the
/// htpasswd fingerprint, so its exact bytes matter.
///
/// # Errors
///
/// [`GenerateError::Rng`] if the RNG fails; [`GenerateError::Bcrypt`] if
/// hashing fails.
pub fn htpasswd_line<R: CryptoRngCore + ?Sized>(
    username: &str,
    password: &SecretValue,
    rng: &mut R,
) -> Result<SecretValue, GenerateError> {
    let mut salt = [0_u8; 16];
    rng.try_fill_bytes(&mut salt)
        .map_err(|_| GenerateError::Rng)?;
    let parts = bcrypt::hash_with_salt(password.expose(), BCRYPT_COST, salt)
        .map_err(|_| GenerateError::Bcrypt)?;
    let hash = parts.format_for_version(Version::TwoY);
    let mut line = Vec::with_capacity(username.len() + 1 + hash.len());
    line.extend_from_slice(username.as_bytes());
    line.push(b':');
    line.extend_from_slice(hash.as_bytes());
    Ok(SecretValue::new(line))
}

/// True if `line` is `<username>:<hash>` and the hash verifies `password`.
pub fn htpasswd_verifies(line: &[u8], username: &str, password: &SecretValue) -> bool {
    let Some(hash) = line
        .strip_prefix(username.as_bytes())
        .and_then(|rest| rest.strip_prefix(b":"))
    else {
        return false;
    };
    let Ok(hash) = std::str::from_utf8(hash) else {
        return false;
    };
    bcrypt::verify(password.expose(), hash).unwrap_or(false)
}
