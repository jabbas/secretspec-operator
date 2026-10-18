// SPDX-License-Identifier: Apache-2.0

//! Helpers shared by the integration tests.

#![allow(dead_code)]

use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRngCore, SeedableRng};

/// A deterministic cryptographic RNG for reproducible tests.
pub fn seeded_rng(seed: u64) -> impl CryptoRngCore {
    ChaCha20Rng::seed_from_u64(seed)
}
