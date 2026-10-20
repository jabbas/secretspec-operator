// SPDX-License-Identifier: Apache-2.0

//! Shared fixture loader for the integration tests.
//!
//! Fixtures are YAML manifests under `tests/fixtures/<dir>/`. Optional header
//! comments steer the tests that consume them:
//!
//! ```yaml
//! # expect: <text the rejection message must contain>
//! # layer: cel|schema   (default: cel)
//! # resolve-expect: <text resolve()'s error must contain; schema layer only>
//! # server-expect: <text the API server's message must contain; schema layer only>
//! ```
//!
//! For schema-layer fixtures `# expect:` is the offending JSON path (the API
//! server and `resolve()` both report it) and `# server-expect:` pins the
//! server's own wording for the server-side suite, falling back to `# expect:`.
//!
//! Loading is strict: a fixture with a key the generated CRD does not declare
//! fails the test ([`unknown_keys`]), because the production types ignore
//! unknown fields.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)] // each test binary uses a different subset

use std::path::PathBuf;

/// Which validation layer is expected to reject an invalid fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layer {
    Cel,
    Schema,
}

#[derive(Clone, Debug)]
pub struct Fixture {
    /// File name, e.g. `v01-minimal-fields.yaml`.
    pub file: String,
    /// Text the rejection message must contain (`# expect:` header).
    pub expect: Option<String>,
    /// Text `resolve()`'s error must contain (`# resolve-expect:` header). Only
    /// needed for schema-layer fixtures whose `# expect:` is not a JSON path.
    pub resolve_expect: Option<String>,
    /// Text the API server's message must contain (`# server-expect:` header);
    /// schema layer only, where the server words the rejection itself.
    pub server_expect: Option<String>,
    /// Layer expected to reject the fixture (`# layer:` header).
    pub layer: Layer,
    /// The parsed manifest.
    pub value: serde_json::Value,
}

/// Every key of `manifest` that the generated CRD schema does not declare, as
/// a JSON path. The production types ignore unknown fields (see the comment in
/// `v1alpha1/mod.rs`), so this is what keeps a fixture typo from silently
/// testing the default instead of the intended value.
pub fn unknown_keys(manifest: &serde_json::Value) -> Vec<String> {
    use kube::CustomResourceExt;
    use serde_json::Value;

    fn walk(schema: &Value, value: &Value, path: &str, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                let props = schema.get("properties").and_then(Value::as_object);
                let additional = schema.get("additionalProperties").filter(|a| a.is_object());
                for (key, child) in map {
                    let child_path = format!("{path}.{key}");
                    if let Some(child_schema) = props.and_then(|p| p.get(key)) {
                        walk(child_schema, child, &child_path, out);
                    } else if let Some(child_schema) = additional {
                        walk(child_schema, child, &child_path, out);
                    } else {
                        out.push(child_path);
                    }
                }
            }
            Value::Array(items) => {
                if let Some(item_schema) = schema.get("items") {
                    for (i, item) in items.iter().enumerate() {
                        walk(item_schema, item, &format!("{path}[{i}]"), out);
                    }
                }
            }
            _ => {}
        }
    }

    let crd =
        serde_json::to_value(secretspec_api::v1alpha1::SecretSpec::crd()).expect("crd serialises");
    let root = &crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"];
    let mut out = Vec::new();
    let Value::Object(map) = manifest else {
        return vec!["<root is not an object>".to_owned()];
    };
    for (key, child) in map {
        match key.as_str() {
            // Validated by the API server, not part of the CRD schema.
            "apiVersion" | "kind" | "metadata" => {}
            _ => match root["properties"].get(key) {
                Some(schema) => walk(schema, child, key, &mut out),
                None => out.push(key.clone()),
            },
        }
    }
    out
}

/// Loads every `*.yaml` fixture in `tests/fixtures/<dir>/`, sorted by file name.
///
/// # Panics
/// Panics (test failure) on unreadable directories, unparsable YAML, or an
/// unknown `# layer:` value.
pub fn load(dir: &str) -> Vec<Fixture> {
    load_from(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(dir),
    )
}

/// The repository's top-level `examples/` directory.
pub fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

/// Loads every copy-paste example in `examples/` like a valid fixture. The
/// examples carry no `metadata.namespace` (users apply them with `-n`).
pub fn load_examples() -> Vec<Fixture> {
    let examples = load_from(&examples_dir());
    assert!(!examples.is_empty(), "no examples found");
    examples
}

fn load_from(path: &std::path::Path) -> Vec<Fixture> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let text = std::fs::read_to_string(&file).expect("read fixture");
            let header = |key: &str| {
                text.lines()
                    .take_while(|l| l.starts_with('#'))
                    .find_map(|l| l.strip_prefix(&format!("# {key}:")))
                    .map(|v| v.trim().to_owned())
            };
            let layer = match header("layer").as_deref() {
                None | Some("cel") => Layer::Cel,
                Some("schema") => Layer::Schema,
                Some(other) => panic!("{}: unknown layer {other:?}", file.display()),
            };
            let value = serde_norway::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: invalid YAML: {e}", file.display()));
            let unknown = unknown_keys(&value);
            assert!(
                unknown.is_empty(),
                "{}: keys unknown to the CRD (typo?): {unknown:?}",
                file.display()
            );
            Fixture {
                file: file
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned(),
                expect: header("expect"),
                resolve_expect: header("resolve-expect"),
                server_expect: header("server-expect"),
                layer,
                value,
            }
        })
        .collect()
}
