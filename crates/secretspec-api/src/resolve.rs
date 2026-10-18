// SPDX-License-Identifier: Apache-2.0

//! Defaults resolution and runtime validation of a [`SecretSpecSpec`].
//!
//! [`resolve`] has two jobs:
//!
//! 1. It is the operator's last line of defence against invalid stored
//!    objects: it re-checks every schema rule and every CEL rule of the CRD
//!    (objects created before a rule existed, or with validation bypassed, can
//!    still be in etcd) plus the rules that cannot be expressed in the CRD
//!    (the effective alphabet after `exclude`).
//! 2. It produces the fully resolved generation parameters. Fingerprints are
//!    computed from these, never from the raw spec, so every default below is
//!    frozen for `v1alpha1`: changing one would rotate credentials. See the
//!    design document, sections 4.2 to 4.5.
//!
//! All violations are collected, not just the first. Each is reported as
//! `<json path>: <message>`. CEL rules reuse the `MSG_*` constants of
//! [`crate::constants`]; schema-layer rules use the `MSG_*` constants of this
//! module (the API server words those rejections itself).
//!
//! `secret.type` immutability is a CEL *transition* rule: it compares with the
//! previous object, which `resolve` does not have, so it is not checked here.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::constants::{
    DEFAULT_ECDSA_CURVE, DEFAULT_LENGTH, DEFAULT_RSA_BITS, LABEL_MANAGED_BY, MAX_RANDOM_LENGTH,
    MSG_ALPHABET_WITH_CUSTOM, MSG_BASIC_AUTH_TYPE, MSG_BYTES_EXCLUSIVE, MSG_CURVE_ONLY_ECDSA,
    MSG_DATA_COLLISION, MSG_DATA_KEY_FORMAT, MSG_ENCODING_WITH_BYTES, MSG_EXACTLY_ONE_KIND,
    MSG_KEYS_DISTINCT, MSG_REQUIRE_SUM, MSG_RESERVED_METADATA, MSG_ROTATION_RANGE,
    MSG_RSA_BITS_ONLY_RSA, MSG_SSH_AUTH_TYPE, RESERVED_PREFIX,
};
use crate::v1alpha1::{
    BasicAuthPassword, BasicAuthSpec, Charset, DeletionPolicy, EcdsaCurve, Encoding, FieldSpec,
    RequireSpec, RotationSpec, SecretKind, SecretSpecSpec, SshAlgorithm, SshKeyPairSpec,
};

// Schema-layer rejections. `resolve` words them itself; the text follows the
// path: `<json path>: <message>`.

/// `spec.fields`: at most 32 entries.
pub const MSG_FIELDS_MAX: &str = "must have at most 32 entries";
/// `spec.fields[].name`: unique list-map key.
pub const MSG_FIELD_NAME_DUPLICATE: &str = "must be unique within fields";
/// Secret key names (`fields[].name`, `keys.*`): `^[-._a-zA-Z0-9]+$`, at most 253.
pub const MSG_KEY_NAME_FORMAT: &str = "must match ^[-._a-zA-Z0-9]+$ and be at most 253 characters";
/// `spec.data`: at most 64 entries.
pub const MSG_DATA_MAX: &str = "must have at most 64 entries";
/// `spec.data` values: at most 65536 characters.
pub const MSG_DATA_VALUE_LENGTH: &str = "value must be at most 65536 characters";
/// `spec.secret.labels` and `annotations`: at most 32 entries.
pub const MSG_METADATA_MAX: &str = "must have at most 32 entries";
/// `spec.secret.labels` and `annotations` values: at most 4096 characters.
pub const MSG_METADATA_VALUE_LENGTH: &str = "value must be at most 4096 characters";
/// `spec.secret.type`: at most 253 characters.
pub const MSG_TYPE_LENGTH: &str = "must be at most 253 characters";
/// `length` and `bytes`: 1-1024.
pub const MSG_RANDOM_RANGE: &str = "must be between 1 and 1024";
/// `require.<class>`: 0-1024.
pub const MSG_REQUIRE_RANGE: &str = "must be between 0 and 1024";
/// `alphabet`: `^[!-~]+$`, at most 256.
pub const MSG_ALPHABET_FORMAT: &str = "must be 1-256 printable ASCII characters without spaces";
/// `exclude`: `^[ -~]*$`, at most 256.
pub const MSG_EXCLUDE_FORMAT: &str = "must be at most 256 printable ASCII characters";
/// `prefix` and `suffix`: `^[!-~]*$`, at most 64.
pub const MSG_AFFIX_FORMAT: &str = "must be at most 64 printable ASCII characters without spaces";
/// `rotation.interval`: `^[1-9][0-9]*(m|h|d)$`, at most 8 characters.
pub const MSG_ROTATION_FORMAT: &str =
    "must be a positive integer followed by m, h or d (at most 8 characters)";
/// `sshKeyPair.comment`: `^[ -~]*$`, at most 256.
pub const MSG_COMMENT_FORMAT: &str = "must be at most 256 printable ASCII characters";
/// `basicAuth.username`: `^[!-9;-~]+$`, at most 253.
pub const MSG_USERNAME_FORMAT: &str =
    "must be 1-253 visible ASCII characters without ':' and spaces";

// Runtime-only rules (not expressible in the CRD).

/// The effective alphabet is too small to draw from.
pub const MSG_ALPHABET_TOO_SMALL: &str =
    "alphabet must contain at least 2 characters after exclude";
/// A required class is empty in the effective alphabet; `{class}` is replaced.
pub const MSG_CLASS_EMPTY: &str = "no {class} characters left in the alphabet";

const BASIC_AUTH_TYPE: &str = "kubernetes.io/basic-auth";
const SSH_AUTH_TYPE: &str = "kubernetes.io/ssh-auth";
const MAX_KEY_LEN: usize = 253;
/// `rotation.interval` bounds: 1h ..= 3650d.
const MIN_ROTATION_SECS: u64 = 3_600;
const MAX_ROTATION_SECS: u64 = 3_650 * 86_400;

/// The runtime validation failed; every entry is `<json path>: <message>`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}", .errors.join("; "))]
pub struct InvalidSpec {
    /// One entry per violated rule.
    pub errors: Vec<String>,
}

/// A `SecretSpecSpec` with every default applied and every rule checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSpec {
    /// What to generate.
    pub kind: ResolvedKind,
    /// Literal values copied verbatim into the Secret.
    pub data: BTreeMap<String, String>,
    /// What happens to the Secret when the `SecretSpec` is deleted.
    pub deletion_policy: DeletionPolicy,
    /// Rotation interval, if scheduled rotation is enabled.
    pub rotation: Option<Duration>,
    /// Kubernetes Secret type.
    pub secret_type: String,
    /// User labels for the Secret (without the operator's own).
    pub labels: BTreeMap<String, String>,
    /// User annotations for the Secret (without the operator's own).
    pub annotations: BTreeMap<String, String>,
}

/// The one generator kind of a spec.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedKind {
    /// `spec.fields`.
    Fields(Vec<ResolvedField>),
    /// `spec.sshKeyPair`.
    SshKeyPair(ResolvedSsh),
    /// `spec.basicAuth`.
    BasicAuth(ResolvedBasicAuth),
}

/// One resolved `fields[]` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedField {
    /// The Secret key.
    pub name: String,
    /// How the value is generated.
    pub generator: Generator,
}

/// How a string value is generated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Generator {
    /// Random characters from an alphabet.
    Chars(CharsParams),
    /// Random bytes in an encoding.
    Bytes(BytesParams),
}

/// Fully resolved characters-mode parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharsParams {
    /// Number of random characters (without prefix/suffix).
    pub length: u16,
    /// The effective alphabet: sorted ascending, unique, after `exclude`.
    pub alphabet: Vec<u8>,
    /// Minimum number of characters per class.
    pub require: Require,
    /// Text prepended verbatim.
    pub prefix: String,
    /// Text appended verbatim.
    pub suffix: String,
}

/// Minimum number of characters per class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Require {
    /// Lower-case letters.
    pub lower: u16,
    /// Upper-case letters.
    pub upper: u16,
    /// Digits.
    pub digits: u16,
    /// ASCII punctuation.
    pub symbols: u16,
}

/// Fully resolved random-bytes parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BytesParams {
    /// Number of random bytes.
    pub count: u16,
    /// How the bytes are rendered.
    pub encoding: Encoding,
    /// Text prepended verbatim.
    pub prefix: String,
    /// Text appended verbatim.
    pub suffix: String,
}

/// Resolved SSH key algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshAlgorithmParams {
    /// Ed25519.
    Ed25519,
    /// RSA with the given modulus size.
    Rsa {
        /// 2048, 3072 or 4096.
        bits: u16,
    },
    /// ECDSA on the given curve.
    Ecdsa {
        /// The curve.
        curve: EcdsaCurve,
    },
}

/// Resolved `sshKeyPair`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSsh {
    /// Algorithm with its parameters.
    pub algorithm: SshAlgorithmParams,
    /// Public key comment (may be empty).
    pub comment: String,
    /// Secret key of the private key.
    pub private_key_key: String,
    /// Secret key of the public key.
    pub public_key_key: String,
}

/// Resolved `basicAuth`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedBasicAuth {
    /// The user name.
    pub username: String,
    /// Password generation parameters.
    pub password: CharsParams,
    /// Secret key of the user name.
    pub username_key: String,
    /// Secret key of the password.
    pub password_key: String,
    /// Secret key of the htpasswd line.
    pub htpasswd_key: String,
}

impl ResolvedSpec {
    /// Which kind of secret this spec produces.
    pub fn secret_kind(&self) -> SecretKind {
        match self.kind {
            ResolvedKind::Fields(_) => SecretKind::Fields,
            ResolvedKind::SshKeyPair(_) => SecretKind::SshKeyPair,
            ResolvedKind::BasicAuth(_) => SecretKind::BasicAuth,
        }
    }

    /// Estimated entropy in bits per generated key: `length * log2(|alphabet|)`
    /// in characters mode (alphabet after `exclude`), `bytes * 8` in bytes
    /// mode. `basicAuth` reports its password key. SSH keys are asymmetric and
    /// not estimated (empty). Prefix and suffix add nothing.
    pub fn entropy_bits(&self) -> Vec<(String, f64)> {
        match &self.kind {
            ResolvedKind::Fields(fields) => fields
                .iter()
                .map(|field| {
                    let bits = match &field.generator {
                        Generator::Chars(chars) => chars_entropy(chars),
                        Generator::Bytes(bytes) => f64::from(bytes.count) * 8.0,
                    };
                    (field.name.clone(), bits)
                })
                .collect(),
            ResolvedKind::SshKeyPair(_) => Vec::new(),
            ResolvedKind::BasicAuth(basic) => {
                vec![(basic.password_key.clone(), chars_entropy(&basic.password))]
            }
        }
    }
}

fn chars_entropy(chars: &CharsParams) -> f64 {
    // An alphabet has at most 256 distinct bytes, so the conversion is exact.
    let size = u16::try_from(chars.alphabet.len()).unwrap_or(u16::MAX);
    f64::from(chars.length) * f64::from(size).log2()
}

/// Applies every default and checks every rule.
///
/// `name` and `namespace` are those of the `SecretSpec`; they form the default
/// SSH comment. Never panics.
///
/// # Errors
/// [`InvalidSpec`] listing every violated rule.
pub fn resolve(
    spec: &SecretSpecSpec,
    name: &str,
    namespace: &str,
) -> Result<ResolvedSpec, InvalidSpec> {
    let mut errs = Errors::default();

    let kinds = [
        spec.fields.is_some(),
        spec.ssh_key_pair.is_some(),
        spec.basic_auth.is_some(),
    ];
    if kinds.into_iter().filter(|set| *set).count() != 1 {
        errs.add("spec", MSG_EXACTLY_ONE_KIND);
    }

    // Each kind is resolved (and its errors collected) even when more than one
    // is set; the result is only used when there are no errors at all.
    let fields = spec
        .fields
        .as_deref()
        .map(|fields| resolve_fields(fields, &mut errs));
    let ssh = spec
        .ssh_key_pair
        .as_ref()
        .map(|ssh| resolve_ssh(ssh, name, namespace, &mut errs));
    let basic = spec
        .basic_auth
        .as_ref()
        .map(|basic| resolve_basic_auth(basic, &mut errs));

    let rotation = spec
        .rotation
        .as_ref()
        .and_then(|rotation| resolve_rotation(rotation, &mut errs));
    check_data(spec, &mut errs);
    check_secret_template(spec, &mut errs);

    let kind = match (fields, ssh, basic) {
        (Some(fields), None, None) => Some(ResolvedKind::Fields(fields)),
        (None, Some(ssh), None) => Some(ResolvedKind::SshKeyPair(ssh)),
        (None, None, Some(basic)) => Some(ResolvedKind::BasicAuth(basic)),
        _ => None,
    };
    match kind {
        Some(kind) if errs.0.is_empty() => Ok(ResolvedSpec {
            kind,
            data: spec.data.clone(),
            deletion_policy: spec.deletion_policy,
            rotation,
            secret_type: spec.secret.type_.clone(),
            labels: spec.secret.labels.clone(),
            annotations: spec.secret.annotations.clone(),
        }),
        _ => Err(InvalidSpec { errors: errs.0 }),
    }
}

/// Collects `<json path>: <message>` entries.
#[derive(Default)]
struct Errors(Vec<String>);

impl Errors {
    fn add(&mut self, path: &str, message: &str) {
        self.0.push(format!("{path}: {message}"));
    }
}

/// True if `s` is `min..=max` bytes, each within `lo..=hi`. Non-ASCII bytes
/// are never in range, so a pass also means `s` is ASCII.
fn ascii_within(s: &str, lo: u8, hi: u8, min: usize, max: usize) -> bool {
    (min..=max).contains(&s.len()) && s.bytes().all(|b| (lo..=hi).contains(&b))
}

/// `^[-._a-zA-Z0-9]+$`, at most 253 characters.
fn is_key_name(s: &str) -> bool {
    (1..=MAX_KEY_LEN).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn check_key_name(errs: &mut Errors, path: &str, key: &str) {
    if !is_key_name(key) {
        errs.add(path, MSG_KEY_NAME_FORMAT);
    }
}

fn resolve_rotation(rotation: &RotationSpec, errs: &mut Errors) -> Option<Duration> {
    const PATH: &str = "spec.rotation.interval";
    let Some(seconds) = parse_interval(&rotation.interval) else {
        errs.add(PATH, MSG_ROTATION_FORMAT);
        return None;
    };
    // Rejected here rather than trusted to the string pattern: the Duration
    // must be in range whatever the string looked like.
    if !(MIN_ROTATION_SECS..=MAX_ROTATION_SECS).contains(&seconds) {
        errs.add(PATH, MSG_ROTATION_RANGE);
        return None;
    }
    Some(Duration::from_secs(seconds))
}

/// Seconds of `^[1-9][0-9]*(m|h|d)$` (at most 8 characters), `None` if the
/// string does not match.
fn parse_interval(interval: &str) -> Option<u64> {
    if interval.len() > 8 {
        return None;
    }
    let (digits, unit) = interval.split_at_checked(interval.len().checked_sub(1)?)?;
    let unit_seconds: u64 = match unit {
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => return None,
    };
    if digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(unit_seconds)
}

/// Every Secret key the generated kind will write.
fn generated_keys(spec: &SecretSpecSpec) -> Vec<&str> {
    let mut keys = Vec::new();
    if let Some(fields) = &spec.fields {
        keys.extend(fields.iter().map(|f| f.name.as_str()));
    }
    if let Some(ssh) = &spec.ssh_key_pair {
        keys.extend([ssh.keys.private_key.as_str(), ssh.keys.public_key.as_str()]);
    }
    if let Some(basic) = &spec.basic_auth {
        keys.extend([
            basic.keys.username.as_str(),
            basic.keys.password.as_str(),
            basic.keys.htpasswd.as_str(),
        ]);
    }
    keys
}

fn check_data(spec: &SecretSpecSpec, errs: &mut Errors) {
    if spec.data.len() > 64 {
        errs.add("spec.data", MSG_DATA_MAX);
    }
    for (key, value) in &spec.data {
        if value.chars().count() > 65_536 {
            errs.add(&format!("spec.data[{key}]"), MSG_DATA_VALUE_LENGTH);
        }
    }
    if !spec.data.keys().all(|key| is_key_name(key)) {
        errs.add("spec.data", MSG_DATA_KEY_FORMAT);
    }
    if generated_keys(spec)
        .into_iter()
        .any(|key| spec.data.contains_key(key))
    {
        errs.add("spec.data", MSG_DATA_COLLISION);
    }
}

fn check_secret_template(spec: &SecretSpecSpec, errs: &mut Errors) {
    let secret = &spec.secret;
    if secret.type_.chars().count() > MAX_KEY_LEN {
        errs.add("spec.secret.type", MSG_TYPE_LENGTH);
    }
    // `secret.type` immutability (a transition rule) needs the old object;
    // `resolve` has none, so it is enforced by the API server only.
    let basic_ok = spec
        .basic_auth
        .as_ref()
        .is_some_and(|b| b.keys.username == "username" && b.keys.password == "password");
    if secret.type_ == BASIC_AUTH_TYPE && !basic_ok {
        errs.add("spec", MSG_BASIC_AUTH_TYPE);
    }
    let ssh_ok = spec
        .ssh_key_pair
        .as_ref()
        .is_some_and(|s| s.keys.private_key == "ssh-privatekey");
    if secret.type_ == SSH_AUTH_TYPE && !ssh_ok {
        errs.add("spec", MSG_SSH_AUTH_TYPE);
    }
    for (path, map) in [
        ("spec.secret.labels", &secret.labels),
        ("spec.secret.annotations", &secret.annotations),
    ] {
        if map.len() > 32 {
            errs.add(path, MSG_METADATA_MAX);
        }
        for (key, value) in map {
            if value.chars().count() > 4_096 {
                errs.add(&format!("{path}[{key}]"), MSG_METADATA_VALUE_LENGTH);
            }
        }
        let reserved = map.keys().any(|key| {
            key.chars().count() > MAX_KEY_LEN
                || key.starts_with(RESERVED_PREFIX)
                || key == LABEL_MANAGED_BY
        });
        if reserved {
            errs.add(path, MSG_RESERVED_METADATA);
        }
    }
}

fn resolve_fields(fields: &[FieldSpec], errs: &mut Errors) -> Vec<ResolvedField> {
    if fields.len() > 32 {
        errs.add("spec.fields", MSG_FIELDS_MAX);
    }
    let mut seen = BTreeSet::new();
    let mut resolved = Vec::with_capacity(fields.len());
    for (index, field) in fields.iter().enumerate() {
        let path = format!("spec.fields[{index}]");
        check_key_name(errs, &format!("{path}.name"), &field.name);
        if !seen.insert(field.name.as_str()) {
            errs.add(&format!("{path}.name"), MSG_FIELD_NAME_DUPLICATE);
        }
        if field.encoding.is_some() != field.bytes.is_some() {
            errs.add(&path, MSG_ENCODING_WITH_BYTES);
        }
        let generator = if let Some(count) = field.bytes {
            let chars_mode_option = field.length.is_some()
                || field.charset.is_some()
                || field.alphabet.is_some()
                || field.exclude.is_some()
                || field.require.is_some();
            if chars_mode_option {
                errs.add(&path, MSG_BYTES_EXCLUSIVE);
            }
            if !(1..=MAX_RANDOM_LENGTH).contains(&count) {
                errs.add(&format!("{path}.bytes"), MSG_RANDOM_RANGE);
            }
            Generator::Bytes(BytesParams {
                count,
                // Placeholder when `encoding` is missing: that is an error
                // recorded above, so the result is discarded.
                encoding: field.encoding.unwrap_or(Encoding::Hex),
                prefix: affix(errs, &path, "prefix", field.prefix.as_deref()),
                suffix: affix(errs, &path, "suffix", field.suffix.as_deref()),
            })
        } else {
            Generator::Chars(resolve_chars(errs, &path, &CharsInput::from_field(field)))
        };
        resolved.push(ResolvedField {
            name: field.name.clone(),
            generator,
        });
    }
    resolved
}

/// The characters-mode options shared by `fields[]` and `basicAuth.password`.
struct CharsInput<'a> {
    length: Option<u16>,
    charset: Option<Charset>,
    alphabet: Option<&'a str>,
    exclude: Option<&'a str>,
    require: Option<&'a RequireSpec>,
    prefix: Option<&'a str>,
    suffix: Option<&'a str>,
}

impl<'a> CharsInput<'a> {
    fn from_field(field: &'a FieldSpec) -> Self {
        Self {
            length: field.length,
            charset: field.charset,
            alphabet: field.alphabet.as_deref(),
            exclude: field.exclude.as_deref(),
            require: field.require.as_ref(),
            prefix: field.prefix.as_deref(),
            suffix: field.suffix.as_deref(),
        }
    }

    fn from_password(password: &'a BasicAuthPassword) -> Self {
        Self {
            length: password.length,
            charset: password.charset,
            alphabet: password.alphabet.as_deref(),
            exclude: password.exclude.as_deref(),
            require: password.require.as_ref(),
            prefix: password.prefix.as_deref(),
            suffix: password.suffix.as_deref(),
        }
    }
}

/// Validates `prefix` / `suffix` and returns the value (`""` when unset).
fn affix(errs: &mut Errors, path: &str, field: &str, value: Option<&str>) -> String {
    let value = value.unwrap_or_default();
    if !ascii_within(value, b'!', b'~', 0, 64) {
        errs.add(&format!("{path}.{field}"), MSG_AFFIX_FORMAT);
    }
    value.to_owned()
}

fn resolve_chars(errs: &mut Errors, path: &str, input: &CharsInput<'_>) -> CharsParams {
    let length = input.length.unwrap_or(DEFAULT_LENGTH);
    if !(1..=MAX_RANDOM_LENGTH).contains(&length) {
        errs.add(&format!("{path}.length"), MSG_RANDOM_RANGE);
    }

    let charset = input.charset.unwrap_or(Charset::Alphanumeric);
    let mut alphabet_ok = true;
    if input.alphabet.is_some() != (charset == Charset::Custom) {
        errs.add(path, MSG_ALPHABET_WITH_CUSTOM);
        alphabet_ok = false;
    }
    if let Some(alphabet) = input.alphabet
        && !ascii_within(alphabet, b'!', b'~', 1, 256)
    {
        errs.add(&format!("{path}.alphabet"), MSG_ALPHABET_FORMAT);
        alphabet_ok = false;
    }
    let exclude = input.exclude.unwrap_or_default();
    if !ascii_within(exclude, b' ', b'~', 0, 256) {
        errs.add(&format!("{path}.exclude"), MSG_EXCLUDE_FORMAT);
    }

    let require = resolve_require(errs, path, input.require, length);
    let alphabet = effective_alphabet(charset, input.alphabet, exclude);
    // Runtime rules only make sense for a coherent alphabet definition.
    if alphabet_ok {
        check_alphabet(errs, path, &alphabet, require);
    }

    CharsParams {
        length,
        alphabet,
        require,
        prefix: affix(errs, path, "prefix", input.prefix),
        suffix: affix(errs, path, "suffix", input.suffix),
    }
}

fn resolve_require(
    errs: &mut Errors,
    path: &str,
    require: Option<&RequireSpec>,
    length: u16,
) -> Require {
    let Some(spec) = require else {
        return Require::default();
    };
    let mut count = |class: &str, value: Option<u16>| {
        let value = value.unwrap_or(0);
        if value > MAX_RANDOM_LENGTH {
            errs.add(&format!("{path}.require.{class}"), MSG_REQUIRE_RANGE);
        }
        value
    };
    let resolved = Require {
        lower: count("lower", spec.lower),
        upper: count("upper", spec.upper),
        digits: count("digits", spec.digits),
        symbols: count("symbols", spec.symbols),
    };
    let sum = u32::from(resolved.lower)
        + u32::from(resolved.upper)
        + u32::from(resolved.digits)
        + u32::from(resolved.symbols);
    if sum > u32::from(length) {
        errs.add(path, MSG_REQUIRE_SUM);
    }
    resolved
}

/// The charset's characters (a custom `alphabet` is deduplicated), minus
/// `exclude`, sorted by byte.
fn effective_alphabet(charset: Charset, custom: Option<&str>, exclude: &str) -> Vec<u8> {
    let mut set: BTreeSet<u8> = match charset {
        Charset::Alphanumeric => (0..=127u8).filter(u8::is_ascii_alphanumeric).collect(),
        Charset::Ascii => (0..=127u8)
            .filter(|b| b.is_ascii_alphanumeric() || b.is_ascii_punctuation())
            .collect(),
        Charset::Hex => b"0123456789abcdef".iter().copied().collect(),
        Charset::Custom => custom.unwrap_or_default().bytes().collect(),
    };
    for byte in exclude.bytes() {
        set.remove(&byte);
    }
    set.into_iter().collect()
}

/// Membership test of a character class.
type ClassTest = fn(&u8) -> bool;

/// Runtime rules: the alphabet has at least two characters and every required
/// class still has a member in it.
fn check_alphabet(errs: &mut Errors, path: &str, alphabet: &[u8], require: Require) {
    if alphabet.len() < 2 {
        errs.add(path, MSG_ALPHABET_TOO_SMALL);
    }
    let classes: [(&str, u16, ClassTest); 4] = [
        ("lower", require.lower, u8::is_ascii_lowercase),
        ("upper", require.upper, u8::is_ascii_uppercase),
        ("digits", require.digits, u8::is_ascii_digit),
        ("symbols", require.symbols, u8::is_ascii_punctuation),
    ];
    for (class, required, is_member) in classes {
        if required > 0 && !alphabet.iter().any(is_member) {
            errs.add(
                &format!("{path}.require.{class}"),
                &MSG_CLASS_EMPTY.replace("{class}", class),
            );
        }
    }
}

fn resolve_ssh(
    ssh: &SshKeyPairSpec,
    name: &str,
    namespace: &str,
    errs: &mut Errors,
) -> ResolvedSsh {
    const PATH: &str = "spec.sshKeyPair";
    if ssh.rsa_bits.is_some() && ssh.algorithm != SshAlgorithm::Rsa {
        errs.add(PATH, MSG_RSA_BITS_ONLY_RSA);
    }
    if ssh.ecdsa_curve.is_some() && ssh.algorithm != SshAlgorithm::Ecdsa {
        errs.add(PATH, MSG_CURVE_ONLY_ECDSA);
    }
    let algorithm = match ssh.algorithm {
        SshAlgorithm::Ed25519 => SshAlgorithmParams::Ed25519,
        SshAlgorithm::Rsa => SshAlgorithmParams::Rsa {
            bits: ssh.rsa_bits.map_or(DEFAULT_RSA_BITS, u16::from),
        },
        SshAlgorithm::Ecdsa => SshAlgorithmParams::Ecdsa {
            curve: ssh.ecdsa_curve.unwrap_or(DEFAULT_ECDSA_CURVE),
        },
    };
    let comment = match &ssh.comment {
        Some(comment) => {
            if !ascii_within(comment, b' ', b'~', 0, 256) {
                errs.add(&format!("{PATH}.comment"), MSG_COMMENT_FORMAT);
            }
            comment.clone()
        }
        None => format!("{name}@{namespace}"),
    };
    let keys = &ssh.keys;
    check_key_name(errs, &format!("{PATH}.keys.privateKey"), &keys.private_key);
    check_key_name(errs, &format!("{PATH}.keys.publicKey"), &keys.public_key);
    if keys.private_key == keys.public_key {
        errs.add(&format!("{PATH}.keys"), MSG_KEYS_DISTINCT);
    }
    ResolvedSsh {
        algorithm,
        comment,
        private_key_key: keys.private_key.clone(),
        public_key_key: keys.public_key.clone(),
    }
}

fn resolve_basic_auth(basic: &BasicAuthSpec, errs: &mut Errors) -> ResolvedBasicAuth {
    const PATH: &str = "spec.basicAuth";
    let username_ok = (1..=MAX_KEY_LEN).contains(&basic.username.len())
        && basic
            .username
            .bytes()
            .all(|b| (b'!'..=b'~').contains(&b) && b != b':');
    if !username_ok {
        errs.add(&format!("{PATH}.username"), MSG_USERNAME_FORMAT);
    }
    let password = resolve_chars(
        errs,
        &format!("{PATH}.password"),
        &CharsInput::from_password(&basic.password),
    );
    let keys = &basic.keys;
    check_key_name(errs, &format!("{PATH}.keys.username"), &keys.username);
    check_key_name(errs, &format!("{PATH}.keys.password"), &keys.password);
    check_key_name(errs, &format!("{PATH}.keys.htpasswd"), &keys.htpasswd);
    if keys.username == keys.password
        || keys.username == keys.htpasswd
        || keys.password == keys.htpasswd
    {
        errs.add(&format!("{PATH}.keys"), MSG_KEYS_DISTINCT);
    }
    ResolvedBasicAuth {
        username: basic.username.clone(),
        password,
        username_key: keys.username.clone(),
        password_key: keys.password.clone(),
        htpasswd_key: keys.htpasswd.clone(),
    }
}
