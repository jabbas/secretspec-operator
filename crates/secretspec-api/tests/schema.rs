// SPDX-License-Identifier: Apache-2.0

//! Structural tests of the generated CRD schema and type round-trips.
// Test helpers (not `#[test]` fns) may panic on broken fixtures.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use kube::CustomResourceExt;
use secretspec_api::v1alpha1::SecretSpec;
use serde_json::{Value, json};

fn crd_json() -> Value {
    serde_json::to_value(SecretSpec::crd()).expect("crd serialises")
}

fn root_schema() -> Value {
    crd_json()["spec"]["versions"][0]["schema"]["openAPIV3Schema"].clone()
}

fn spec_schema() -> Value {
    root_schema()["properties"]["spec"].clone()
}

/// Follows `properties` / `items` steps, e.g. `["fields", "items", "length"]`.
fn at<'a>(schema: &'a Value, path: &[&str]) -> &'a Value {
    path.iter().fold(schema, |node, step| match *step {
        "items" => &node["items"],
        name => &node["properties"][name],
    })
}

#[test]
fn crd_has_expected_identity() {
    let crd = crd_json();
    let spec = &crd["spec"];
    assert_eq!(spec["group"], "secretspec.jabbas.eu");
    assert_eq!(spec["group"], secretspec_api::constants::GROUP);
    assert_eq!(spec["scope"], "Namespaced");
    assert_eq!(spec["names"]["kind"], "SecretSpec");
    assert_eq!(spec["names"]["plural"], "secretspecs");
    assert_eq!(spec["names"]["shortNames"], json!(["ss"]));

    let versions = spec["versions"].as_array().expect("versions");
    assert_eq!(versions.len(), 1);
    let v = &versions[0];
    assert_eq!(v["name"], "v1alpha1");
    assert_eq!(v["served"], true);
    assert_eq!(v["storage"], true);
    assert!(
        v["subresources"]["status"].is_object(),
        "status subresource"
    );

    let columns: Vec<(&str, &str, Option<&str>)> = v["additionalPrinterColumns"]
        .as_array()
        .expect("printer columns")
        .iter()
        .map(|c| {
            (
                c["name"].as_str().expect("name"),
                c["jsonPath"].as_str().expect("jsonPath"),
                c["type"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        columns,
        vec![
            (
                "Ready",
                r#".status.conditions[?(@.type=="Ready")].status"#,
                Some("string")
            ),
            ("Kind", ".status.kind", Some("string")),
            ("Next Rotation", ".status.nextRotationTime", Some("date")),
            ("Age", ".metadata.creationTimestamp", Some("date")),
        ]
    );
}

fn check_bounded(path: &str, node: &Value, problems: &mut Vec<String>) {
    let Some(obj) = node.as_object() else { return };
    match obj.get("type").and_then(Value::as_str) {
        Some("string") if !obj.contains_key("maxLength") && !obj.contains_key("enum") => {
            problems.push(format!("{path}: string without maxLength/enum"));
        }
        Some("array") if !obj.contains_key("maxItems") => {
            problems.push(format!("{path}: array without maxItems"));
        }
        Some("object")
            if obj
                .get("additionalProperties")
                .is_some_and(Value::is_object)
                && !obj.contains_key("maxProperties") =>
        {
            problems.push(format!("{path}: map without maxProperties"));
        }
        _ => {}
    }
    if let Some(props) = obj.get("properties").and_then(Value::as_object) {
        for (name, child) in props {
            check_bounded(&format!("{path}.{name}"), child, problems);
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(child) = obj.get(key) {
            check_bounded(&format!("{path}.{key}"), child, problems);
        }
    }
}

#[test]
fn schema_is_bounded() {
    let mut problems = Vec::new();
    check_bounded("spec", &spec_schema(), &mut problems);
    assert!(
        problems.is_empty(),
        "unbounded schema nodes:\n{}",
        problems.join("\n")
    );
}

#[test]
fn schema_bounds_have_exact_values() {
    let spec = spec_schema();
    let p = |path: &[&str]| at(&spec, path).clone();

    assert_eq!(p(&["fields"])["maxItems"], 32);
    assert_eq!(p(&["fields"])["minItems"], 1);
    assert_eq!(p(&["data"])["maxProperties"], 64);
    assert_eq!(p(&["data"])["additionalProperties"]["maxLength"], 65536);
    for map in ["labels", "annotations"] {
        let m = p(&["secret", map]);
        assert_eq!(m["maxProperties"], 32, "{map}");
        assert_eq!(m["additionalProperties"]["maxLength"], 4096, "{map}");
    }
    assert_eq!(p(&["secret", "type"])["maxLength"], 253);

    // Secret key names: not `.` and not starting with `..` (as Kubernetes).
    let key_re = "^[.]?[-_a-zA-Z0-9][-._a-zA-Z0-9]*$";
    let name = p(&["fields", "items", "name"]);
    assert_eq!(
        (&name["maxLength"], &name["pattern"]),
        (&json!(253), &json!(key_re))
    );
    for (parent, keys) in [
        (vec!["sshKeyPair", "keys"], vec!["privateKey", "publicKey"]),
        (
            vec!["basicAuth", "keys"],
            vec!["username", "password", "htpasswd"],
        ),
    ] {
        for key in keys {
            let mut path = parent.clone();
            path.push(key);
            let k = p(&path);
            assert_eq!(k["maxLength"], 253, "{path:?}");
            assert_eq!(k["pattern"], key_re, "{path:?}");
        }
    }

    for (path, min, max) in [
        (vec!["fields", "items", "length"], 1, 1024),
        (vec!["fields", "items", "bytes"], 1, 1024),
        (vec!["basicAuth", "password", "length"], 1, 1024),
        (vec!["fields", "items", "require", "lower"], 0, 1024),
        (vec!["fields", "items", "require", "upper"], 0, 1024),
        (vec!["fields", "items", "require", "digits"], 0, 1024),
        (vec!["fields", "items", "require", "symbols"], 0, 1024),
        (vec!["basicAuth", "password", "require", "symbols"], 0, 1024),
    ] {
        let n = p(&path);
        // The typed CRD stores bounds as f64, so they serialise as `1.0`.
        assert_eq!(n["minimum"].as_f64(), Some(f64::from(min)), "{path:?}");
        assert_eq!(n["maximum"].as_f64(), Some(f64::from(max)), "{path:?}");
    }

    for (path, max, pattern) in [
        (vec!["fields", "items", "alphabet"], 256, "^[!-~]+$"),
        (vec!["fields", "items", "exclude"], 256, "^[ -~]*$"),
        (vec!["fields", "items", "prefix"], 64, "^[!-~]*$"),
        (vec!["fields", "items", "suffix"], 64, "^[!-~]*$"),
        (vec!["basicAuth", "password", "alphabet"], 256, "^[!-~]+$"),
        (vec!["basicAuth", "password", "exclude"], 256, "^[ -~]*$"),
        (vec!["basicAuth", "password", "prefix"], 64, "^[!-~]*$"),
        (vec!["basicAuth", "password", "suffix"], 64, "^[!-~]*$"),
        (vec!["sshKeyPair", "comment"], 256, "^[ -~]*$"),
        (vec!["rotation", "interval"], 8, "^[1-9][0-9]*(m|h|d)$"),
    ] {
        let n = p(&path);
        assert_eq!(
            (&n["maxLength"], &n["pattern"]),
            (&json!(max), &json!(pattern)),
            "{path:?}"
        );
    }

    let user = p(&["basicAuth", "username"]);
    assert_eq!(user["minLength"], 1);
    assert_eq!(user["maxLength"], 253);
    assert_eq!(user["pattern"], "^[!-9;-~]+$");
}

fn check_documented(path: &str, node: &Value, problems: &mut Vec<String>) {
    if node
        .get("description")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        problems.push(format!("{path}: missing description"));
    }
    if let Some(props) = node.get("properties").and_then(Value::as_object) {
        for (name, child) in props {
            check_documented(&format!("{path}.{name}"), child, problems);
        }
    }
    if let Some(items) = node.get("items") {
        check_documented(&format!("{path}[]"), items, problems);
    }
}

#[test]
fn every_property_is_documented_with_its_own_text() {
    let mut problems = Vec::new();
    check_documented("spec", &spec_schema(), &mut problems);
    check_documented(
        "status",
        &root_schema()["properties"]["status"],
        &mut problems,
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));

    // A type-level enum doc must not shadow the field doc.
    let spec = spec_schema();
    let desc = |path: &[&str]| {
        at(&spec, path)["description"]
            .as_str()
            .expect("description")
            .to_owned()
    };
    assert!(desc(&["fields", "items", "charset"]).contains("alphanumeric"));
    assert!(desc(&["sshKeyPair", "ecdsaCurve"]).contains("P-384"));
}

#[test]
fn schema_defaults_present_and_absent() {
    let spec = spec_schema();
    let default = |path: &[&str]| at(&spec, path).get("default").cloned();

    assert_eq!(default(&["deletionPolicy"]), Some(json!("Delete")));
    assert_eq!(default(&["secret"]), Some(json!({"type": "Opaque"})));
    assert_eq!(default(&["secret", "type"]), Some(json!("Opaque")));
    assert_eq!(
        default(&["sshKeyPair", "algorithm"]),
        Some(json!("ed25519"))
    );
    assert_eq!(
        default(&["sshKeyPair", "keys"]),
        Some(json!({"privateKey": "ssh-privatekey", "publicKey": "ssh-publickey"}))
    );
    assert_eq!(
        default(&["sshKeyPair", "keys", "privateKey"]),
        Some(json!("ssh-privatekey"))
    );
    assert_eq!(
        default(&["sshKeyPair", "keys", "publicKey"]),
        Some(json!("ssh-publickey"))
    );
    assert_eq!(
        default(&["basicAuth", "keys"]),
        Some(json!({"username": "username", "password": "password", "htpasswd": "auth"}))
    );
    assert_eq!(
        default(&["basicAuth", "keys", "username"]),
        Some(json!("username"))
    );
    assert_eq!(
        default(&["basicAuth", "keys", "password"]),
        Some(json!("password"))
    );
    assert_eq!(
        default(&["basicAuth", "keys", "htpasswd"]),
        Some(json!("auth"))
    );

    for path in [
        vec!["fields", "items", "length"],
        vec!["fields", "items", "charset"],
        vec!["fields", "items", "bytes"],
        vec!["fields", "items", "encoding"],
        vec!["sshKeyPair", "rsaBits"],
        vec!["sshKeyPair", "ecdsaCurve"],
        vec!["sshKeyPair", "comment"],
        vec!["basicAuth", "password", "length"],
        vec!["basicAuth", "password", "charset"],
        vec!["rotation"],
        vec!["fields"],
        vec!["sshKeyPair"],
        vec!["basicAuth"],
    ] {
        assert_eq!(
            default(&path),
            None,
            "{path:?} must not have a schema default"
        );
    }
}

#[test]
fn enums_and_required_are_exact() {
    let spec = spec_schema();
    let e = |path: &[&str]| at(&spec, path)["enum"].clone();
    assert_eq!(e(&["deletionPolicy"]), json!(["Delete", "Retain"]));
    assert_eq!(
        e(&["fields", "items", "charset"]),
        json!(["alphanumeric", "ascii", "hex", "custom"])
    );
    assert_eq!(
        e(&["fields", "items", "encoding"]),
        json!(["hex", "base64", "base64url", "base32"])
    );
    assert_eq!(
        e(&["sshKeyPair", "algorithm"]),
        json!(["ed25519", "rsa", "ecdsa"])
    );
    assert_eq!(e(&["sshKeyPair", "rsaBits"]), json!([2048, 3072, 4096]));
    assert_eq!(
        e(&["sshKeyPair", "ecdsaCurve"]),
        json!(["P-256", "P-384", "P-521"])
    );
    assert_eq!(at(&spec, &["sshKeyPair", "rsaBits"])["type"], "integer");

    // The API server rejects an explicit `null` on an `enum` property even when
    // it is `nullable` (verified against kind). Enum properties are therefore
    // non-nullable, so the server prunes `null` like an omitted property.
    for path in [
        vec!["fields", "items", "charset"],
        vec!["fields", "items", "encoding"],
        vec!["basicAuth", "password", "charset"],
        vec!["sshKeyPair", "rsaBits"],
        vec!["sshKeyPair", "ecdsaCurve"],
    ] {
        let prop = at(&spec, &path);
        assert!(
            prop.get("nullable").is_none(),
            "{path:?} must not be nullable"
        );
        assert!(
            !prop["enum"]
                .as_array()
                .expect("enum")
                .contains(&Value::Null),
            "{path:?}: enum must not list null"
        );
    }

    let status = &root_schema()["properties"]["status"]["properties"];
    assert_eq!(
        status["kind"]["enum"],
        json!(["Fields", "SSHKeyPair", "BasicAuth"])
    );

    // Every spec property is optional; the "exactly one kind" rule is CEL (Task 3).
    assert!(
        spec.get("required").is_none(),
        "spec must have no required properties"
    );
}

#[test]
fn fields_is_list_map_keyed_by_name() {
    let spec = spec_schema();
    let fields = at(&spec, &["fields"]);
    assert_eq!(fields["x-kubernetes-list-type"], "map");
    assert_eq!(fields["x-kubernetes-list-map-keys"], json!(["name"]));
    assert_eq!(at(&spec, &["fields", "items"])["required"], json!(["name"]));
    assert_eq!(at(&spec, &["basicAuth"])["required"], json!(["username"]));
}

#[test]
fn every_valid_fixture_deserializes() {
    let fixtures = common::load("valid");
    assert!(
        fixtures.len() >= 17,
        "expected the valid fixture set, got {}",
        fixtures.len()
    );
    for fixture in fixtures {
        let parsed: SecretSpec = serde_json::from_value(fixture.value.clone())
            .unwrap_or_else(|e| panic!("{}: {e}", fixture.file));
        // Round-trip stability: serde defaults are materialised on the way out,
        // so compare typed values rather than raw documents.
        let json = serde_json::to_value(&parsed).expect("serialise");
        let again: SecretSpec = serde_json::from_value(json.clone())
            .unwrap_or_else(|e| panic!("{}: re-parse: {e}", fixture.file));
        assert_eq!(
            parsed, again,
            "{}: round trip changed the value",
            fixture.file
        );
        assert_eq!(
            json,
            serde_json::to_value(&again).expect("serialise"),
            "{}",
            fixture.file
        );
    }
}

#[test]
fn unknown_spec_key_is_rejected() {
    let manifest = json!({
        "apiVersion": "secretspec.jabbas.eu/v1alpha1",
        "kind": "SecretSpec",
        "metadata": {"name": "x", "namespace": "test"},
        "spec": {"fields": [{"name": "p"}], "bogus": true}
    });
    assert!(serde_json::from_value::<SecretSpec>(manifest).is_err());
}

#[test]
fn wire_format_of_renamed_types() {
    use secretspec_api::v1alpha1::{EcdsaCurve, RsaBits, SecretKind};
    fn wire<T: serde::Serialize>(v: &T) -> Value {
        serde_json::to_value(v).expect("serialise")
    }
    assert_eq!(wire(&SecretKind::Fields), json!("Fields"));
    assert_eq!(wire(&SecretKind::SshKeyPair), json!("SSHKeyPair"));
    assert_eq!(wire(&SecretKind::BasicAuth), json!("BasicAuth"));
    assert_eq!(wire(&RsaBits::B3072), json!(3072));
    assert_eq!(wire(&EcdsaCurve::P521), json!("P-521"));
    assert!(serde_json::from_value::<RsaBits>(json!(1024)).is_err());
    assert!(serde_json::from_value::<EcdsaCurve>(json!("P-224")).is_err());
}

/// Every bound `resolve()` enforces must equal the one in the generated CRD.
/// The constants are the single source: the schema attributes use them, the
/// CEL rules embed them, and this test fails if the CRD ever differs (for
/// example because an attribute went back to a literal) or if a message that
/// quotes a number goes stale.
// A flat table of bounds; splitting it would only hide the list.
#[allow(clippy::too_many_lines)]
#[test]
fn schema_bounds_match_resolve_constants() {
    use secretspec_api::constants::*;
    use secretspec_api::resolve::{
        MSG_AFFIX_FORMAT, MSG_ALPHABET_FORMAT, MSG_COMMENT_FORMAT, MSG_DATA_MAX,
        MSG_DATA_VALUE_LENGTH, MSG_EXCLUDE_FORMAT, MSG_FIELDS_MAX, MSG_KEY_NAME_FORMAT,
        MSG_METADATA_MAX, MSG_METADATA_VALUE_LENGTH, MSG_RANDOM_RANGE, MSG_REQUIRE_RANGE,
        MSG_ROTATION_FORMAT, MSG_TYPE_LENGTH, MSG_USERNAME_FORMAT,
    };

    let spec = spec_schema();
    let node = |path: &[&str]| at(&spec, path).clone();
    let num = |v: &Value| v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"));
    let mut checked = 0;
    // (path, JSON keyword, constant, message that quotes the number, if any)
    let mut check = |path: &[&str], keyword: &str, expected: f64, message: Option<&str>| {
        let n = node(path);
        let actual = if let Some(rest) = keyword.strip_prefix("additionalProperties.") {
            num(&n["additionalProperties"][rest])
        } else {
            num(&n[keyword])
        };
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "{path:?}.{keyword}: CRD has {actual}, constant is {expected}"
        );
        if let Some(message) = message {
            assert!(
                message.contains(&format!("{expected}")),
                "{message:?} does not mention {expected}"
            );
        }
        checked += 1;
    };
    let f = |n: usize| u32::try_from(n).map(f64::from).expect("fits");

    check(&["fields"], "maxItems", f(MAX_FIELDS), Some(MSG_FIELDS_MAX));
    check(&["fields"], "minItems", f(MIN_FIELDS), None);
    check(
        &["data"],
        "maxProperties",
        f(MAX_DATA_ENTRIES),
        Some(MSG_DATA_MAX),
    );
    check(
        &["data"],
        "additionalProperties.maxLength",
        f(MAX_DATA_VALUE_LEN),
        Some(MSG_DATA_VALUE_LENGTH),
    );
    for map in ["labels", "annotations"] {
        check(
            &["secret", map],
            "maxProperties",
            f(MAX_METADATA_ENTRIES),
            Some(MSG_METADATA_MAX),
        );
        check(
            &["secret", map],
            "additionalProperties.maxLength",
            f(MAX_METADATA_VALUE_LEN),
            Some(MSG_METADATA_VALUE_LENGTH),
        );
    }
    check(
        &["secret", "type"],
        "maxLength",
        f(MAX_TYPE_LEN),
        Some(MSG_TYPE_LENGTH),
    );

    for key_path in [
        vec!["fields", "items", "name"],
        vec!["sshKeyPair", "keys", "privateKey"],
        vec!["sshKeyPair", "keys", "publicKey"],
        vec!["basicAuth", "keys", "username"],
        vec!["basicAuth", "keys", "password"],
        vec!["basicAuth", "keys", "htpasswd"],
    ] {
        check(
            &key_path,
            "maxLength",
            f(MAX_KEY_NAME_LEN),
            Some(MSG_KEY_NAME_FORMAT),
        );
        assert_eq!(node(&key_path)["pattern"], KEY_NAME_PATTERN, "{key_path:?}");
    }

    for parent in [vec!["fields", "items"], vec!["basicAuth", "password"]] {
        let sub = |name: &'static str| -> Vec<&'static str> {
            parent.iter().copied().chain([name]).collect()
        };
        check(
            &sub("length"),
            "minimum",
            f64::from(MIN_RANDOM_LENGTH),
            Some(MSG_RANDOM_RANGE),
        );
        check(
            &sub("length"),
            "maximum",
            f64::from(MAX_RANDOM_LENGTH),
            Some(MSG_RANDOM_RANGE),
        );
        check(
            &sub("alphabet"),
            "maxLength",
            f(MAX_ALPHABET_LEN),
            Some(MSG_ALPHABET_FORMAT),
        );
        check(
            &sub("exclude"),
            "maxLength",
            f(MAX_EXCLUDE_LEN),
            Some(MSG_EXCLUDE_FORMAT),
        );
        check(
            &sub("prefix"),
            "maxLength",
            f(MAX_AFFIX_LEN),
            Some(MSG_AFFIX_FORMAT),
        );
        check(
            &sub("suffix"),
            "maxLength",
            f(MAX_AFFIX_LEN),
            Some(MSG_AFFIX_FORMAT),
        );
        for class in ["lower", "upper", "digits", "symbols"] {
            let p = [sub("require"), vec![class]].concat();
            check(&p, "minimum", 0.0, Some(MSG_REQUIRE_RANGE));
            check(
                &p,
                "maximum",
                f64::from(MAX_REQUIRE_COUNT),
                Some(MSG_REQUIRE_RANGE),
            );
        }
    }
    check(
        &["fields", "items", "bytes"],
        "minimum",
        f64::from(MIN_RANDOM_LENGTH),
        None,
    );
    check(
        &["fields", "items", "bytes"],
        "maximum",
        f64::from(MAX_RANDOM_LENGTH),
        None,
    );

    check(
        &["basicAuth", "username"],
        "minLength",
        f(MIN_USERNAME_LEN),
        Some(MSG_USERNAME_FORMAT),
    );
    check(
        &["basicAuth", "username"],
        "maxLength",
        f(MAX_USERNAME_LEN),
        Some(MSG_USERNAME_FORMAT),
    );
    check(
        &["sshKeyPair", "comment"],
        "maxLength",
        f(MAX_COMMENT_LEN),
        Some(MSG_COMMENT_FORMAT),
    );
    check(
        &["rotation", "interval"],
        "maxLength",
        f(MAX_INTERVAL_LEN),
        Some(MSG_ROTATION_FORMAT),
    );
    assert_eq!(checked, 49, "number of bounds compared with the CRD");

    // The metadata key limit lives in a CEL rule, not in a schema keyword.
    let rule = at(&spec, &["secret", "labels"])["x-kubernetes-validations"][0]["rule"]
        .as_str()
        .expect("labels rule")
        .to_owned();
    assert!(
        rule.contains(&format!("size(k) <= {MAX_METADATA_KEY_LEN}")),
        "{rule}"
    );
    let rule = at(&spec, &["data"])["x-kubernetes-validations"][0]["rule"]
        .as_str()
        .expect("data rule")
        .to_owned();
    assert!(
        rule.contains(&format!("size(k) <= {MAX_KEY_NAME_LEN}")),
        "{rule}"
    );
    assert!(rule.contains(KEY_NAME_PATTERN), "{rule}");
}
