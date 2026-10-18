// SPDX-License-Identifier: Apache-2.0

//! A secret byte string that never leaks through formatting.

use std::fmt;

use zeroize::Zeroizing;

/// Secret bytes: zeroed on drop, redacted in `Debug` and `Display`.
///
/// The only way to read the bytes is the explicit [`SecretValue::expose`].
pub struct SecretValue(Zeroizing<Vec<u8>>);

impl SecretValue {
    /// Wraps `bytes`; they are zeroed when the value is dropped.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The secret bytes. Do not log or format the result.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}
