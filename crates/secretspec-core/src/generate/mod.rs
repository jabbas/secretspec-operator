// SPDX-License-Identifier: Apache-2.0

//! Credential generators.
//!
//! Every generator takes the random source as `rng: &mut R` with
//! `R: CryptoRngCore`, the cryptographic RNG trait of `rand_core` 0.6 (the
//! version the stable `ssh-key` 0.6 series is built on). No generator touches
//! a global RNG; production code passes [`os_seeded_rng`], tests pass a
//! seeded RNG.

mod bytes;
mod chars;

pub use bytes::generate_bytes;
pub use chars::generate_chars;

use rand_chacha::ChaCha20Rng;
use rand_core::{OsRng, RngCore, SeedableRng};
use zeroize::Zeroizing;

/// Why a value could not be generated.
///
/// Messages never contain generated or configured secret material.
#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    /// Fewer than two distinct characters to choose from.
    #[error("alphabet has fewer than 2 characters")]
    AlphabetTooSmall,
    /// The alphabet is larger than the index sampler supports.
    #[error("alphabet is too large")]
    AlphabetTooLarge,
    /// A required class has no member in the alphabet.
    #[error("no {0} characters in the alphabet")]
    EmptyClass(&'static str),
    /// The per-class minimums add up to more than the requested length.
    #[error("required character counts exceed length")]
    RequireExceedsLength,
    /// The random number generator failed.
    #[error("random number generator failed")]
    Rng,
    /// SSH key generation or encoding failed.
    #[error("ssh key generation failed: {0}")]
    Ssh(String),
    /// bcrypt hashing failed.
    #[error("bcrypt failed")]
    Bcrypt,
}

/// An OS-seeded `ChaCha20` CSPRNG, for production use.
///
/// The 256-bit seed is read from the operating system's entropy source
/// (`getrandom`) and wiped after use; all further output comes from `ChaCha20`.
/// Create one per reconcile rather than sharing it across tasks.
///
/// # Errors
///
/// [`GenerateError::Rng`] if the operating system cannot supply entropy.
pub fn os_seeded_rng() -> Result<ChaCha20Rng, GenerateError> {
    let mut seed = Zeroizing::new(<ChaCha20Rng as SeedableRng>::Seed::default());
    OsRng
        .try_fill_bytes(&mut *seed)
        .map_err(|_| GenerateError::Rng)?;
    Ok(ChaCha20Rng::from_seed(*seed))
}
