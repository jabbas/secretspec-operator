// SPDX-License-Identifier: Apache-2.0

//! Random bytes in a text encoding.

use data_encoding::{BASE32, BASE64, BASE64URL, HEXLOWER};
use rand_core::CryptoRngCore;
use secretspec_api::resolve::BytesParams;
use secretspec_api::v1alpha1::Encoding;
use zeroize::Zeroizing;

use super::GenerateError;
use crate::value::SecretValue;

/// Draws `count` random bytes, encodes them, and wraps the result in
/// `prefix` and `suffix`.
///
/// # Errors
///
/// Currently infallible; the `Result` keeps the signature uniform with the
/// other generators.
pub fn generate_bytes<R: CryptoRngCore + ?Sized>(
    p: &BytesParams,
    rng: &mut R,
) -> Result<SecretValue, GenerateError> {
    let mut raw = Zeroizing::new(vec![0u8; usize::from(p.count)]);
    rng.fill_bytes(&mut raw);

    let encoded = Zeroizing::new(match p.encoding {
        Encoding::Hex => HEXLOWER.encode(&raw),
        Encoding::Base64 => BASE64.encode(&raw),
        Encoding::Base64url => BASE64URL.encode(&raw),
        Encoding::Base32 => BASE32.encode(&raw),
    });

    let mut out = Vec::with_capacity(p.prefix.len() + encoded.len() + p.suffix.len());
    out.extend_from_slice(p.prefix.as_bytes());
    out.extend_from_slice(encoded.as_bytes());
    out.extend_from_slice(p.suffix.as_bytes());
    Ok(SecretValue::new(out))
}
