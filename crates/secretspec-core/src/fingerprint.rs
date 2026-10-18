// SPDX-License-Identifier: Apache-2.0

//! The frozen v1 fingerprint format.
//!
//! A *slot* is a stable logical name for one generated value (`field/<name>`,
//! `ssh/keypair`, ...). Its fingerprint is the SHA-256 of a canonical text of
//! the slot's fully resolved generation parameters. The operator regenerates a
//! value exactly when its stored fingerprint differs from the expected one.
//!
//! **The canonical text is frozen forever.** Changing any byte of it changes
//! the fingerprints of existing Secrets and so rotates every credential in
//! every cluster. The normative description is `docs/fingerprints.md`; the
//! golden tests in `tests/fingerprint_golden.rs` pin the hashes.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use hmac::{Hmac, KeyInit, Mac};
use secretspec_api::resolve::{
    BytesParams, CharsParams, Generator, ResolvedKind, ResolvedSpec, SshAlgorithmParams,
};
use secretspec_api::v1alpha1::{EcdsaCurve, Encoding};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::value::SecretValue;

/// First line of every canonical text; the format version.
const HEADER: &str = "secretspec-fingerprint-v1";
/// Maximum length of a Secret key name (and so of a `field/<name>` slot name).
const MAX_KEY_NAME_LEN: usize = 253;

/// A generation slot: the unit a fingerprint is stored and compared for.
///
/// Slots do not depend on `keys.*`, so renaming a Secret key moves a value
/// without regenerating it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    /// `field/<name>`: one `fields[]` entry.
    Field(String),
    /// `ssh/keypair`: the SSH key pair.
    SshKeyPair,
    /// `basic-auth/password`: the `basicAuth` password.
    BasicAuthPassword,
    /// `basic-auth/htpasswd`: the `basicAuth` htpasswd line.
    BasicAuthHtpasswd,
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field(name) => write!(f, "field/{name}"),
            Self::SshKeyPair => f.write_str("ssh/keypair"),
            Self::BasicAuthPassword => f.write_str("basic-auth/password"),
            Self::BasicAuthHtpasswd => f.write_str("basic-auth/htpasswd"),
        }
    }
}

impl FromStr for Slot {
    type Err = FingerprintParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ssh/keypair" => Ok(Self::SshKeyPair),
            "basic-auth/password" => Ok(Self::BasicAuthPassword),
            "basic-auth/htpasswd" => Ok(Self::BasicAuthHtpasswd),
            _ => match s.strip_prefix("field/") {
                Some(name) if is_key_name(name) => Ok(Self::Field(name.to_owned())),
                _ => Err(FingerprintParseError::InvalidSlot(s.to_owned())),
            },
        }
    }
}

/// A Secret key name as the API validates it: `[-._a-zA-Z0-9]+`, 1-253
/// characters, not `.`, not starting with `..`.
///
/// Mirrors `secretspec_api::resolve::is_key_name`, which is private to the API
/// crate; a test in `tests/fingerprint_golden.rs` checks both agree.
fn is_key_name(s: &str) -> bool {
    (1..=MAX_KEY_NAME_LEN).contains(&s.len())
        && s != "."
        && !s.starts_with("..")
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A SHA-256 fingerprint: exactly 64 lowercase hex characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint(String);

impl Fingerprint {
    /// Parses a fingerprint.
    ///
    /// # Errors
    /// [`FingerprintParseError::InvalidFingerprint`] unless `s` is exactly 64
    /// lowercase hex characters.
    pub fn parse(s: &str) -> Result<Self, FingerprintParseError> {
        if s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            Ok(Self(s.to_owned()))
        } else {
            Err(FingerprintParseError::InvalidFingerprint(s.to_owned()))
        }
    }

    /// The 64 lowercase hex characters.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A fingerprint annotation, a slot or a fingerprint could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FingerprintParseError {
    /// The annotation is not JSON or not a JSON object.
    #[error("fingerprints annotation is not a JSON object: {0}")]
    Json(String),
    /// Not one of the known slot forms, or the field name is not a valid key name.
    #[error("unknown fingerprint slot {0:?}")]
    InvalidSlot(String),
    /// Not 64 lowercase hex characters (or not a string at all).
    #[error("invalid fingerprint {0:?}: expected 64 lowercase hex characters")]
    InvalidFingerprint(String),
    /// An entry with an unknown key whose value is not a string; it cannot be
    /// preserved and is dropped.
    #[error("dropped unknown fingerprints entry {0:?}: value is not a string")]
    InvalidUnknownEntry(String),
}

/// Canonical text of a characters-mode slot (`field/<name>` or
/// `basic-auth/password`).
pub fn canonical_chars(slot: &Slot, p: &CharsParams) -> String {
    format!(
        "{HEADER}\nslot={slot}\nmode=chars\nlength={}\nalphabet={}\nrequire={},{},{},{}\nprefix={}\nsuffix={}\n",
        p.length,
        hex::encode(&p.alphabet),
        p.require.lower,
        p.require.upper,
        p.require.digits,
        p.require.symbols,
        hex::encode(p.prefix.as_bytes()),
        hex::encode(p.suffix.as_bytes()),
    )
}

/// Canonical text of a random-bytes slot (`field/<name>`).
pub fn canonical_bytes(slot: &Slot, p: &BytesParams) -> String {
    format!(
        "{HEADER}\nslot={slot}\nmode=bytes\ncount={}\nencoding={}\nprefix={}\nsuffix={}\n",
        p.count,
        encoding_wire(p.encoding),
        hex::encode(p.prefix.as_bytes()),
        hex::encode(p.suffix.as_bytes()),
    )
}

/// Canonical text of the `ssh/keypair` slot.
pub fn canonical_ssh(p: &SshAlgorithmParams) -> String {
    let algorithm = match p {
        SshAlgorithmParams::Ed25519 => "algorithm=ed25519\n".to_owned(),
        SshAlgorithmParams::Rsa { bits } => format!("algorithm=rsa\nbits={bits}\n"),
        SshAlgorithmParams::Ecdsa { curve } => {
            format!("algorithm=ecdsa\ncurve={}\n", curve_wire(*curve))
        }
    };
    format!("{HEADER}\nslot={}\n{algorithm}", Slot::SshKeyPair)
}

/// Canonical text of the `basic-auth/htpasswd` slot. Unlike the others it
/// depends on generated values (the password and the stored htpasswd line), so
/// it cannot be computed before generation.
///
/// `htpasswd_line` is the stored line `<username>:<bcrypt hash>` exactly as it
/// is in the Secret data (no trailing newline). The password enters only as
/// `HMAC-SHA256(key = htpasswd_line, message = password)`: the annotation is
/// readable by people who cannot read Secret data, and without the
/// salt-bearing line the value cannot be used to test password guesses.
pub fn canonical_htpasswd(
    username: &str,
    password: &SecretValue,
    htpasswd_line: &SecretValue,
    cost: u32,
) -> String {
    format!(
        "{HEADER}\nslot={}\nusername={}\npassword-mac={}\ncost={cost}\n",
        Slot::BasicAuthHtpasswd,
        hex::encode(username.as_bytes()),
        hex::encode(password_mac(htpasswd_line.expose(), password.expose())),
    )
}

/// `HMAC-SHA256(key, message)`.
fn password_mac(key: &[u8], message: &[u8]) -> Vec<u8> {
    // HMAC accepts keys of any length (longer keys are hashed first), so
    // `new_from_slice` cannot fail for `Hmac<Sha256>`.
    let Ok(mut mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(key) else {
        unreachable!("HMAC-SHA256 accepts keys of any length")
    };
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// SHA-256 of the canonical text, as 64 lowercase hex characters.
pub fn fingerprint_of(canonical: &str) -> Fingerprint {
    Fingerprint(hex::encode(Sha256::digest(canonical.as_bytes())))
}

/// The fingerprints a Secret generated from `spec` must carry. Excludes
/// `basic-auth/htpasswd`, which depends on the generated password.
pub fn expected_fingerprints(spec: &ResolvedSpec) -> BTreeMap<Slot, Fingerprint> {
    let mut out = BTreeMap::new();
    match &spec.kind {
        ResolvedKind::Fields(fields) => {
            for field in fields {
                let slot = Slot::Field(field.name.clone());
                let canonical = match &field.generator {
                    Generator::Chars(p) => canonical_chars(&slot, p),
                    Generator::Bytes(p) => canonical_bytes(&slot, p),
                };
                out.insert(slot, fingerprint_of(&canonical));
            }
        }
        ResolvedKind::SshKeyPair(ssh) => {
            out.insert(
                Slot::SshKeyPair,
                fingerprint_of(&canonical_ssh(&ssh.algorithm)),
            );
        }
        ResolvedKind::BasicAuth(basic) => {
            out.insert(
                Slot::BasicAuthPassword,
                fingerprint_of(&canonical_chars(&Slot::BasicAuthPassword, &basic.password)),
            );
        }
    }
    out
}

/// The API wire string of an encoding. Written out here (not derived from
/// serde) so a rename in the API crate cannot silently change the format.
fn encoding_wire(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Hex => "hex",
        Encoding::Base64 => "base64",
        Encoding::Base64url => "base64url",
        Encoding::Base32 => "base32",
    }
}

/// The API wire string of a curve; see [`encoding_wire`].
fn curve_wire(curve: EcdsaCurve) -> &'static str {
    match curve {
        EcdsaCurve::P256 => "P-256",
        EcdsaCurve::P384 => "P-384",
        EcdsaCurve::P521 => "P-521",
    }
}

/// The content of the `secretspec.jabbas.eu/fingerprints` annotation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FingerprintMap {
    /// Fingerprints of the slots this version knows.
    pub slots: BTreeMap<Slot, Fingerprint>,
    /// Entries whose key is not a slot this version knows (for example one
    /// written by a newer operator), kept verbatim so that a writer carries
    /// them through instead of dropping them.
    pub unknown: BTreeMap<String, String>,
}

/// The result of reading a fingerprints annotation that is a JSON object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedAnnotation {
    /// Every usable entry: valid known slots and preserved unknown ones.
    pub map: FingerprintMap,
    /// One entry per known slot whose value was not a valid fingerprint (that
    /// slot is absent from `map`), and per unknown key whose value was not a
    /// string (dropped).
    pub problems: Vec<FingerprintParseError>,
}

impl FingerprintMap {
    /// Compact JSON object `{"<slot>":"<hex>",...}` of the known slots and the
    /// preserved unknown entries, keys sorted by their string form. A known
    /// slot wins over an unknown entry with the same key.
    pub fn to_annotation(&self) -> String {
        let mut sorted: BTreeMap<String, &str> = self
            .unknown
            .iter()
            .map(|(key, value)| (key.clone(), value.as_str()))
            .collect();
        sorted.extend(
            self.slots
                .iter()
                .map(|(slot, fingerprint)| (slot.to_string(), fingerprint.as_str())),
        );
        // A map of strings always serializes; the fallback is unreachable.
        serde_json::to_string(&sorted).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Reads an annotation tolerantly (normative rules in
    /// `docs/fingerprints.md`): a key that is not a known slot is kept in
    /// [`FingerprintMap::unknown`]; a known slot whose value is not a valid
    /// fingerprint is left out and reported in [`ParsedAnnotation::problems`].
    /// One bad entry never invalidates the others.
    ///
    /// # Errors
    /// [`FingerprintParseError::Json`] only if the text is not JSON or not a
    /// JSON object.
    pub fn from_annotation(s: &str) -> Result<ParsedAnnotation, FingerprintParseError> {
        let raw: serde_json::Map<String, Value> =
            serde_json::from_str(s).map_err(|e| FingerprintParseError::Json(e.to_string()))?;
        let mut parsed = ParsedAnnotation::default();
        for (key, value) in raw {
            match (key.parse::<Slot>(), value) {
                (Ok(slot), Value::String(text)) => match Fingerprint::parse(&text) {
                    Ok(fingerprint) => {
                        parsed.map.slots.insert(slot, fingerprint);
                    }
                    Err(e) => parsed.problems.push(e),
                },
                (Ok(_), other) => parsed
                    .problems
                    .push(FingerprintParseError::InvalidFingerprint(other.to_string())),
                (Err(_), Value::String(text)) => {
                    parsed.map.unknown.insert(key, text);
                }
                (Err(_), _) => parsed
                    .problems
                    .push(FingerprintParseError::InvalidUnknownEntry(key)),
            }
        }
        Ok(parsed)
    }
}
