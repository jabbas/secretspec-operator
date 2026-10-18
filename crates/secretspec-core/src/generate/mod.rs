// SPDX-License-Identifier: Apache-2.0

//! Credential generators.
//!
//! Every generator takes the random source as `rng: &mut R` with
//! `R: CryptoRng`, the cryptographic RNG trait of `rand_core` 0.10 (the
//! version `ssh-key` 0.7 is built on). No generator touches a global RNG;
//! production code passes [`os_rng`], tests pass a seeded RNG.

mod bytes;
mod chars;

pub use bytes::generate_bytes;
pub use chars::generate_chars;

use getrandom::SysRng;
use rand_core::UnwrapErr;

/// Why a value could not be generated.
///
/// Messages never contain generated or configured secret material.
#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    /// Fewer than two distinct characters to choose from.
    #[error("alphabet has fewer than 2 characters")]
    AlphabetTooSmall,
    /// A required class has no member in the alphabet.
    #[error("no {0} characters in the alphabet")]
    EmptyClass(&'static str),
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

/// The operating-system CSPRNG, for production use.
///
/// The OS RNG failing is unrecoverable; the returned adapter panics rather
/// than ever emitting weak randomness.
pub fn os_rng() -> UnwrapErr<SysRng> {
    UnwrapErr(SysRng)
}
