// SPDX-License-Identifier: Apache-2.0

//! Behavior of the character and byte generators.

mod support;

use rand_core::RngCore;
use secretspec_api::resolve::{BytesParams, CharsParams, Require};
use secretspec_api::v1alpha1::Encoding;
use secretspec_core::generate::{GenerateError, generate_bytes, generate_chars, os_seeded_rng};
use support::seeded_rng;

fn alnum() -> Vec<u8> {
    let mut a: Vec<u8> = (b'0'..=b'9')
        .chain(b'A'..=b'Z')
        .chain(b'a'..=b'z')
        .collect();
    a.sort_unstable();
    a
}

fn chars(length: u16, alphabet: Vec<u8>, require: Require) -> CharsParams {
    CharsParams {
        length,
        alphabet,
        require,
        prefix: String::new(),
        suffix: String::new(),
    }
}

fn bytes(encoding: Encoding) -> BytesParams {
    BytesParams {
        count: 32,
        encoding,
        prefix: String::new(),
        suffix: String::new(),
    }
}

#[test]
fn chars_length_and_alphabet() {
    let alphabet = alnum();
    let p = chars(32, alphabet.clone(), Require::default());
    let v = generate_chars(&p, &mut seeded_rng(1)).unwrap();
    assert_eq!(v.expose().len(), 32);
    assert!(v.expose().iter().all(|b| alphabet.contains(b)));

    let p = CharsParams {
        prefix: "GK".into(),
        suffix: "-x".into(),
        ..p
    };
    let v = generate_chars(&p, &mut seeded_rng(1)).unwrap();
    assert_eq!(v.expose().len(), 32 + 4);
    assert!(v.expose().starts_with(b"GK"));
    assert!(v.expose().ends_with(b"-x"));
}

fn v12_alphabet() -> Vec<u8> {
    (0x21u8..=0x7e)
        .filter(|b| !matches!(b, b'/' | b'@' | b'"'))
        .collect()
}

fn count(v: &[u8], f: impl Fn(&u8) -> bool) -> usize {
    v.iter().filter(|b| f(b)).count()
}

#[test]
fn chars_require_counts_hold() {
    let p = chars(
        32,
        v12_alphabet(),
        Require {
            lower: 1,
            upper: 1,
            digits: 1,
            symbols: 1,
        },
    );
    for seed in 0..1000 {
        let v = generate_chars(&p, &mut seeded_rng(seed)).unwrap();
        let v = v.expose();
        assert_eq!(v.len(), 32);
        assert!(count(v, u8::is_ascii_lowercase) >= 1);
        assert!(count(v, u8::is_ascii_uppercase) >= 1);
        assert!(count(v, u8::is_ascii_digit) >= 1);
        assert!(count(v, u8::is_ascii_punctuation) >= 1);
        assert!(!v.iter().any(|b| matches!(b, b'/' | b'@' | b'"')));
    }

    let p = chars(
        32,
        v12_alphabet(),
        Require {
            lower: 8,
            upper: 8,
            digits: 8,
            symbols: 8,
        },
    );
    for seed in 0..100 {
        let v = generate_chars(&p, &mut seeded_rng(seed)).unwrap();
        let v = v.expose();
        assert_eq!(v.len(), 32);
        assert_eq!(count(v, u8::is_ascii_lowercase), 8);
        assert_eq!(count(v, u8::is_ascii_uppercase), 8);
        assert_eq!(count(v, u8::is_ascii_digit), 8);
        assert_eq!(count(v, u8::is_ascii_punctuation), 8);
    }
}

#[test]
fn chars_uniformity_smoke() {
    let p = chars(1, b"ab".to_vec(), Require::default());
    let mut rng = seeded_rng(42);
    let a = (0..10_000)
        .filter(|_| generate_chars(&p, &mut rng).unwrap().expose() == b"a")
        .count();
    assert!((4_900..=5_100).contains(&a), "a drawn {a} times");
}

#[test]
fn chars_uniformity_non_power_of_two() {
    // 3 symbols: a naive `% 3` on a byte would favor 'a' (86 vs 85 of 256);
    // rejection sampling must not. Position shuffling must not bias either.
    let p = chars(3, b"abc".to_vec(), Require::default());
    let mut rng = seeded_rng(7);
    let mut counts = [0usize; 3];
    for _ in 0..10_000 {
        let v = generate_chars(&p, &mut rng).unwrap();
        for b in v.expose() {
            counts[usize::from(b - b'a')] += 1;
        }
    }
    for c in counts {
        assert!((9_600..=10_400).contains(&c), "counts {counts:?}");
    }
}

#[test]
fn chars_shuffle_uniform_over_long_bodies() {
    // length 300 > 256 exercises the two-byte index path. Digit positions
    // must fall in each third of the string about equally often.
    let alphabet: Vec<u8> = (b'a'..=b'z').chain(b'0'..=b'9').collect();
    let p = chars(
        300,
        alphabet,
        Require {
            digits: 1,
            ..Require::default()
        },
    );
    let mut rng = seeded_rng(11);
    let mut thirds = [0usize; 3];
    let trials = 9_000;
    for _ in 0..trials {
        let v = generate_chars(&p, &mut rng).unwrap();
        // Digits also come from the unconstrained remainder, so a body may
        // hold several; all of them are exchangeable, so count every digit
        // position.
        let digits: Vec<usize> = v
            .expose()
            .iter()
            .enumerate()
            .filter(|(_, b)| b.is_ascii_digit())
            .map(|(i, _)| i)
            .collect();
        for i in digits {
            thirds[i / 100] += 1;
        }
    }
    let total: usize = thirds.iter().sum();
    for t in thirds {
        // Each third holds 1/3 of positions; allow 5% relative slack.
        let expected = total / 3;
        assert!(
            t.abs_diff(expected) * 20 < expected,
            "thirds {thirds:?} of {total}"
        );
    }
}

#[test]
fn chars_errors() {
    let p = chars(8, b"a".to_vec(), Require::default());
    assert!(matches!(
        generate_chars(&p, &mut seeded_rng(0)),
        Err(GenerateError::AlphabetTooSmall)
    ));

    let hex = b"0123456789abcdef".to_vec();
    let p = chars(
        8,
        hex,
        Require {
            upper: 1,
            ..Require::default()
        },
    );
    assert!(matches!(
        generate_chars(&p, &mut seeded_rng(0)),
        Err(GenerateError::EmptyClass("upper"))
    ));
}

#[test]
fn chars_require_exceeds_length_is_error() {
    let p = chars(
        10,
        v12_alphabet(),
        Require {
            lower: 4,
            upper: 4,
            digits: 2,
            symbols: 1,
        },
    );
    assert!(matches!(
        generate_chars(&p, &mut seeded_rng(0)),
        Err(GenerateError::RequireExceedsLength)
    ));

    // Exactly filling the length is fine.
    let p = chars(
        4,
        v12_alphabet(),
        Require {
            lower: 1,
            upper: 1,
            digits: 1,
            symbols: 1,
        },
    );
    assert_eq!(
        generate_chars(&p, &mut seeded_rng(0))
            .unwrap()
            .expose()
            .len(),
        4
    );

    // Sums that would overflow u16 must not wrap around.
    let p = chars(
        10,
        v12_alphabet(),
        Require {
            lower: u16::MAX,
            upper: u16::MAX,
            digits: 0,
            symbols: 0,
        },
    );
    assert!(matches!(
        generate_chars(&p, &mut seeded_rng(0)),
        Err(GenerateError::RequireExceedsLength)
    ));
}

#[test]
fn chars_oversized_alphabet_is_error() {
    let p = chars(8, vec![b'a'; 65_537], Require::default());
    assert!(matches!(
        generate_chars(&p, &mut seeded_rng(0)),
        Err(GenerateError::AlphabetTooLarge)
    ));
}

#[test]
fn os_seeded_rng_works() {
    let mut a = os_seeded_rng().unwrap();
    let mut b = os_seeded_rng().unwrap();
    assert_ne!(a.next_u64(), b.next_u64());

    let p = chars(32, alnum(), Require::default());
    let v = generate_chars(&p, &mut a).unwrap();
    assert_eq!(v.expose().len(), 32);
}

#[test]
fn chars_deterministic_with_seed() {
    let p = chars(32, alnum(), Require::default());
    let a = generate_chars(&p, &mut seeded_rng(5)).unwrap();
    let b = generate_chars(&p, &mut seeded_rng(5)).unwrap();
    let c = generate_chars(&p, &mut seeded_rng(6)).unwrap();
    assert_eq!(a.expose(), b.expose());
    assert_ne!(a.expose(), c.expose());
}

#[test]
fn bytes_encodings() {
    let rng = &mut seeded_rng(3);

    let v = generate_bytes(&bytes(Encoding::Hex), rng).unwrap();
    assert_eq!(v.expose().len(), 64);
    assert!(
        v.expose()
            .iter()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    );

    let v = generate_bytes(&bytes(Encoding::Base64), rng).unwrap();
    assert_eq!(v.expose().len(), 44);
    assert!(v.expose().ends_with(b"="));

    let v = generate_bytes(&bytes(Encoding::Base64url), rng).unwrap();
    assert_eq!(v.expose().len(), 44);
    assert!(!v.expose().iter().any(|b| matches!(b, b'+' | b'/')));

    let v = generate_bytes(&bytes(Encoding::Base32), rng).unwrap();
    assert_eq!(v.expose().len(), 56);
    assert!(
        v.expose()
            .iter()
            .all(|b| matches!(b, b'A'..=b'Z' | b'2'..=b'7' | b'='))
    );
}

#[test]
fn bytes_prefix_suffix_and_roundtrip() {
    let p = BytesParams {
        prefix: "pre-".into(),
        suffix: "-suf".into(),
        ..bytes(Encoding::Base64)
    };
    let v = generate_bytes(&p, &mut seeded_rng(9)).unwrap();
    let s = v.expose();
    assert!(s.starts_with(b"pre-") && s.ends_with(b"-suf"));
    let body = &s[4..s.len() - 4];
    let decoded = data_encoding::BASE64.decode(body).unwrap();
    assert_eq!(decoded.len(), 32);

    let hex = BytesParams {
        prefix: "p".into(),
        suffix: "s".into(),
        ..bytes(Encoding::Hex)
    };
    let v = generate_bytes(&hex, &mut seeded_rng(9)).unwrap();
    let body = &v.expose()[1..v.expose().len() - 1];
    assert_eq!(data_encoding::HEXLOWER.decode(body).unwrap().len(), 32);
}

#[test]
fn generated_values_redact() {
    let c = generate_chars(&chars(8, alnum(), Require::default()), &mut seeded_rng(1)).unwrap();
    assert_eq!(format!("{c:?}"), "<redacted>");
    let b = generate_bytes(&bytes(Encoding::Hex), &mut seeded_rng(1)).unwrap();
    assert_eq!(format!("{b:?}"), "<redacted>");
}
