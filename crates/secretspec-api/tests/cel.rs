// SPDX-License-Identifier: Apache-2.0

//! Client-side evaluation of the CEL rules embedded in the CRD.
//!
//! Serde defaults stand in for schema defaults: the typed object is serialised
//! with every defaulted field present, which is what the API server sees after
//! defaulting.
// Test helpers (not `#[test]` fns) may panic on broken fixtures.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeSet;

use kube::CustomResourceExt;
use kube::core::cel::{
    ErrorKind, ScopeContext, WarningKind, analyze_rule, compile_schema, validate_cel,
    validate_cel_update,
};
use secretspec_api::constants::{self, ALL_MESSAGES};
use secretspec_api::v1alpha1::SecretSpec;
use serde_json::Value;

fn parse(fixture: &common::Fixture) -> SecretSpec {
    serde_json::from_value(fixture.value.clone())
        .unwrap_or_else(|e| panic!("{}: does not deserialize: {e}", fixture.file))
}

#[test]
fn valid_fixtures_pass_cel() {
    let fixtures = common::load("valid");
    assert!(
        fixtures.len() >= 17,
        "got {} valid fixtures",
        fixtures.len()
    );
    for fixture in fixtures {
        let obj = parse(&fixture);
        if let Err(e) = validate_cel(&obj) {
            panic!("{}: unexpectedly rejected:\n{e}", fixture.file);
        }
    }
}

#[test]
fn cel_layer_invalid_fixtures_fail_with_expected_message() {
    let fixtures = common::load("invalid");
    assert!(
        fixtures.len() >= 33,
        "got {} invalid fixtures",
        fixtures.len()
    );
    let mut checked = 0;
    for fixture in fixtures.iter().filter(|f| f.layer == common::Layer::Cel) {
        let expect = fixture
            .expect
            .as_deref()
            .unwrap_or_else(|| panic!("{}: missing `# expect:`", fixture.file));
        let obj = parse(fixture);
        let errors =
            validate_cel(&obj).expect_err(&format!("{}: expected a CEL rejection", fixture.file));
        assert!(
            errors
                .iter()
                .any(|e| e.kind == ErrorKind::ValidationFailure && e.message.contains(expect)),
            "{}: no rule failed with {expect:?}; got:\n{errors}",
            fixture.file
        );
        // A rule that errors at runtime (instead of evaluating to false) is a
        // rule bug, even when the object is rejected anyway.
        assert!(
            errors
                .iter()
                .all(|e| e.kind == ErrorKind::ValidationFailure),
            "{}: a rule failed to evaluate:\n{errors}",
            fixture.file
        );
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} CEL-layer fixtures");
}

#[test]
fn schema_layer_fixtures_declare_what_the_server_says() {
    for fixture in common::load("invalid") {
        assert!(
            fixture.expect.is_some(),
            "{}: missing `# expect:`",
            fixture.file
        );
    }
}

#[test]
fn type_change_is_rejected() {
    let fixtures = common::load("update/u01-type-immutable");
    let by_name = |name: &str| {
        fixtures
            .iter()
            .find(|f| f.file == name)
            .unwrap_or_else(|| panic!("missing {name}"))
    };
    let (old, new) = (by_name("old.yaml"), by_name("new.yaml"));
    let (old_obj, new_obj) = (parse(old), parse(new));

    let errors = validate_cel_update(&new_obj, &old_obj).expect_err("type change must fail");
    let expect = new.expect.as_deref().expect("new.yaml declares expect");
    assert_eq!(expect, constants::MSG_TYPE_IMMUTABLE);
    assert!(
        errors
            .iter()
            .any(|e| e.kind == ErrorKind::ValidationFailure && e.message.contains(expect)),
        "got:\n{errors}"
    );
    assert!(
        errors
            .iter()
            .all(|e| e.kind == ErrorKind::ValidationFailure),
        "a rule failed to evaluate:\n{errors}"
    );

    validate_cel_update(&old_obj, &old_obj).expect("unchanged type is accepted");
    validate_cel_update(&new_obj, &new_obj).expect("unchanged type is accepted");
}

/// One `x-kubernetes-validations` entry of the CRD with the schema node it
/// is attached to.
struct CrdRule {
    path: String,
    message: String,
    rule: String,
    node: Value,
}

/// Every `x-kubernetes-validations` entry in the CRD.
fn crd_rules() -> Vec<CrdRule> {
    fn walk(path: &str, node: &Value, out: &mut Vec<CrdRule>) {
        let Value::Object(map) = node else { return };
        if let Some(Value::Array(rules)) = map.get("x-kubernetes-validations") {
            for rule in rules {
                let text = |key: &str| {
                    rule[key]
                        .as_str()
                        .unwrap_or_else(|| panic!("{path}: rule without {key}: {rule}"))
                        .to_owned()
                };
                out.push(CrdRule {
                    path: path.to_owned(),
                    message: text("message"),
                    rule: text("rule"),
                    node: node.clone(),
                });
            }
        }
        for (key, child) in map {
            let step = match key.as_str() {
                "properties" => {
                    for (name, prop) in child.as_object().expect("properties") {
                        walk(&format!("{path}.{name}"), prop, out);
                    }
                    continue;
                }
                "items" => "[]",
                "additionalProperties" => "{}",
                _ => continue,
            };
            walk(&format!("{path}{step}"), child, out);
        }
    }
    let crd = serde_json::to_value(SecretSpec::crd()).expect("crd serialises");
    let spec = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"];
    let mut out = Vec::new();
    walk("spec", spec, &mut out);
    out
}

#[test]
fn every_rule_has_a_fixture() {
    let mut expected: BTreeSet<String> = BTreeSet::new();
    for dir in ["invalid", "update/u01-type-immutable"] {
        expected.extend(common::load(dir).into_iter().filter_map(|f| f.expect));
    }
    let untested: Vec<&str> = ALL_MESSAGES
        .iter()
        .copied()
        .filter(|m| !expected.contains(*m))
        .collect();
    assert!(
        untested.is_empty(),
        "rules without a fixture expecting their message: {untested:?}"
    );
}

#[test]
fn crd_rules_and_message_constants_agree() {
    let in_crd: BTreeSet<String> = crd_rules().into_iter().map(|r| r.message).collect();
    let declared: BTreeSet<String> = ALL_MESSAGES.iter().map(|m| (*m).to_owned()).collect();
    assert_eq!(
        declared.len(),
        ALL_MESSAGES.len(),
        "duplicate message constants"
    );
    assert_eq!(in_crd, declared, "CRD rule messages vs constants.rs");
}

#[test]
fn rules_sit_on_the_narrowest_schema_node() {
    use constants::*;
    let mut got: Vec<(String, String)> = crd_rules()
        .into_iter()
        .map(|r| (r.path, r.message))
        .collect();
    got.sort();
    let mut want: Vec<(&str, &str)> = vec![
        ("spec", MSG_EXACTLY_ONE_KIND),
        ("spec", MSG_DATA_COLLISION),
        ("spec", MSG_BASIC_AUTH_TYPE),
        ("spec", MSG_SSH_AUTH_TYPE),
        ("spec.data", MSG_DATA_KEY_FORMAT),
        ("spec.rotation.interval", MSG_ROTATION_RANGE),
        ("spec.secret.type", MSG_TYPE_IMMUTABLE),
        ("spec.secret.labels", MSG_RESERVED_METADATA),
        ("spec.secret.annotations", MSG_RESERVED_METADATA),
        ("spec.sshKeyPair", MSG_RSA_BITS_ONLY_RSA),
        ("spec.sshKeyPair", MSG_CURVE_ONLY_ECDSA),
        ("spec.sshKeyPair.keys", MSG_KEYS_DISTINCT),
        ("spec.basicAuth.keys", MSG_KEYS_DISTINCT),
        ("spec.fields[]", MSG_BYTES_EXCLUSIVE),
        ("spec.fields[]", MSG_ENCODING_WITH_BYTES),
        ("spec.fields[]", MSG_ALPHABET_WITH_CUSTOM),
        ("spec.fields[]", MSG_REQUIRE_SUM),
        ("spec.basicAuth.password", MSG_ALPHABET_WITH_CUSTOM),
        ("spec.basicAuth.password", MSG_REQUIRE_SUM),
    ];
    want.sort_unstable();
    let got: Vec<(&str, &str)> = got.iter().map(|(p, m)| (p.as_str(), m.as_str())).collect();
    assert_eq!(got, want);
}

#[test]
fn rules_pass_static_analysis() {
    let rules = crd_rules();
    assert!(!rules.is_empty());
    for r in rules {
        let compiled = compile_schema(&r.node);
        // The scope check mistakes comprehension variables (`exists(f, ...)`)
        // for unknown identifiers; only the cost/bounds findings are relevant.
        let warnings: Vec<_> = analyze_rule(&r.rule, &compiled, ScopeContext::CrdValidation)
            .into_iter()
            .filter(|w| w.kind != WarningKind::WrongScope)
            .collect();
        assert!(
            warnings.is_empty(),
            "{} ({}): {warnings:?}",
            r.path,
            r.message
        );
    }
}

#[test]
fn rotation_interval_boundaries() {
    let base = common::load("valid")
        .into_iter()
        .find(|f| f.file == "v14-rotation-60m.yaml")
        .expect("v14 exists");
    let with = |interval: &str| {
        let mut value = base.value.clone();
        value["spec"]["rotation"]["interval"] = Value::String(interval.to_owned());
        serde_json::from_value::<SecretSpec>(value).expect("deserialises")
    };
    for ok in ["60m", "61m", "5256000m", "1h", "87600h", "1d", "3650d"] {
        if let Err(e) = validate_cel(&with(ok)) {
            panic!("{ok} must be accepted:\n{e}");
        }
    }
    for bad in ["1m", "59m", "5256001m", "87601h", "3651d"] {
        let errors = validate_cel(&with(bad)).expect_err(bad);
        assert!(
            errors.iter().any(|e| e.kind == ErrorKind::ValidationFailure
                && e.message == constants::MSG_ROTATION_RANGE),
            "{bad}: {errors}"
        );
    }
}

/// The CEL rule for `data` keys and the runtime check in `resolve()` must
/// accept exactly the same key names.
#[test]
fn data_key_names_agree_between_cel_and_resolve() {
    let base = common::load("valid")
        .into_iter()
        .find(|f| f.file == "v01-minimal-fields.yaml")
        .expect("v01 exists");
    let long_ok = "a".repeat(253);
    let long_bad = "a".repeat(254);
    let cases = [
        ("a", true),
        (".a", true),
        ("a.", true),
        ("a..b", true),
        ("-", true),
        ("_", true),
        (".-", true),
        (long_ok.as_str(), true),
        (".", false),
        ("..", false),
        ("..a", false),
        ("...", false),
        ("", false),
        ("a/b", false),
        ("a b", false),
        ("ü", false),
        (long_bad.as_str(), false),
    ];
    for (key, ok) in cases {
        let mut value = base.value.clone();
        value["spec"]["data"] = serde_json::json!({ key: "v" });
        let obj: SecretSpec = serde_json::from_value(value).expect("deserialises");

        let cel_ok = validate_cel(&obj).is_ok();
        assert_eq!(cel_ok, ok, "CEL on {key:?}");
        let resolve_ok = secretspec_api::resolve(&obj.spec, "n", "ns").is_ok();
        assert_eq!(resolve_ok, ok, "resolve on {key:?}");
    }
}
