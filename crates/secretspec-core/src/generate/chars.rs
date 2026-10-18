// SPDX-License-Identifier: Apache-2.0

//! Random characters from an alphabet with per-class minimums.

use rand_core::CryptoRngCore;
use secretspec_api::resolve::CharsParams;
use zeroize::Zeroizing;

use super::GenerateError;
use crate::value::SecretValue;

/// Largest `n` accepted by [`uniform_below`] (two bytes of entropy).
const MAX_INDEX_RANGE: usize = 65_536;

/// A character class: name, required count, membership test.
type Class = (&'static str, u16, fn(&u8) -> bool);

/// Uniform integer in `0..n` (`1 <= n <= 65_536`) by rejection sampling.
///
/// Draws one byte when `n <= 256`, otherwise two (little-endian). A draw `x`
/// from `0..range` (`range` = 256 or 65 536) is rejected when
/// `x >= n * floor(range / n)`; otherwise the result is `x % n`. The accepted
/// region is a whole number of copies of `0..n`, so every outcome is equally
/// likely — plain `x % n` would favor small values.
fn uniform_below<R: CryptoRngCore + ?Sized>(rng: &mut R, n: usize) -> usize {
    debug_assert!((1..=MAX_INDEX_RANGE).contains(&n));
    // `n <= 65_536`, so the conversion cannot fail.
    let n = u32::try_from(n).unwrap_or(65_536);
    if n == 1 {
        return 0;
    }
    let wide = n > 256;
    let range: u32 = if wide { 65_536 } else { 256 };
    let zone = n * (range / n);
    loop {
        let mut buf = [0u8; 2];
        let x = if wide {
            rng.fill_bytes(&mut buf);
            u32::from(u16::from_le_bytes(buf))
        } else {
            rng.fill_bytes(&mut buf[..1]);
            u32::from(buf[0])
        };
        if x < zone {
            return (x % n) as usize;
        }
    }
}

fn pick<R: CryptoRngCore + ?Sized>(rng: &mut R, set: &[u8]) -> u8 {
    set[uniform_below(rng, set.len())]
}

/// Generates a random string from `p.alphabet`.
///
/// Each class with a non-zero `require` count contributes that many
/// characters drawn from `alphabet ∩ class`; the remaining
/// `length − Σrequire` characters come from the whole alphabet. The body is
/// then Fisher–Yates shuffled with the same RNG, and wrapped in
/// `prefix`/`suffix`.
///
/// Precondition: `p.alphabet` is sorted and contains each byte at most once
/// (as produced by the resolver). Duplicates would silently weight those
/// characters; the generator does not re-check this.
///
/// # Errors
///
/// [`GenerateError::AlphabetTooSmall`] if the alphabet has fewer than two
/// characters; [`GenerateError::AlphabetTooLarge`] if it has more entries
/// than the index sampler supports (65 536);
/// [`GenerateError::RequireExceedsLength`] if the per-class minimums add up
/// to more than `length` (the output is never lengthened or clamped);
/// [`GenerateError::EmptyClass`] if a required class has no member in the
/// alphabet.
pub fn generate_chars<R: CryptoRngCore + ?Sized>(
    p: &CharsParams,
    rng: &mut R,
) -> Result<SecretValue, GenerateError> {
    if p.alphabet.len() < 2 {
        return Err(GenerateError::AlphabetTooSmall);
    }
    if p.alphabet.len() > MAX_INDEX_RANGE {
        return Err(GenerateError::AlphabetTooLarge);
    }
    let required = u32::from(p.require.lower)
        + u32::from(p.require.upper)
        + u32::from(p.require.digits)
        + u32::from(p.require.symbols);
    if required > u32::from(p.length) {
        return Err(GenerateError::RequireExceedsLength);
    }

    let classes: [Class; 4] = [
        ("lower", p.require.lower, u8::is_ascii_lowercase),
        ("upper", p.require.upper, u8::is_ascii_uppercase),
        ("digits", p.require.digits, u8::is_ascii_digit),
        ("symbols", p.require.symbols, u8::is_ascii_punctuation),
    ];

    let mut body = Zeroizing::new(Vec::with_capacity(usize::from(p.length)));
    for (name, count, in_class) in classes {
        if count == 0 {
            continue;
        }
        let subset: Vec<u8> = p.alphabet.iter().copied().filter(in_class).collect();
        if subset.is_empty() {
            return Err(GenerateError::EmptyClass(name));
        }
        for _ in 0..count {
            body.push(pick(rng, &subset));
        }
    }

    while body.len() < usize::from(p.length) {
        body.push(pick(rng, &p.alphabet));
    }

    // Fisher–Yates: position `i` swaps with a uniform index in `0..=i`.
    for i in (1..body.len()).rev() {
        let j = uniform_below(rng, i + 1);
        body.swap(i, j);
    }

    let mut out = Vec::with_capacity(p.prefix.len() + body.len() + p.suffix.len());
    out.extend_from_slice(p.prefix.as_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(p.suffix.as_bytes());
    Ok(SecretValue::new(out))
}
