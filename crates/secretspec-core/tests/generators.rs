// SPDX-License-Identifier: Apache-2.0

//! Behavior of the character and byte generators.

mod support;

use rand_core::RngCore;
use secretspec_api::resolve::{BytesParams, CharsParams, Require, SshAlgorithmParams};
use secretspec_api::v1alpha1::{EcdsaCurve, Encoding};
use secretspec_core::SecretValue;
use secretspec_core::generate::{
    GenerateError, generate_bytes, generate_chars, generate_ssh, htpasswd_line, htpasswd_verifies,
    os_seeded_rng, private_key_matches, public_key_line,
};
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

const ALL_ALGORITHMS: [SshAlgorithmParams; 5] = [
    SshAlgorithmParams::Ed25519,
    SshAlgorithmParams::Rsa { bits: 2048 },
    SshAlgorithmParams::Ecdsa {
        curve: EcdsaCurve::P256,
    },
    SshAlgorithmParams::Ecdsa {
        curve: EcdsaCurve::P384,
    },
    SshAlgorithmParams::Ecdsa {
        curve: EcdsaCurve::P521,
    },
];

#[test]
fn ssh_ed25519_round_trip() {
    let pair = generate_ssh(
        SshAlgorithmParams::Ed25519,
        "deploy@app",
        &mut seeded_rng(1),
    )
    .unwrap();
    let private = pair.private_key.expose();
    assert!(private.starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----"));
    assert!(pair.public_key.starts_with("ssh-ed25519 "));
    assert!(pair.public_key.ends_with(" deploy@app\n"));
    assert_eq!(
        public_key_line(private, "deploy@app").unwrap(),
        pair.public_key
    );
    assert!(private_key_matches(private, SshAlgorithmParams::Ed25519));
    assert!(!private_key_matches(
        private,
        SshAlgorithmParams::Rsa { bits: 2048 }
    ));
}

#[test]
fn ssh_empty_comment() {
    let pair = generate_ssh(SshAlgorithmParams::Ed25519, "", &mut seeded_rng(2)).unwrap();
    let line = pair.public_key.strip_suffix('\n').unwrap();
    assert_eq!(line.split(' ').count(), 2);
    assert!(!pair.public_key.ends_with(" \n"));
    assert_eq!(
        public_key_line(pair.private_key.expose(), "").unwrap(),
        pair.public_key
    );
}

#[test]
fn ssh_rsa_2048_and_ecdsa_curves() {
    let expected_prefix = [
        "ssh-ed25519 ",
        "ssh-rsa ",
        "ecdsa-sha2-nistp256 ",
        "ecdsa-sha2-nistp384 ",
        "ecdsa-sha2-nistp521 ",
    ];
    for (i, (alg, prefix)) in ALL_ALGORITHMS.iter().zip(expected_prefix).enumerate() {
        let pair = generate_ssh(*alg, "c", &mut seeded_rng(10 + i as u64)).unwrap();
        assert!(pair.public_key.starts_with(prefix), "{alg:?}");
        let private = pair.private_key.expose();
        for (j, other) in ALL_ALGORITHMS.iter().enumerate() {
            assert_eq!(
                private_key_matches(private, *other),
                i == j,
                "{alg:?} vs {other:?}"
            );
        }
    }
    // Same algorithm, different RSA size.
    let pair = generate_ssh(
        SshAlgorithmParams::Rsa { bits: 2048 },
        "",
        &mut seeded_rng(3),
    )
    .unwrap();
    assert!(!private_key_matches(
        pair.private_key.expose(),
        SshAlgorithmParams::Rsa { bits: 3072 }
    ));
}

#[test]
#[ignore = "slow: RSA 4096"]
fn ssh_rsa_4096() {
    let pair = generate_ssh(
        SshAlgorithmParams::Rsa { bits: 4096 },
        "big",
        &mut seeded_rng(4),
    )
    .unwrap();
    assert!(private_key_matches(
        pair.private_key.expose(),
        SshAlgorithmParams::Rsa { bits: 4096 }
    ));
}

#[test]
fn ssh_comment_change_keeps_key() {
    let pair = generate_ssh(SshAlgorithmParams::Ed25519, "old", &mut seeded_rng(5)).unwrap();
    let renamed = public_key_line(pair.private_key.expose(), "new").unwrap();
    assert!(renamed.ends_with(" new\n"));
    let blob = |line: &str| line.split(' ').nth(1).map(str::to_owned);
    assert_eq!(blob(&renamed), blob(&pair.public_key));
}

/// A committed SSH fixture: private key file name, its parameters and the
/// expected `authorized_keys` line for comment `fixture`.
struct Golden {
    name: &'static str,
    params: SshAlgorithmParams,
    private_key: &'static [u8],
    public_line: &'static str,
}

macro_rules! golden {
    ($name:literal, $params:expr) => {
        Golden {
            name: $name,
            params: $params,
            private_key: include_bytes!(concat!("fixtures/ssh/", $name)),
            public_line: include_str!(concat!("fixtures/ssh/", $name, ".pub")),
        }
    };
}

const P256: SshAlgorithmParams = SshAlgorithmParams::Ecdsa {
    curve: EcdsaCurve::P256,
};
const P384: SshAlgorithmParams = SshAlgorithmParams::Ecdsa {
    curve: EcdsaCurve::P384,
};
const P521: SshAlgorithmParams = SshAlgorithmParams::Ecdsa {
    curve: EcdsaCurve::P521,
};
const RSA_2048: SshAlgorithmParams = SshAlgorithmParams::Rsa { bits: 2048 };
const RSA_3072: SshAlgorithmParams = SshAlgorithmParams::Rsa { bits: 3072 };
const RSA_4096: SshAlgorithmParams = SshAlgorithmParams::Rsa { bits: 4096 };
const ED25519: SshAlgorithmParams = SshAlgorithmParams::Ed25519;

/// Every supported parameter set, for the negative checks.
const ALL_SSH_PARAMS: [SshAlgorithmParams; 7] =
    [ED25519, RSA_2048, RSA_3072, RSA_4096, P256, P384, P521];

/// Golden private keys, one set written by this crate and one by OpenSSH's
/// `ssh-keygen` (see `fixtures/ssh/README.md`).
///
/// A failure here blocks a release: stored private keys that no longer match
/// their parameters, or whose re-derived public line changes, would make the
/// operator regenerate or rewrite every SSH key pair on upgrade.
#[test]
fn ssh_golden_fixtures() {
    let fixtures = [
        golden!("crate-ed25519", ED25519),
        golden!("crate-rsa-2048", RSA_2048),
        golden!("crate-rsa-3072", RSA_3072),
        golden!("crate-rsa-4096", RSA_4096),
        golden!("crate-ecdsa-p256", P256),
        golden!("crate-ecdsa-p384", P384),
        golden!("crate-ecdsa-p521", P521),
        golden!("openssh-ed25519", ED25519),
        golden!("openssh-rsa-2048", RSA_2048),
        golden!("openssh-rsa-3072", RSA_3072),
        golden!("openssh-rsa-4096", RSA_4096),
        golden!("openssh-ecdsa-p256", P256),
        golden!("openssh-ecdsa-p384", P384),
        golden!("openssh-ecdsa-p521", P521),
    ];
    for golden in &fixtures {
        assert!(
            private_key_matches(golden.private_key, golden.params),
            "{} no longer matches its parameters",
            golden.name
        );
        for other in ALL_SSH_PARAMS.into_iter().filter(|p| *p != golden.params) {
            assert!(
                !private_key_matches(golden.private_key, other),
                "{} matches {other:?}",
                golden.name
            );
        }
        assert_eq!(
            public_key_line(golden.private_key, "fixture").unwrap(),
            golden.public_line,
            "{}: re-derived public key line changed",
            golden.name
        );
    }
}

#[test]
fn public_key_line_rejects_garbage() {
    assert!(matches!(
        public_key_line(b"not a key", "c"),
        Err(GenerateError::Ssh(_))
    ));
    assert!(!private_key_matches(
        b"not a key",
        SshAlgorithmParams::Ed25519
    ));
}

#[test]
fn ssh_error_and_debug_never_show_key_material() {
    let pair = generate_ssh(SshAlgorithmParams::Ed25519, "c", &mut seeded_rng(6)).unwrap();
    assert_eq!(format!("{:?}", pair.private_key), "<redacted>");
    let err = public_key_line(b"-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n", "c").unwrap_err();
    assert!(!err.to_string().contains("AAAA"));
}

fn is_htpasswd_shape(line: &str, user: &str) -> bool {
    let Some(hash) = line
        .strip_prefix(user)
        .and_then(|r| r.strip_prefix(":$2y$12$"))
    else {
        return false;
    };
    hash.len() == 53
        && hash
            .bytes()
            .all(|b| b == b'.' || b == b'/' || b.is_ascii_alphanumeric())
}

#[test]
fn htpasswd_format_and_verify() {
    let pw = SecretValue::new(b"s3cret-pass".to_vec());
    let line = htpasswd_line("admin", &pw, &mut seeded_rng(7)).unwrap();
    let text = std::str::from_utf8(line.expose()).unwrap();
    assert!(is_htpasswd_shape(text, "admin"), "{text}");
    assert!(!text.contains('\n'));
    assert!(htpasswd_verifies(line.expose(), "admin", &pw));
    assert!(!htpasswd_verifies(
        line.expose(),
        "admin",
        &SecretValue::new(b"other".to_vec())
    ));
    assert!(!htpasswd_verifies(line.expose(), "root", &pw));
    assert!(!htpasswd_verifies(b"admin", "admin", &pw));
    assert!(!htpasswd_verifies(b"admin:not-a-hash", "admin", &pw));

    let same = htpasswd_line("admin", &pw, &mut seeded_rng(7)).unwrap();
    assert_eq!(same.expose(), line.expose());
    let other = htpasswd_line("admin", &pw, &mut seeded_rng(8)).unwrap();
    assert_ne!(other.expose(), line.expose());
}

#[test]
fn htpasswd_verifies_line_from_htpasswd_tool() {
    // Produced by `htpasswd -nbB -C 12 admin secret` (docs/fingerprints.md, vector 9).
    let line = b"admin:$2y$12$0DlAzt5T2HhGhCfza8hh9uTBlWS.K.Fiorr.aH/bnWF6xDZES4D.K";
    assert!(htpasswd_verifies(
        line,
        "admin",
        &SecretValue::new(b"secret".to_vec())
    ));
}
