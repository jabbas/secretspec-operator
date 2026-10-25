// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask licenses`: collects the licence texts of the third-party
//! crates compiled into the operator, plus this project's own `LICENSE` and
//! `NOTICE`, into `target/dist/licenses/` (copied to `/licenses/` in the
//! image, see `build/Containerfile`).
//!
//! The crates are those the operator binary links for the release targets:
//! the normal-dependency closure of `secretspec-operator` from
//! `cargo metadata --filter-platform <target>`, for every target in
//! [`DIST_TARGETS`]. Dev- and build-only dependencies are not shipped and not
//! listed. For each crate the licence-like files from its source directory
//! (`LICENSE*`, `LICENCE*`, `COPYING*`, `NOTICE*`, `UNLICENSE*`,
//! `COPYRIGHT*`) are copied verbatim.
//!
//! Layout of the bundle:
//!
//! ```text
//! LICENSE, NOTICE                     this project
//! THIRD-PARTY-LICENSES.txt            index: crate, version, SPDX expression, files
//! third-party/<crate>-<version>/...   the crates' licence files
//! ```
//!
//! `cargo deny list` is not used for this: in cargo-deny 0.20 its output
//! formats (`human`, `json`, `tsv`) list licence identifiers per crate but do
//! not contain licence texts.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

use serde::Deserialize;

use crate::{BoxError, DIST_TARGETS, OPERATOR_BIN, workspace_root};

/// Bundle directory, relative to the workspace root.
const OUT_DIR: &str = "target/dist/licenses";
/// Name of the index file inside the bundle.
const INDEX_FILE: &str = "THIRD-PARTY-LICENSES.txt";

/// The parts of `cargo metadata` output used here.
#[derive(Debug, Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    resolve: Resolve,
}

#[derive(Debug, Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    license: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    repository: Option<String>,
    /// `None` for workspace members and path dependencies.
    source: Option<String>,
    manifest_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
struct Node {
    id: String,
    deps: Vec<NodeDep>,
}

#[derive(Debug, Deserialize)]
struct NodeDep {
    pkg: String,
    dep_kinds: Vec<DepKind>,
}

#[derive(Debug, Deserialize)]
struct DepKind {
    /// `None` is a normal dependency; `"build"` and `"dev"` are not linked.
    kind: Option<String>,
}

/// A third-party crate whose licence files are bundled.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Crate {
    name: String,
    version: String,
    license: String,
    authors: Vec<String>,
    repository: Option<String>,
    dir: PathBuf,
}

/// The standard MIT licence text (SPDX `MIT`) for `holders`.
fn mit_text(holders: &str) -> String {
    format!(
        "MIT License\n\nCopyright (c) {holders}\n\n\
Permission is hereby granted, free of charge, to any person obtaining a copy\n\
of this software and associated documentation files (the \"Software\"), to deal\n\
in the Software without restriction, including without limitation the rights\n\
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell\n\
copies of the Software, and to permit persons to whom the Software is\n\
furnished to do so, subject to the following conditions:\n\n\
The above copyright notice and this permission notice shall be included in all\n\
copies or substantial portions of the Software.\n\n\
THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR\n\
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,\n\
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE\n\
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER\n\
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,\n\
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE\n\
SOFTWARE.\n"
    )
}

/// Licence file for a crate whose published package ships none (the `kube`
/// crates and `cel`: their licence lives in the repository root only).
///
/// Only the two plain SPDX identifiers `Apache-2.0` and `MIT` are handled; for
/// those the canonical text is generated, headed by a note that says where the
/// information comes from. `apache_text` is the Apache-2.0 text (the
/// project's own `LICENSE`). Any other licence expression returns `None`, so
/// the command fails and a person looks at the crate.
fn fallback_license(krate: &Crate, apache_text: &str) -> Option<(&'static str, String)> {
    let mut note = format!(
        "NOTE: the published crate {}-{} ships no licence file. It declares the\n\
         licence `{}` in its Cargo.toml; the canonical text of that licence\n\
         follows.\n",
        krate.name, krate.version, krate.license
    );
    if let Some(repository) = &krate.repository {
        let _ = writeln!(note, "Source repository: {repository}");
    }
    match krate.license.as_str() {
        "Apache-2.0" => Some(("LICENSE-APACHE", format!("{note}\n{apache_text}"))),
        "MIT" => {
            let holders = if krate.authors.is_empty() {
                format!("the {} authors", krate.name)
            } else {
                krate.authors.join(", ")
            };
            let _ = writeln!(
                note,
                "Copyright holders as declared in `authors`: {holders}"
            );
            Some(("LICENSE-MIT", format!("{note}\n{}", mit_text(&holders))))
        }
        _ => None,
    }
}

/// Whether a file name looks like a licence, notice or copying file.
fn is_license_file(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    [
        "LICENSE",
        "LICENCE",
        "COPYING",
        "NOTICE",
        "UNLICENSE",
        "COPYRIGHT",
    ]
    .iter()
    .any(|prefix| upper.starts_with(prefix))
}

/// Third-party crates in the normal-dependency closure of the operator.
fn third_party_crates(metadata: &Metadata) -> Result<BTreeSet<Crate>, String> {
    let packages: HashMap<&str, &Package> = metadata
        .packages
        .iter()
        .map(|p| (p.id.as_str(), p))
        .collect();
    let nodes: HashMap<&str, &Node> = metadata
        .resolve
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();
    let root = metadata
        .packages
        .iter()
        .find(|p| p.name == OPERATOR_BIN && p.source.is_none())
        .ok_or_else(|| format!("package `{OPERATOR_BIN}` not found in cargo metadata"))?;

    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack = vec![root.id.as_str()];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let node = nodes
            .get(id)
            .ok_or_else(|| format!("`{id}` missing from the resolve graph"))?;
        for dep in &node.deps {
            if dep.dep_kinds.iter().any(|k| k.kind.is_none()) {
                stack.push(dep.pkg.as_str());
            }
        }
    }

    let mut crates = BTreeSet::new();
    for id in seen {
        let package = packages
            .get(id)
            .ok_or_else(|| format!("`{id}` missing from the package list"))?;
        if package.source.is_none() {
            continue; // workspace member: covered by the project's own LICENSE
        }
        let dir = package
            .manifest_path
            .parent()
            .ok_or_else(|| format!("bad manifest path for `{}`", package.name))?
            .to_path_buf();
        crates.insert(Crate {
            name: package.name.clone(),
            version: package.version.clone(),
            license: package
                .license
                .clone()
                .unwrap_or_else(|| "(not declared)".to_owned()),
            authors: package.authors.clone(),
            repository: package.repository.clone(),
            dir,
        });
    }
    Ok(crates)
}

/// Runs `cargo metadata` for one target triple.
fn metadata_for(cargo: &std::ffi::OsStr, root: &Path, target: &str) -> Result<Metadata, BoxError> {
    let output = Command::new(cargo)
        .current_dir(root)
        .args(["metadata", "--locked", "--format-version", "1"])
        .args(["--filter-platform", target])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed for {target}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Licence-like files directly inside `dir`, sorted by name.
fn license_files(dir: &Path) -> Result<Vec<PathBuf>, BoxError> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_file() && is_license_file(&name.to_string_lossy()) {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

pub fn licenses() -> Result<ExitCode, BoxError> {
    let root = workspace_root()?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let mut crates = BTreeSet::new();
    for (target, _) in DIST_TARGETS {
        crates.extend(third_party_crates(&metadata_for(&cargo, &root, target)?)?);
    }

    let out = root.join(OUT_DIR);
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(out.join("third-party"))?;
    for own in ["LICENSE", "NOTICE"] {
        fs::copy(root.join(own), out.join(own))?;
    }
    let apache_text = fs::read_to_string(root.join("LICENSE"))?;

    let mut index = String::new();
    index.push_str("Third-party crates compiled into secretspec-operator.\n");
    index.push_str("Generated by \"cargo xtask licenses\" from Cargo.lock.\n\n");
    let mut missing: BTreeMap<String, String> = BTreeMap::new();
    let mut generated = 0_usize;
    for krate in &crates {
        let files = license_files(&krate.dir)?;
        let label = format!("{}-{}", krate.name, krate.version);
        let dest = out.join("third-party").join(&label);
        let mut names: Vec<String> = files
            .iter()
            .filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        if files.is_empty() {
            if let Some((name, text)) = fallback_license(krate, &apache_text) {
                fs::create_dir_all(&dest)?;
                fs::write(dest.join(name), text)?;
                names.push(format!("{name} (generated, the crate ships none)"));
                generated += 1;
            } else {
                missing.insert(label.clone(), krate.license.clone());
            }
        } else {
            fs::create_dir_all(&dest)?;
            for file in &files {
                if let Some(name) = file.file_name() {
                    fs::copy(file, dest.join(name))?;
                }
            }
        }
        writeln!(
            index,
            "{label}\n  license: {}\n  files: {}",
            krate.license,
            if names.is_empty() {
                "(none)".to_owned()
            } else {
                names.join(", ")
            }
        )?;
    }
    fs::write(out.join(INDEX_FILE), index)?;

    println!(
        "wrote {OUT_DIR}: LICENSE, NOTICE and {} third-party crates ({generated} with a generated text)",
        crates.len(),
    );
    if !missing.is_empty() {
        eprintln!("error: these crates ship no licence file and have no supported fallback:");
        for (label, license) in &missing {
            eprintln!("  {label} ({license})");
        }
        eprintln!("Review each one, then extend `fallback_license` in xtask/src/licenses.rs.");
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(id: &str, name: &str, source: Option<&str>) -> Package {
        Package {
            id: id.to_owned(),
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            license: Some("MIT".to_owned()),
            authors: vec![],
            repository: None,
            source: source.map(str::to_owned),
            manifest_path: PathBuf::from(format!("/src/{name}/Cargo.toml")),
        }
    }

    fn node(id: &str, deps: &[(&str, Option<&str>)]) -> Node {
        Node {
            id: id.to_owned(),
            deps: deps
                .iter()
                .map(|(pkg, kind)| NodeDep {
                    pkg: (*pkg).to_owned(),
                    dep_kinds: vec![DepKind {
                        kind: kind.map(str::to_owned),
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn matches_license_like_file_names() {
        for name in [
            "LICENSE",
            "LICENSE-MIT",
            "license-apache.txt",
            "LICENCE.md",
            "COPYING",
            "NOTICE",
            "UNLICENSE",
            "COPYRIGHT",
        ] {
            assert!(is_license_file(name), "{name}");
        }
        for name in ["Cargo.toml", "README.md", "src", "build.rs", "CHANGELOG.md"] {
            assert!(!is_license_file(name), "{name}");
        }
    }

    #[test]
    fn closure_follows_normal_edges_only_and_skips_workspace_members() {
        let metadata = Metadata {
            packages: vec![
                package("op", OPERATOR_BIN, None),
                package("api", "secretspec-api", None),
                package("a", "a", Some("registry+x")),
                package("b", "b", Some("registry+x")),
                package("buildonly", "buildonly", Some("registry+x")),
                package("devonly", "devonly", Some("registry+x")),
                package("unrelated", "unrelated", Some("registry+x")),
            ],
            resolve: Resolve {
                nodes: vec![
                    node(
                        "op",
                        &[
                            ("api", None),
                            ("a", None),
                            ("buildonly", Some("build")),
                            ("devonly", Some("dev")),
                        ],
                    ),
                    node("api", &[("b", None)]),
                    node("a", &[("b", None)]),
                    node("b", &[]),
                    node("buildonly", &[]),
                    node("devonly", &[]),
                    node("unrelated", &[]),
                ],
            },
        };
        let names: Vec<String> = third_party_crates(&metadata)
            .expect("closure")
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["a", "b"]);
    }

    fn krate(license: &str) -> Crate {
        Crate {
            name: "demo".to_owned(),
            version: "2.0.0".to_owned(),
            license: license.to_owned(),
            authors: vec!["A <a@example.com>".to_owned(), "B".to_owned()],
            repository: Some("https://example.com/demo".to_owned()),
            dir: PathBuf::from("/src/demo"),
        }
    }

    #[test]
    fn fallback_covers_plain_mit_and_apache_only() {
        let (name, text) = fallback_license(&krate("MIT"), "APACHE TEXT").expect("mit");
        assert_eq!(name, "LICENSE-MIT");
        assert!(text.contains("ships no licence file"));
        assert!(text.contains("https://example.com/demo"));
        assert!(text.contains("Copyright (c) A <a@example.com>, B"));
        assert!(text.contains("Permission is hereby granted"));

        let (name, text) = fallback_license(&krate("Apache-2.0"), "APACHE TEXT").expect("apache");
        assert_eq!(name, "LICENSE-APACHE");
        assert!(text.ends_with("APACHE TEXT"));

        assert!(fallback_license(&krate("MIT OR Apache-2.0"), "x").is_none());
        assert!(fallback_license(&krate("(not declared)"), "x").is_none());
    }

    #[test]
    fn missing_operator_package_is_an_error() {
        let metadata = Metadata {
            packages: vec![package("a", "a", Some("registry+x"))],
            resolve: Resolve { nodes: vec![] },
        };
        assert!(third_party_crates(&metadata).is_err());
    }
}
