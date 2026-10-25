// SPDX-License-Identifier: Apache-2.0

//! `cargo xtask licenses`: collects the licence texts of the third-party
//! crates linked into the operator, plus this project's own `LICENSE` and
//! `NOTICE`, into `target/dist/licenses/` (copied to `/licenses/` in the
//! image, see `build/Containerfile`).
//!
//! The crates are those `cargo tree -p secretspec-operator --edges normal
//! --target <triple>` lists, for every target in [`DIST_TARGETS`]. `cargo
//! tree -p` resolves features for that package alone, so features that only
//! dev-dependencies of other workspace members switch on do not leak in, and
//! dev- and build-only dependencies are left out. (Proc-macro crates are
//! normal dependencies for cargo and stay in, conservatively.) The licence
//! files are then looked up in the source directory of each crate, found
//! through `cargo metadata`.
//!
//! Per crate the licence files (`LICENSE*`, `LICENCE*`, `COPYING*`,
//! `NOTICE*`, `UNLICENSE*`, `COPYRIGHT*`, and every file in a `LICENSES/`,
//! `LICENSE/` or `licenses/` subdirectory) are copied verbatim. The command
//! fails, and leaves no bundle behind, when for any crate
//!
//! - there is no licence file other than a `NOTICE`, and no fallback applies, or
//! - the SPDX expression in its `Cargo.toml` is not satisfied by the files
//!   (each `AND` operand needs a file, one `OR` alternative is enough; a file
//!   matches by name or by content), or the expression has an identifier this
//!   tool does not know.
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

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs, io};

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
}

#[derive(Debug, Deserialize)]
struct Package {
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

impl Crate {
    /// `<name>-<version>`, the directory name in the bundle.
    fn label(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// Parses `cargo tree --prefix none` output into `(name, version)` pairs.
///
/// Lines look like `name v1.2.3`, optionally followed by `(proc-macro)`,
/// `(*)` or a `(path)` / `(source)` suffix, which are ignored here.
fn parse_tree(output: &str) -> Result<BTreeSet<(String, String)>, String> {
    let mut found = BTreeSet::new();
    for line in output.lines().filter(|l| !l.trim().is_empty()) {
        let mut words = line.split_whitespace();
        let (Some(name), Some(version)) = (words.next(), words.next()) else {
            return Err(format!("unexpected `cargo tree` line `{line}`"));
        };
        let Some(version) = version.strip_prefix('v') else {
            return Err(format!("unexpected `cargo tree` line `{line}`"));
        };
        found.insert((name.to_owned(), version.to_owned()));
    }
    Ok(found)
}

/// The third-party crates named by the `cargo tree` outputs, looked up in the
/// `cargo metadata` package list. Workspace members (no `source`) are skipped.
fn select_crates(
    trees: &[String],
    packages: &[Package],
) -> Result<BTreeMap<String, Crate>, String> {
    let mut wanted = BTreeSet::new();
    for tree in trees {
        wanted.extend(parse_tree(tree)?);
    }
    let mut crates = BTreeMap::new();
    for (name, version) in wanted {
        let matches: Vec<&Package> = packages
            .iter()
            .filter(|p| p.name == name && p.version == version)
            .collect();
        let external: Vec<&&Package> = matches.iter().filter(|p| p.source.is_some()).collect();
        let package = match (matches.as_slice(), external.as_slice()) {
            ([], _) => return Err(format!("{name} {version} not found in cargo metadata")),
            (_, []) => continue, // workspace member: covered by the project's LICENSE
            (_, [only]) => **only,
            (_, _) => {
                return Err(format!(
                    "{name} {version} is ambiguous: several sources in cargo metadata"
                ));
            }
        };
        let dir = package
            .manifest_path
            .parent()
            .ok_or_else(|| format!("bad manifest path for `{name}`"))?
            .to_path_buf();
        let krate = Crate {
            name: package.name.clone(),
            version: package.version.clone(),
            license: package
                .license
                .clone()
                .unwrap_or_else(|| "(not declared)".to_owned()),
            authors: package.authors.clone(),
            repository: package.repository.clone(),
            dir,
        };
        crates.insert(krate.label(), krate);
    }
    Ok(crates)
}

// ---------------------------------------------------------------- licence files

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

/// Whether a directory name marks a directory of licence texts.
fn is_license_dir(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "licenses" | "licences" | "license" | "licence"
    )
}

/// A `NOTICE` carries attributions, not a licence text.
fn is_notice(rel: &Path) -> bool {
    rel.file_name().is_some_and(|n| {
        n.to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("NOTICE")
    })
}

/// A licence file of a crate: `rel` is its path below the crate directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LicenseFile {
    rel: PathBuf,
    path: PathBuf,
}

/// Licence files of the crate at `dir`: matching files directly in it, and
/// every file below a licence directory. Sorted by relative path.
fn license_files(dir: &Path) -> io::Result<Vec<LicenseFile>> {
    fn walk(
        root: &Path,
        rel: &Path,
        in_license_dir: bool,
        out: &mut Vec<LicenseFile>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(root.join(rel))? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let child = rel.join(&name);
            let kind = entry.file_type()?;
            if kind.is_file() && (in_license_dir || is_license_file(&name_str)) {
                out.push(LicenseFile {
                    path: entry.path(),
                    rel: child,
                });
            } else if kind.is_dir()
                && (in_license_dir || (rel.as_os_str().is_empty() && is_license_dir(&name_str)))
            {
                walk(root, &child, true, out)?;
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, Path::new(""), false, &mut files)?;
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(files)
}

// ------------------------------------------------------------ SPDX expressions

/// How to recognise the licence text of an SPDX identifier.
struct Hints {
    /// A file whose name (split at non-alphanumerics, lower case) has one of
    /// these words.
    names: &'static [&'static str],
    /// A file whose content (lower case, whitespace collapsed) has one of
    /// these phrases.
    content: &'static [&'static str],
}

/// The recognition hints of `id`, or `None` for an unknown identifier.
fn hints(id: &str) -> Option<Hints> {
    let (names, content): (&[&str], &[&str]) = match id.trim_end_matches('+') {
        "Apache-2.0" => (&["apache"], &["apache license"]),
        "MIT" | "MIT-0" => (&["mit"], &["permission is hereby granted"]),
        "BSD-2-Clause" | "BSD-3-Clause" => (
            &["bsd"],
            &["redistribution and use in source and binary forms"],
        ),
        "0BSD" => (&["0bsd", "bsd"], &["permission to use, copy, modify"]),
        "ISC" => (&["isc"], &["permission to use, copy, modify"]),
        "Unlicense" => (&["unlicense"], &["unencumbered"]),
        "Zlib" => (
            &["zlib"],
            &["altered source versions must be plainly marked"],
        ),
        "Unicode-3.0" | "Unicode-DFS-2016" => (&["unicode"], &["unicode"]),
        "CC0-1.0" => (&["cc0"], &["cc0"]),
        "CDLA-Permissive-2.0" => (&["cdla"], &["cdla-permissive"]),
        "MPL-2.0" => (&["mpl"], &["mozilla public license"]),
        "BSL-1.0" => (&["bsl", "boost"], &["boost software license"]),
        _ => return None,
    };
    Some(Hints { names, content })
}

/// A licence file with its content, for matching.
struct Candidate {
    name_words: Vec<String>,
    content: String,
}

impl Candidate {
    fn new(file_name: &str, content: &[u8]) -> Self {
        let stem = file_name.to_ascii_lowercase();
        Self {
            name_words: stem
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(str::to_owned)
                .collect(),
            content: String::from_utf8_lossy(content)
                .to_ascii_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    fn matches(&self, hints: &Hints) -> bool {
        hints
            .names
            .iter()
            .any(|w| self.name_words.iter().any(|n| n == w))
            || hints.content.iter().any(|c| self.content.contains(c))
    }
}

/// SPDX expression tokens: `(`, `)` and words.
fn tokenize(expression: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for c in expression.chars() {
        match c {
            '(' | ')' => {
                if !word.is_empty() {
                    tokens.push(std::mem::take(&mut word));
                }
                tokens.push(c.to_string());
            }
            // Legacy crates.io syntax: `MIT/Apache-2.0` means `MIT OR Apache-2.0`.
            '/' => {
                if !word.is_empty() {
                    tokens.push(std::mem::take(&mut word));
                }
                tokens.push("OR".to_owned());
            }
            c if c.is_whitespace() => {
                if !word.is_empty() {
                    tokens.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

/// Evaluates `expression` (`AND` binds tighter than `OR`; `WITH <exception>`
/// is ignored) with `has` telling whether a licence text for an identifier is
/// present. Unknown identifiers and malformed expressions are errors.
fn satisfied(expression: &str, has: &dyn Fn(&Hints) -> bool) -> Result<bool, String> {
    struct Parser<'a> {
        tokens: Vec<String>,
        pos: usize,
        has: &'a dyn Fn(&Hints) -> bool,
    }
    impl Parser<'_> {
        fn peek(&self) -> Option<&str> {
            self.tokens.get(self.pos).map(String::as_str)
        }
        fn or(&mut self) -> Result<bool, String> {
            let mut result = self.and()?;
            while self.peek() == Some("OR") {
                self.pos += 1;
                let next = self.and()?;
                result |= next;
            }
            Ok(result)
        }
        fn and(&mut self) -> Result<bool, String> {
            let mut result = self.term()?;
            while self.peek() == Some("AND") {
                self.pos += 1;
                let next = self.term()?;
                result &= next;
            }
            Ok(result)
        }
        fn term(&mut self) -> Result<bool, String> {
            let Some(token) = self.peek().map(str::to_owned) else {
                return Err("expression ends early".to_owned());
            };
            self.pos += 1;
            if token == "(" {
                let inner = self.or()?;
                if self.peek() != Some(")") {
                    return Err("missing `)`".to_owned());
                }
                self.pos += 1;
                return Ok(inner);
            }
            let hints = hints(&token)
                .ok_or_else(|| format!("unrecognised licence identifier `{token}`"))?;
            if self.peek() == Some("WITH") {
                self.pos += 2; // WITH <exception>
            }
            Ok((self.has)(&hints))
        }
    }
    let mut parser = Parser {
        tokens: tokenize(expression),
        pos: 0,
        has,
    };
    let result = parser.or()?;
    if parser.pos != parser.tokens.len() {
        return Err(format!("unexpected `{}`", parser.tokens[parser.pos]));
    }
    Ok(result)
}

/// Checks that `files` (all licence files of a crate) cover `expression`.
fn check_coverage(expression: &str, files: &[LicenseFile]) -> Result<(), String> {
    let mut candidates = Vec::new();
    for file in files.iter().filter(|f| !is_notice(&f.rel)) {
        let content = fs::read(&file.path).map_err(|e| format!("{}: {e}", file.path.display()))?;
        let name = file
            .rel
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        candidates.push(Candidate::new(&name, &content));
    }
    let has = |hints: &Hints| candidates.iter().any(|c| c.matches(hints));
    if satisfied(expression, &has)? {
        Ok(())
    } else {
        Err(format!(
            "no licence file matches the declared expression `{expression}`"
        ))
    }
}

// ------------------------------------------------------------------ fallbacks

/// The standard MIT licence text (SPDX `MIT`). The copyright line is a
/// placeholder: the holders are not known from the crate's package.
fn mit_text() -> String {
    "MIT License\n\n\
Copyright (c) the copyright holders of the crate (not shipped with it; see the\n\
note at the top of this file)\n\n\
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
        .to_owned()
}

/// Licence file for a crate whose published package ships no licence text (the
/// `kube` crates: their licence lives in the repository root only).
///
/// Only the two plain SPDX identifiers `Apache-2.0` and `MIT` are handled; for
/// those the canonical text is generated, headed by a note that says where the
/// information comes from. The declared authors appear only in that note,
/// never as a copyright line. `apache_text` is the Apache-2.0 text (the
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
    if !krate.authors.is_empty() {
        let _ = writeln!(
            note,
            "Authors declared in Cargo.toml: {}",
            krate.authors.join(", ")
        );
    }
    match krate.license.as_str() {
        "Apache-2.0" => Some(("LICENSE-APACHE", format!("{note}\n{apache_text}"))),
        "MIT" => Some(("LICENSE-MIT", format!("{note}\n{}", mit_text()))),
        _ => None,
    }
}

// --------------------------------------------------------------------- bundle

/// What [`write_bundle`] did.
#[derive(Debug, Default, PartialEq, Eq)]
struct Bundle {
    /// Crates in the bundle.
    crates: usize,
    /// Of those, crates with a generated fallback text.
    generated: Vec<String>,
    /// One line per crate that cannot be bundled. Non-empty means no bundle
    /// was left on disk.
    problems: Vec<String>,
}

/// Writes the bundle into `out` (replacing it). On any problem, or I/O error,
/// `out` is removed again, so a partial bundle is never left to be shipped.
fn write_bundle(
    project: &Path,
    out: &Path,
    crates: &BTreeMap<String, Crate>,
) -> Result<Bundle, BoxError> {
    let result = write_bundle_inner(project, out, crates);
    if !matches!(&result, Ok(bundle) if bundle.problems.is_empty()) {
        match fs::remove_dir_all(out) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err.into()),
            _ => {}
        }
    }
    result
}

fn write_bundle_inner(
    project: &Path,
    out: &Path,
    crates: &BTreeMap<String, Crate>,
) -> Result<Bundle, BoxError> {
    match fs::remove_dir_all(out) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err.into()),
        _ => {}
    }
    fs::create_dir_all(out.join("third-party"))?;
    for own in ["LICENSE", "NOTICE"] {
        fs::copy(project.join(own), out.join(own))?;
    }
    let apache_text = fs::read_to_string(project.join("LICENSE"))?;

    let mut bundle = Bundle {
        crates: crates.len(),
        ..Bundle::default()
    };
    let mut index = String::new();
    index.push_str("Third-party crates secretspec-operator is built from: its normal\n");
    index.push_str(
        "dependencies for the release targets (Cargo.lock), proc-macro crates included.\n",
    );
    index.push_str("Generated by \"cargo xtask licenses\".\n\n");
    for (label, krate) in crates {
        let files = license_files(&krate.dir)?;
        let dest = out.join("third-party").join(label);
        let mut names: Vec<String> = files
            .iter()
            .map(|f| f.rel.to_string_lossy().into_owned())
            .collect();
        let has_text = files.iter().any(|f| !is_notice(&f.rel));
        let mut copy = true;
        if has_text {
            if let Err(why) = check_coverage(&krate.license, &files) {
                bundle
                    .problems
                    .push(format!("{label} ({}): {why}", krate.license));
                copy = false;
            }
        } else if let Some((name, text)) = fallback_license(krate, &apache_text) {
            fs::create_dir_all(&dest)?;
            fs::write(dest.join(name), text)?;
            names.push(format!("{name} (generated, the crate ships none)"));
            bundle.generated.push(label.clone());
        } else {
            bundle.problems.push(format!(
                "{label} ({}): no licence file (other than a NOTICE) and no fallback",
                krate.license
            ));
            copy = false;
        }
        if copy {
            for file in &files {
                let target = dest.join(&file.rel);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&file.path, target)?;
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
    Ok(bundle)
}

// -------------------------------------------------------------------- command

/// `cargo tree` output for the operator and one target.
fn tree_for(cargo: &std::ffi::OsStr, root: &Path, target: &str) -> Result<String, BoxError> {
    let output = Command::new(cargo)
        .current_dir(root)
        .args(["tree", "--locked", "--package", OPERATOR_BIN])
        .args(["--edges", "normal", "--prefix", "none", "--target", target])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "`cargo tree` failed for {target}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn metadata(cargo: &std::ffi::OsStr, root: &Path) -> Result<Metadata, BoxError> {
    let output = Command::new(cargo)
        .current_dir(root)
        .args(["metadata", "--locked", "--format-version", "1"])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "`cargo metadata` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

pub fn licenses() -> Result<ExitCode, BoxError> {
    let root = workspace_root()?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    let mut trees = Vec::new();
    for (target, _) in DIST_TARGETS {
        trees.push(tree_for(&cargo, &root, target)?);
    }
    let crates = select_crates(&trees, &metadata(&cargo, &root)?.packages)?;

    let bundle = write_bundle(&root, &root.join(OUT_DIR), &crates)?;
    if !bundle.problems.is_empty() {
        eprintln!("error: cannot bundle the licences of these crates (no bundle was written):");
        for problem in &bundle.problems {
            eprintln!("  {problem}");
        }
        eprintln!(
            "Review each one, then extend `fallback_license` or `hints` in xtask/src/licenses.rs."
        );
        return Ok(ExitCode::FAILURE);
    }
    println!(
        "wrote {OUT_DIR}: LICENSE, NOTICE and {} third-party crates ({} with a generated text: {})",
        bundle.crates,
        bundle.generated.len(),
        bundle.generated.join(", ")
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scratch directory below the system temp dir, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = env::temp_dir().join(format!(
                "xtask-licenses-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }

        fn write(&self, rel: &str, content: &str) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(&path, content).expect("write");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const MIT_BODY: &str = "Permission is hereby granted, free of charge, to any person";
    const APACHE_BODY: &str = "Apache License\nVersion 2.0, January 2004";

    fn krate(dir: &Path, name: &str, license: &str) -> Crate {
        Crate {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            license: license.to_owned(),
            authors: vec!["A <a@example.com>".to_owned(), "B".to_owned()],
            repository: Some("https://example.com/demo".to_owned()),
            dir: dir.to_path_buf(),
        }
    }

    fn crates_of(list: Vec<Crate>) -> BTreeMap<String, Crate> {
        list.into_iter().map(|c| (c.label(), c)).collect()
    }

    /// A project root with LICENSE and NOTICE, and an `out` path below it.
    fn project(scratch: &Scratch) -> (PathBuf, PathBuf) {
        scratch.write("project/LICENSE", "PROJECT LICENSE\n");
        scratch.write("project/NOTICE", "PROJECT NOTICE\n");
        (scratch.0.join("project"), scratch.0.join("out"))
    }

    // -- cargo tree / metadata

    fn package(name: &str, version: &str, source: Option<&str>) -> Package {
        Package {
            name: name.to_owned(),
            version: version.to_owned(),
            license: Some("MIT".to_owned()),
            authors: vec![],
            repository: None,
            source: source.map(str::to_owned),
            manifest_path: PathBuf::from(format!("/src/{name}-{version}/Cargo.toml")),
        }
    }

    #[test]
    fn parses_cargo_tree_lines() {
        let found = parse_tree(
            "secretspec-operator v0.1.0 (/work/crates/secretspec-operator)\n\
             serde v1.0.229\n\
             serde_derive v1.0.229 (proc-macro)\n\
             serde v1.0.229 (*)\n\n",
        )
        .expect("parse");
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["secretspec-operator", "serde", "serde_derive"]);
        assert!(parse_tree("garbage").is_err());
        assert!(parse_tree("name 1.0.0").is_err());
    }

    #[test]
    fn selection_unions_targets_and_skips_workspace_members() {
        let packages = vec![
            package("secretspec-operator", "0.1.0", None),
            package("a", "1.0.0", Some("registry+x")),
            package("b", "2.0.0", Some("registry+x")),
            package("dev-only", "1.0.0", Some("registry+x")),
        ];
        let trees = vec![
            "secretspec-operator v0.1.0 (/w)\na v1.0.0\n".to_owned(),
            "secretspec-operator v0.1.0 (/w)\na v1.0.0\nb v2.0.0\n".to_owned(),
        ];
        let selected = select_crates(&trees, &packages).expect("select");
        assert_eq!(
            selected.keys().map(String::as_str).collect::<Vec<_>>(),
            ["a-1.0.0", "b-2.0.0"]
        );
    }

    #[test]
    fn selection_rejects_unknown_and_ambiguous_crates() {
        let unknown = select_crates(&["nope v1.0.0\n".to_owned()], &[]);
        assert!(unknown.is_err());

        let mut twin = package("a", "1.0.0", Some("git+x"));
        twin.manifest_path = PathBuf::from("/other/a/Cargo.toml");
        let packages = vec![package("a", "1.0.0", Some("registry+x")), twin];
        let err = select_crates(&["a v1.0.0\n".to_owned()], &packages).expect_err("ambiguous");
        assert!(err.contains("ambiguous"), "{err}");
    }

    // -- licence files and expressions

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
        assert!(is_license_dir("LICENSES"));
        assert!(is_license_dir("licenses"));
        assert!(!is_license_dir("src"));
    }

    #[test]
    fn finds_files_in_license_subdirectories_only() {
        let s = Scratch::new();
        s.write("c/LICENSE-MIT", MIT_BODY);
        s.write("c/LICENSES/Apache-2.0.txt", APACHE_BODY);
        s.write("c/LICENSES/deep/MIT.txt", MIT_BODY);
        s.write("c/src/LICENSE", "not a crate licence");
        s.write("c/README.md", "readme");
        let rels: Vec<String> = license_files(&s.0.join("c"))
            .expect("walk")
            .iter()
            .map(|f| f.rel.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rels,
            [
                "LICENSE-MIT",
                "LICENSES/Apache-2.0.txt",
                "LICENSES/deep/MIT.txt"
            ]
        );
    }

    #[test]
    fn expressions_need_a_text_per_and_operand_and_one_per_or() {
        let only_mit = |h: &Hints| h.names.contains(&"mit");
        assert_eq!(satisfied("MIT", &only_mit), Ok(true));
        assert_eq!(satisfied("MIT OR Apache-2.0", &only_mit), Ok(true));
        // Legacy crates.io syntax for OR.
        assert_eq!(satisfied("MIT/Apache-2.0", &only_mit), Ok(true));
        assert_eq!(satisfied("Unlicense/Apache-2.0", &only_mit), Ok(false));
        assert_eq!(satisfied("MIT AND Apache-2.0", &only_mit), Ok(false));
        assert_eq!(satisfied("Apache-2.0", &only_mit), Ok(false));
        assert_eq!(
            satisfied("(MIT OR Apache-2.0) AND Unlicense", &only_mit),
            Ok(false)
        );
        assert_eq!(
            satisfied("MIT AND (Apache-2.0 OR MIT)", &only_mit),
            Ok(true)
        );
        assert_eq!(
            satisfied("Apache-2.0 WITH LLVM-exception OR MIT", &only_mit),
            Ok(true)
        );
        assert!(satisfied("MIT OR Frobnicate-1.0", &only_mit).is_err());
        assert!(satisfied("(MIT", &only_mit).is_err());
        assert!(satisfied("MIT OR", &only_mit).is_err());
        assert!(satisfied("", &only_mit).is_err());
    }

    #[test]
    fn coverage_matches_by_name_or_content_and_ignores_notices() {
        let s = Scratch::new();
        // MIT by file name; Apache by content under a generic name.
        s.write("c/LICENSE-MIT", "short");
        s.write("c/LICENSE", APACHE_BODY);
        s.write("c/NOTICE", "MIT Apache attributions");
        let files = license_files(&s.0.join("c")).expect("walk");
        assert_eq!(check_coverage("MIT AND Apache-2.0", &files), Ok(()));
        assert!(check_coverage("MIT AND Unlicense", &files).is_err());

        // A NOTICE alone never satisfies an expression.
        let s = Scratch::new();
        s.write("c/NOTICE", "Permission is hereby granted; Apache License");
        let files = license_files(&s.0.join("c")).expect("walk");
        assert!(check_coverage("MIT", &files).is_err());
    }

    // -- fallbacks

    #[test]
    fn fallback_covers_plain_mit_and_apache_only() {
        let dir = Path::new("/src/demo");
        let (name, text) =
            fallback_license(&krate(dir, "demo", "MIT"), "APACHE TEXT").expect("mit");
        assert_eq!(name, "LICENSE-MIT");
        assert!(text.contains("ships no licence file"));
        assert!(text.contains("https://example.com/demo"));
        assert!(text.contains("Authors declared in Cargo.toml: A <a@example.com>, B"));
        assert!(text.contains("Permission is hereby granted"));
        // The authors are in the note only, never in a copyright line.
        assert!(!text.contains("Copyright (c) A"));
        assert_eq!(text.matches("a@example.com").count(), 1);

        let (name, text) =
            fallback_license(&krate(dir, "demo", "Apache-2.0"), "APACHE TEXT").expect("apache");
        assert_eq!(name, "LICENSE-APACHE");
        assert!(text.ends_with("APACHE TEXT"));

        assert!(fallback_license(&krate(dir, "demo", "MIT OR Apache-2.0"), "x").is_none());
        assert!(fallback_license(&krate(dir, "demo", "(not declared)"), "x").is_none());
    }

    // -- the bundle

    #[test]
    fn bundle_copies_files_subdirectories_and_generates_fallbacks() {
        let s = Scratch::new();
        let (project, out) = project(&s);
        s.write("a/LICENSE-MIT", MIT_BODY);
        s.write("a/LICENSE-APACHE", APACHE_BODY);
        s.write("a/NOTICE", "notice a");
        s.write("b/LICENSES/MIT.txt", MIT_BODY);
        s.write("bare/Cargo.toml", "");
        let crates = crates_of(vec![
            krate(&s.0.join("a"), "a", "MIT OR Apache-2.0"),
            krate(&s.0.join("b"), "b", "MIT"),
            krate(&s.0.join("bare"), "bare", "Apache-2.0"),
        ]);

        let bundle = write_bundle(&project, &out, &crates).expect("bundle");
        assert_eq!(bundle.problems, Vec::<String>::new());
        assert_eq!(bundle.crates, 3);
        assert_eq!(bundle.generated, ["bare-1.0.0"]);

        let read = |rel: &str| fs::read_to_string(out.join(rel)).expect(rel);
        assert_eq!(read("LICENSE"), "PROJECT LICENSE\n");
        assert_eq!(read("NOTICE"), "PROJECT NOTICE\n");
        assert_eq!(read("third-party/a-1.0.0/LICENSE-MIT"), MIT_BODY);
        assert_eq!(read("third-party/a-1.0.0/NOTICE"), "notice a");
        assert_eq!(read("third-party/b-1.0.0/LICENSES/MIT.txt"), MIT_BODY);
        assert!(read("third-party/bare-1.0.0/LICENSE-APACHE").contains("PROJECT LICENSE"));
        let index = read(INDEX_FILE);
        assert!(index.contains(
            "a-1.0.0\n  license: MIT OR Apache-2.0\n  files: LICENSE-APACHE, LICENSE-MIT, NOTICE"
        ));
        assert!(index.contains("LICENSES/MIT.txt"));
        assert!(index.contains("LICENSE-APACHE (generated, the crate ships none)"));
    }

    #[test]
    fn bundle_replaces_a_previous_one() {
        let s = Scratch::new();
        let (project, out) = project(&s);
        s.write("out/third-party/stale-9.9.9/LICENSE", "stale");
        s.write("a/LICENSE", MIT_BODY);
        let crates = crates_of(vec![krate(&s.0.join("a"), "a", "MIT")]);
        write_bundle(&project, &out, &crates).expect("bundle");
        assert!(!out.join("third-party/stale-9.9.9").exists());
        assert!(out.join("third-party/a-1.0.0/LICENSE").exists());
    }

    #[test]
    fn bundle_fails_and_leaves_nothing_for_notice_only_and_partial_sets() {
        let s = Scratch::new();
        let (project, out) = project(&s);
        s.write("notice_only/NOTICE", "attributions");
        s.write("partial/LICENSE-MIT", MIT_BODY);
        s.write("good/LICENSE", MIT_BODY);
        let crates = crates_of(vec![
            krate(&s.0.join("good"), "good", "MIT"),
            // Only a NOTICE and an expression that has no fallback.
            krate(&s.0.join("notice_only"), "notice_only", "MIT OR Apache-2.0"),
            // Has a text, but not for both operands.
            krate(&s.0.join("partial"), "partial", "MIT AND Apache-2.0"),
        ]);

        let bundle = write_bundle(&project, &out, &crates).expect("bundle");
        assert_eq!(bundle.problems.len(), 2, "{:?}", bundle.problems);
        assert!(
            bundle
                .problems
                .iter()
                .any(|p| p.starts_with("notice_only-1.0.0"))
        );
        assert!(
            bundle
                .problems
                .iter()
                .any(|p| p.starts_with("partial-1.0.0"))
        );
        assert!(!out.exists(), "a failed run must not leave a bundle");
    }

    #[test]
    fn bundle_fails_for_an_unrecognised_expression() {
        let s = Scratch::new();
        let (project, out) = project(&s);
        s.write("odd/LICENSE", MIT_BODY);
        let crates = crates_of(vec![krate(&s.0.join("odd"), "odd", "Frobnicate-1.0")]);
        let bundle = write_bundle(&project, &out, &crates).expect("bundle");
        assert_eq!(bundle.problems.len(), 1);
        assert!(bundle.problems[0].contains("unrecognised licence identifier"));
        assert!(!out.exists());
    }

    #[test]
    fn bundle_removes_the_output_on_io_errors() {
        let s = Scratch::new();
        let (project, out) = project(&s);
        // The crate directory does not exist: reading it fails midway.
        let crates = crates_of(vec![krate(&s.0.join("missing"), "missing", "MIT")]);
        assert!(write_bundle(&project, &out, &crates).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn crates_with_the_same_name_and_version_share_one_entry() {
        // The bundle is keyed by `<name>-<version>`: the same crate found
        // through several targets is written once, never twice.
        let s = Scratch::new();
        let (project, out) = project(&s);
        s.write("a/LICENSE", MIT_BODY);
        let one = krate(&s.0.join("a"), "a", "MIT");
        let crates = crates_of(vec![one.clone(), one]);
        assert_eq!(crates.len(), 1);
        let bundle = write_bundle(&project, &out, &crates).expect("bundle");
        assert_eq!(bundle.crates, 1);
        assert_eq!(
            fs::read_dir(out.join("third-party")).expect("dir").count(),
            1
        );
    }
}
