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

/// Loads every `*.yaml` fixture in `tests/fixtures/<dir>/`, sorted by file name.
///
/// # Panics
/// Panics (test failure) on unreadable directories, unparsable YAML, or an
/// unknown `# layer:` value.
pub fn load(dir: &str) -> Vec<Fixture> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&path)
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
