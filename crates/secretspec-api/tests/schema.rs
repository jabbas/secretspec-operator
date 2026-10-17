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
    assert_eq!(p(&["data"])["maxProperties"], 64);
    assert_eq!(p(&["data"])["additionalProperties"]["maxLength"], 65536);
    for map in ["labels", "annotations"] {
        let m = p(&["secret", map]);
        assert_eq!(m["maxProperties"], 32, "{map}");
        assert_eq!(m["additionalProperties"]["maxLength"], 4096, "{map}");
    }
    assert_eq!(p(&["secret", "type"])["maxLength"], 253);

    let key_re = "^[-._a-zA-Z0-9]+$";
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
