// SPDX-License-Identifier: Apache-2.0

//! Defaults resolution and runtime validation (`resolve()`).
// Test helpers (not `#[test]` fns) may panic on broken fixtures.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::time::Duration;

use secretspec_api::constants::{
    ALL_MESSAGES, MSG_ALPHABET_WITH_CUSTOM, MSG_DATA_COLLISION, MSG_KEYS_DISTINCT, MSG_REQUIRE_SUM,
    MSG_ROTATION_RANGE, MSG_TYPE_IMMUTABLE,
};
use secretspec_api::resolve::{
    BytesParams, CharsParams, Generator, InvalidSpec, Require, ResolvedBasicAuth, ResolvedField,
    ResolvedKind, ResolvedSpec, ResolvedSsh, SshAlgorithmParams, resolve,
};
use secretspec_api::v1alpha1::{
    DeletionPolicy, EcdsaCurve, Encoding, SecretKind, SecretSpec, SecretSpecSpec,
};

/// Fixtures that cannot deserialize into `SecretSpec` (type errors) and
/// therefore never reach `resolve()`. Every invalid fixture is expected to
/// deserialize, so this stays empty.
const NOT_DESERIALIZABLE: &[&str] = &[];

fn resolve_object(obj: &SecretSpec) -> Result<ResolvedSpec, InvalidSpec> {
    resolve(
        &obj.spec,
        obj.metadata.name.as_deref().expect("fixture has a name"),
        obj.metadata
            .namespace
            .as_deref()
            .expect("fixture has a namespace"),
    )
}

fn fixture_object(dir: &str, prefix: &str) -> SecretSpec {
    let fixture = common::load(dir)
        .into_iter()
        .find(|f| f.file.starts_with(prefix))
        .unwrap_or_else(|| panic!("no fixture {prefix}* in {dir}"));
    serde_json::from_value(fixture.value).expect("fixture deserializes")
}

fn resolve_valid(prefix: &str) -> ResolvedSpec {
    let obj = fixture_object("valid", prefix);
    resolve_object(&obj).unwrap_or_else(|e| panic!("{prefix}: {e}"))
}

fn inline_spec(yaml: &str) -> SecretSpecSpec {
    serde_norway::from_str(yaml).expect("inline manifest deserializes")
}

fn resolve_inline(yaml: &str) -> ResolvedSpec {
    resolve(&inline_spec(yaml), "n", "ns").unwrap_or_else(|e| panic!("{e}\n{yaml}"))
}

fn inline_error(yaml: &str) -> String {
    resolve(&inline_spec(yaml), "n", "ns")
        .expect_err(&format!("expected a rejection:\n{yaml}"))
        .to_string()
}

fn fields_of(resolved: &ResolvedSpec) -> &[ResolvedField] {
    let ResolvedKind::Fields(fields) = &resolved.kind else {
        panic!("not a Fields spec: {:?}", resolved.kind);
    };
    fields
}

fn chars_of(field: &ResolvedField) -> &CharsParams {
    let Generator::Chars(chars) = &field.generator else {
        panic!("not a characters field: {field:?}");
    };
    chars
}

fn ssh_of(resolved: &ResolvedSpec) -> &ResolvedSsh {
    let ResolvedKind::SshKeyPair(ssh) = &resolved.kind else {
        panic!("not an SshKeyPair spec: {:?}", resolved.kind);
    };
    ssh
}

fn basic_auth_of(resolved: &ResolvedSpec) -> &ResolvedBasicAuth {
    let ResolvedKind::BasicAuth(basic) = &resolved.kind else {
        panic!("not a BasicAuth spec: {:?}", resolved.kind);
    };
    basic
}

fn alphanumeric() -> Vec<u8> {
    (b'0'..=b'9')
        .chain(b'A'..=b'Z')
        .chain(b'a'..=b'z')
        .collect()
}

#[test]
fn valid_fixtures_all_resolve() {
    for fixture in common::load("valid") {
        let obj: SecretSpec = serde_json::from_value(fixture.value).expect("deserializes");
        if let Err(e) = resolve_object(&obj) {
            panic!("{}: {e}", fixture.file);
        }
    }
}

#[test]
fn minimal_field_resolves_to_frozen_defaults() {
    let resolved = resolve_valid("v01");
    assert_eq!(resolved.secret_kind(), SecretKind::Fields);
    assert_eq!(resolved.deletion_policy, DeletionPolicy::Delete);
    assert_eq!(resolved.rotation, None);
    assert_eq!(resolved.secret_type, "Opaque");
    assert!(resolved.data.is_empty());
    assert!(resolved.labels.is_empty() && resolved.annotations.is_empty());
    let fields = fields_of(&resolved);
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "password");
    let expected = alphanumeric();
    assert_eq!(expected.len(), 62);
    assert_eq!(
        fields[0].generator,
        Generator::Chars(CharsParams {
            length: 32,
            alphabet: expected,
            require: Require {
                lower: 0,
                upper: 0,
                digits: 0,
                symbols: 0
            },
            prefix: String::new(),
            suffix: String::new(),
        })
    );
}

#[test]
fn bytes_field_resolves() {
    let resolved = resolve_valid("v09");
    let fields = fields_of(&resolved);
    assert_eq!(
        fields[0].generator,
        Generator::Bytes(BytesParams {
            count: 32,
            encoding: Encoding::Hex,
            prefix: String::new(),
            suffix: String::new(),
        })
    );
}

#[test]
fn rds_password_alphabet_excludes_chars() {
    let resolved = resolve_valid("v12");
    let fields = fields_of(&resolved);
    let chars = chars_of(&fields[0]);
    // 94 printable non-space characters; `/`, `@` and `"` are removed. The
    // excluded space was never in the alphabet, so 94 - 3 = 91 remain.
    assert_eq!(chars.alphabet.len(), 91);
    for excluded in *b"/@\" " {
        assert!(!chars.alphabet.contains(&excluded), "{excluded}");
    }
    assert!(chars.alphabet.windows(2).all(|w| w[0] < w[1]), "sorted");
    assert_eq!(
        chars.require,
        Require {
            lower: 1,
            upper: 1,
            digits: 1,
            symbols: 1
        }
    );
}

#[test]
fn custom_alphabet_is_deduplicated_and_sorted() {
    let resolved = resolve_inline("fields: [{name: pin, charset: custom, alphabet: cbaab}]");
    assert_eq!(chars_of(&fields_of(&resolved)[0]).alphabet, b"abc");
}

#[test]
fn charset_alphabets_are_exact() {
    let resolved = resolve_inline(
        "fields: [{name: h, charset: hex}, {name: a, charset: ascii}, {name: x, charset: alphanumeric}]",
    );
    let fields = fields_of(&resolved);
    assert_eq!(chars_of(&fields[0]).alphabet, b"0123456789abcdef");
    let ascii = &chars_of(&fields[1]).alphabet;
    assert_eq!(ascii.len(), 94);
    assert!(ascii.iter().all(u8::is_ascii_graphic));
    assert_eq!(chars_of(&fields[2]).alphabet, alphanumeric());
}

#[test]
fn prefix_and_suffix_are_kept() {
    let resolved = resolve_inline(
        "fields: [{name: k, prefix: GK, length: 24, charset: hex}, {name: b, bytes: 4, encoding: base32, suffix: '=x'}]",
    );
    let fields = fields_of(&resolved);
    let chars = chars_of(&fields[0]);
    assert_eq!((chars.length, chars.prefix.as_str()), (24, "GK"));
    let Generator::Bytes(bytes) = &fields[1].generator else {
        panic!("not bytes");
    };
    assert_eq!(
        (bytes.encoding, bytes.suffix.as_str()),
        (Encoding::Base32, "=x")
    );
}

#[test]
fn ssh_defaults() {
    let obj = fixture_object("valid", "v03");
    let resolved = resolve(&obj.spec, "deploy", "app").expect("v03 resolves");
    assert_eq!(resolved.secret_kind(), SecretKind::SshKeyPair);
    assert_eq!(
        ssh_of(&resolved),
        &ResolvedSsh {
            algorithm: SshAlgorithmParams::Ed25519,
            comment: "deploy@app".to_owned(),
            private_key_key: "ssh-privatekey".to_owned(),
            public_key_key: "ssh-publickey".to_owned(),
        }
    );

    assert_eq!(
        ssh_of(&resolve_valid("v04")).algorithm,
        SshAlgorithmParams::Rsa { bits: 3072 }
    );
    assert_eq!(
        ssh_of(&resolve_valid("v05")).algorithm,
        SshAlgorithmParams::Ecdsa {
            curve: EcdsaCurve::P384
        }
    );
    assert_eq!(
        ssh_of(&resolve_inline("sshKeyPair: {algorithm: rsa}")).algorithm,
        SshAlgorithmParams::Rsa { bits: 4096 }
    );
    assert_eq!(
        ssh_of(&resolve_inline("sshKeyPair: {algorithm: ecdsa}")).algorithm,
        SshAlgorithmParams::Ecdsa {
            curve: EcdsaCurve::P256
        }
    );
    assert_eq!(ssh_of(&resolve_valid("v17")).comment, "");
}

#[test]
fn basic_auth_defaults() {
    let resolved = resolve_valid("v06");
    assert_eq!(resolved.secret_kind(), SecretKind::BasicAuth);
    let basic = basic_auth_of(&resolved);
    assert_eq!(basic.username, "admin");
    assert_eq!(basic.password.length, 32);
    assert_eq!(basic.password.alphabet, alphanumeric());
    assert_eq!(basic.password.require, Require::default());
    assert_eq!(
        (
            basic.username_key.as_str(),
            basic.password_key.as_str(),
            basic.htpasswd_key.as_str()
        ),
        ("username", "password", "users")
    );
    // Without a `password` block everything is defaulted, htpasswd key `auth`.
    let minimal = resolve_inline("basicAuth: {username: admin}");
    let basic = basic_auth_of(&minimal);
    assert_eq!(
        (basic.password.length, basic.htpasswd_key.as_str()),
        (32, "auth")
    );
}

#[test]
fn secret_metadata_is_carried_over() {
    let resolved = resolve_inline(
        "deletionPolicy: Retain\n\
         data: {owner: app}\n\
         secret: {type: kubernetes.io/basic-auth, labels: {a: b}, annotations: {c: d}}\n\
         basicAuth: {username: admin}",
    );
    assert_eq!(resolved.deletion_policy, DeletionPolicy::Retain);
    assert_eq!(resolved.secret_type, "kubernetes.io/basic-auth");
    assert_eq!(resolved.data.get("owner").map(String::as_str), Some("app"));
    assert_eq!(resolved.labels.get("a").map(String::as_str), Some("b"));
    assert_eq!(resolved.annotations.get("c").map(String::as_str), Some("d"));
}

#[test]
fn rotation_parses() {
    let secs = |interval: &str| {
        resolve_inline(&format!(
            "rotation: {{interval: {interval}}}\nfields: [{{name: p}}]"
        ))
        .rotation
    };
    assert_eq!(secs("60m"), Some(Duration::from_secs(3_600)));
    assert_eq!(secs("1h"), Some(Duration::from_secs(3_600)));
    assert_eq!(secs("90d"), Some(Duration::from_secs(7_776_000)));
    assert_eq!(secs("3650d"), Some(Duration::from_secs(315_360_000)));
    assert_eq!(secs("87600h"), Some(Duration::from_secs(315_360_000)));
}

#[test]
fn rotation_out_of_range_or_malformed_is_rejected_not_a_panic() {
    for bad in [
        "59m",
        "1m",
        "3651d",
        "87601h",
        "5256001m",
        "9999999d",
        "0h",
        "1w",
        "d",
        "",
        "h1",
        "-1h",
        "+1h",
        "01h",
        "1 h",
        "99999999d",
        "١h",
        "1é",
    ] {
        let yaml = format!("rotation: {{interval: \"{bad}\"}}\nfields: [{{name: p}}]");
        let err = inline_error(&yaml);
        assert!(err.contains("spec.rotation.interval"), "{bad:?}: {err}");
    }
    assert!(
        inline_error("rotation: {interval: 59m}\nfields: [{name: p}]").contains(MSG_ROTATION_RANGE)
    );
}

#[test]
fn runtime_rules() {
    let err = inline_error("fields: [{name: p, charset: custom, alphabet: ab, exclude: a}]");
    assert!(err.contains("at least 2 characters"), "{err}");
    assert!(
        err.contains("spec.fields[0]: alphabet must contain at least 2 characters after exclude"),
        "{err}"
    );

    let err = inline_error("fields: [{name: p, charset: hex, require: {upper: 1}}]");
    assert!(err.contains("no upper characters left"), "{err}");
    assert!(
        err.contains("spec.fields[0].require.upper: no upper characters left in the alphabet"),
        "{err}"
    );

    // The same runtime rules apply to basicAuth.password.
    let err =
        inline_error("basicAuth: {username: u, password: {charset: hex, require: {symbols: 1}}}");
    assert!(
        err.contains("spec.basicAuth.password.require.symbols: no symbols characters left"),
        "{err}"
    );
}

#[test]
fn errors_are_collected() {
    let err = resolve(
        &inline_spec(
            "data: {password: fixed}\n\
             rotation: {interval: 59m}\n\
             fields: [{name: password, length: 4, require: {lower: 5}}]",
        ),
        "n",
        "ns",
    )
    .expect_err("three violations");
    assert!(err.errors.len() >= 3, "{err}");
    let text = err.to_string();
    for msg in [MSG_DATA_COLLISION, MSG_ROTATION_RANGE, MSG_REQUIRE_SUM] {
        assert!(text.contains(msg), "missing {msg:?} in {text}");
    }
    for e in &err.errors {
        assert!(e.contains(": "), "not `<path>: <message>`: {e}");
    }
}

#[test]
fn every_invalid_fixture_is_rejected_by_resolve() {
    let fixtures = common::load("invalid");
    assert!(fixtures.len() >= 33);
    for fixture in fixtures {
        if NOT_DESERIALIZABLE.contains(&fixture.file.as_str()) {
            continue;
        }
        let obj: SecretSpec = serde_json::from_value(fixture.value.clone())
            .unwrap_or_else(|e| panic!("{}: does not deserialize: {e}", fixture.file));
        let err = resolve_object(&obj)
            .expect_err(&format!("{}: resolve() accepted it", fixture.file))
            .to_string();
        let expect = match fixture.layer {
            common::Layer::Cel => fixture.expect.clone().expect("cel fixture has `# expect:`"),
            // The API server's wording is not reproduced; the offending JSON
            // path is. `# expect:` is that path unless a header overrides it.
            common::Layer::Schema => fixture
                .resolve_expect
                .clone()
                .or_else(|| fixture.expect.clone().filter(|e| e.starts_with("spec.")))
                .unwrap_or_else(|| {
                    panic!("{}: schema fixture needs `# resolve-expect:`", fixture.file)
                }),
        };
        assert!(
            err.contains(&expect),
            "{}: expected {expect:?} in:\n{err}",
            fixture.file
        );
    }
}

#[test]
fn resolve_reports_every_cel_message() {
    let mut all = String::new();
    for fixture in common::load("invalid") {
        let obj: SecretSpec = serde_json::from_value(fixture.value).expect("deserializes");
        all.push_str(&resolve_object(&obj).expect_err("invalid").to_string());
        all.push('\n');
    }
    for msg in ALL_MESSAGES {
        // `secret.type` immutability is a transition rule: it needs the old
        // object, which `resolve()` does not have.
        if *msg == MSG_TYPE_IMMUTABLE {
            continue;
        }
        assert!(all.contains(msg), "resolve never reports {msg:?}");
    }
}

#[test]
fn basic_auth_password_cel_rules_are_rechecked() {
    let err = inline_error("basicAuth: {username: u, password: {charset: custom}}");
    assert!(err.contains(MSG_ALPHABET_WITH_CUSTOM), "{err}");
    assert!(err.contains("spec.basicAuth.password"), "{err}");

    let err = inline_error(
        "basicAuth: {username: u, password: {length: 4, require: {lower: 3, digits: 2}}}",
    );
    assert!(err.contains(MSG_REQUIRE_SUM), "{err}");

    // The sum is checked against the default length (32) when unset.
    let err = inline_error("basicAuth: {username: u, password: {require: {lower: 33}}}");
    assert!(err.contains(MSG_REQUIRE_SUM), "{err}");
}

#[test]
fn data_collisions_with_every_generated_key_are_rejected() {
    for yaml in [
        "data: {ssh-publickey: x}\nsshKeyPair: {}",
        "data: {ssh-privatekey: x}\nsshKeyPair: {}",
        "data: {custom-pub: x}\nsshKeyPair: {keys: {publicKey: custom-pub}}",
        "data: {username: x}\nbasicAuth: {username: u}",
        "data: {password: x}\nbasicAuth: {username: u}",
        "data: {auth: x}\nbasicAuth: {username: u}",
        "data: {users: x}\nbasicAuth: {username: u, keys: {htpasswd: users}}",
        "data: {b: x}\nfields: [{name: a}, {name: b, bytes: 1, encoding: hex}]",
    ] {
        let err = inline_error(yaml);
        assert!(err.contains(MSG_DATA_COLLISION), "{yaml}: {err}");
    }
    // Keys that merely look similar do not collide.
    resolve_inline("data: {auth: x}\nbasicAuth: {username: u, keys: {htpasswd: users}}");
}

#[test]
fn basic_auth_keys_must_be_distinct() {
    for keys in [
        "{username: same, htpasswd: same}",
        "{username: same, password: same}",
        "{password: same, htpasswd: same}",
    ] {
        let err = inline_error(&format!("basicAuth: {{username: u, keys: {keys}}}"));
        assert!(err.contains(MSG_KEYS_DISTINCT), "{keys}: {err}");
        assert!(err.contains("spec.basicAuth.keys"), "{keys}: {err}");
    }
    // A default-named key reused explicitly is also a clash.
    let err = inline_error("basicAuth: {username: u, keys: {htpasswd: password}}");
    assert!(err.contains(MSG_KEYS_DISTINCT), "{err}");
    let err = inline_error("sshKeyPair: {keys: {publicKey: ssh-privatekey}}");
    assert!(err.contains(MSG_KEYS_DISTINCT), "{err}");
}

#[test]
fn schema_bounds_are_enforced() {
    let long = "a".repeat(254);
    let cases = [
        (format!("fields: [{{name: {long}}}]"), "spec.fields[0].name"),
        (
            "fields: [{name: p, length: 1025}]".to_owned(),
            "spec.fields[0].length",
        ),
        (
            "fields: [{name: p, bytes: 0, encoding: hex}]".to_owned(),
            "spec.fields[0].bytes",
        ),
        (
            "fields: [{name: p, require: {lower: 1025}}]".to_owned(),
            "spec.fields[0].require.lower",
        ),
        (
            "fields: [{name: p, charset: custom, alphabet: 'a b'}]".to_owned(),
            "spec.fields[0].alphabet",
        ),
        (
            "fields: [{name: p, exclude: \"\\n\"}]".to_owned(),
            "spec.fields[0].exclude",
        ),
        (
            "fields: [{name: p, suffix: 'a b'}]".to_owned(),
            "spec.fields[0].suffix",
        ),
        (
            format!("fields: [{{name: p, prefix: {}}}]", "a".repeat(65)),
            "spec.fields[0].prefix",
        ),
        (
            "basicAuth: {username: ''}".to_owned(),
            "spec.basicAuth.username",
        ),
        (
            "basicAuth: {username: u, password: {length: 0}}".to_owned(),
            "spec.basicAuth.password.length",
        ),
        (
            "basicAuth: {username: u, keys: {password: 'a/b'}}".to_owned(),
            "spec.basicAuth.keys.password",
        ),
        (
            "sshKeyPair: {keys: {privateKey: ''}}".to_owned(),
            "spec.sshKeyPair.keys.privateKey",
        ),
        (
            format!("sshKeyPair: {{comment: \"{}\"}}", "c".repeat(257)),
            "spec.sshKeyPair.comment",
        ),
        (
            format!("secret: {{type: {long}}}\nfields: [{{name: p}}]"),
            "spec.secret.type",
        ),
        (
            format!("data: {{k: {}}}\nfields: [{{name: p}}]", "v".repeat(65_537)),
            "spec.data[k]",
        ),
        (
            format!(
                "secret: {{labels: {{k: {}}}}}\nfields: [{{name: p}}]",
                "v".repeat(4_097)
            ),
            "spec.secret.labels[k]",
        ),
    ];
    for (yaml, path) in cases {
        let err = inline_error(&yaml);
        assert!(err.contains(path), "expected {path} in: {err}");
    }
}

#[test]
fn collection_sizes_are_bounded() {
    let fields: Vec<String> = (0..33).map(|i| format!("{{name: f{i}}}")).collect();
    assert!(inline_error(&format!("fields: [{}]", fields.join(","))).contains("spec.fields"));
    let data: Vec<String> = (0..65).map(|i| format!("k{i}: v")).collect();
    let err = inline_error(&format!(
        "data: {{{}}}\nfields: [{{name: p}}]",
        data.join(",")
    ));
    assert!(err.contains("spec.data"), "{err}");
    let labels: Vec<String> = (0..33).map(|i| format!("k{i}: v")).collect();
    let err = inline_error(&format!(
        "secret: {{annotations: {{{}}}}}\nfields: [{{name: p}}]",
        labels.join(",")
    ));
    assert!(err.contains("spec.secret.annotations"), "{err}");
    // The boundary itself is fine.
    let fields: Vec<String> = (0..32).map(|i| format!("{{name: f{i}}}")).collect();
    resolve_inline(&format!("fields: [{}]", fields.join(",")));
}

#[test]
fn type_contracts_are_rechecked() {
    resolve_inline("secret: {type: kubernetes.io/ssh-auth}\nsshKeyPair: {}");
    resolve_inline("secret: {type: kubernetes.io/basic-auth}\nbasicAuth: {username: u}");
    let err = inline_error(
        "secret: {type: kubernetes.io/basic-auth}\nbasicAuth: {username: u, keys: {password: pw}}",
    );
    assert!(err.contains("kubernetes.io/basic-auth requires"), "{err}");
}

#[test]
fn entropy_estimate() {
    let resolved = resolve_valid("v01");
    let bits = resolved.entropy_bits();
    assert_eq!(bits.len(), 1);
    assert_eq!(bits[0].0, "password");
    assert!((bits[0].1 - 32.0 * 62_f64.log2()).abs() < 1e-9, "{bits:?}");
    assert!((bits[0].1 - 190.5).abs() < 0.1);

    let short = resolve_inline("fields: [{name: p, length: 8}]").entropy_bits();
    assert!(short[0].1 < 128.0, "{short:?}");

    let bytes = resolve_inline("fields: [{name: t, bytes: 16, encoding: hex}]").entropy_bits();
    assert_eq!(bytes, vec![("t".to_owned(), 128.0)]);

    // Mode A uses the alphabet size after `exclude`; prefix/suffix add nothing.
    let hex = resolve_inline("fields: [{name: h, charset: hex, length: 10, prefix: GK}]");
    assert!((hex.entropy_bits()[0].1 - 40.0).abs() < 1e-9);
    let excl = resolve_inline("fields: [{name: h, charset: hex, exclude: '89abcdef', length: 4}]");
    assert!((excl.entropy_bits()[0].1 - 12.0).abs() < 1e-9);
}

#[test]
fn entropy_for_basic_auth_reports_the_password_key_and_ssh_is_empty() {
    let basic = resolve_inline(
        "basicAuth: {username: u, password: {length: 16, charset: hex}, keys: {password: pw}}",
    );
    let bits = basic.entropy_bits();
    assert_eq!(bits.len(), 1);
    assert_eq!(bits[0].0, "pw");
    assert!((bits[0].1 - 64.0).abs() < 1e-9);

    let ssh_bits = resolve_inline("sshKeyPair: {}").entropy_bits();
    assert_eq!(ssh_bits.len(), 0);
}
