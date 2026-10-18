// SPDX-License-Identifier: Apache-2.0

//! `spec.fields[]`: generated strings and tokens.

use kube::KubeSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::rules;
use crate::constants::{
    KEY_NAME_PATTERN, MAX_AFFIX_LEN, MAX_ALPHABET_LEN, MAX_EXCLUDE_LEN, MAX_KEY_NAME_LEN,
    MAX_RANDOM_LENGTH, MAX_REQUIRE_COUNT, MIN_RANDOM_LENGTH,
};

/// One generated Secret key. Two mutually exclusive modes:
///
/// - **characters** (default): random characters; uses `length`, `charset`,
///   `alphabet`, `exclude`, `require`.
/// - **random bytes**: `bytes` + `encoding`; excludes all of the above.
///
/// `prefix` and `suffix` work in both modes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = rules::bytes_exclusive())]
#[x_kube(validation = rules::encoding_with_bytes())]
#[x_kube(validation = rules::alphabet_with_custom())]
#[x_kube(validation = rules::require_sum())]
pub struct FieldSpec {
    /// The Secret key to generate. Letters, digits, `-`, `_` and `.` (at most
    /// 253 characters), but not `.` itself and not starting with `..`; unique
    /// within `fields`.
    #[schemars(length(max = MAX_KEY_NAME_LEN), regex(pattern = KEY_NAME_PATTERN))]
    pub name: String,

    /// Characters mode: number of random characters, 1-1024. Does not count
    /// `prefix`/`suffix`. When unset the operator uses 32 (not written into
    /// the object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = MIN_RANDOM_LENGTH, max = MAX_RANDOM_LENGTH))]
    pub length: Option<u16>,

    /// Characters mode: the alphabet to draw from. `alphanumeric` is a-z, A-Z
    /// and 0-9; `ascii` adds ASCII punctuation; `hex` is 0-9a-f; `custom`
    /// uses `alphabet`. When unset the operator uses `alphanumeric` (not
    /// written into the object).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(transform = super::bounds::plain_enum)]
    pub charset: Option<Charset>,

    /// Characters mode: custom alphabet, required if and only if
    /// `charset: custom`. At least 2 distinct printable ASCII characters (no
    /// space), at most 256 characters; duplicates are ignored. Example:
    /// `"0123456789"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_ALPHABET_LEN), regex(pattern = r"^[!-~]+$"))]
    pub alphabet: Option<String>,

    /// Characters mode: characters removed from the effective alphabet.
    /// Printable ASCII (space allowed), at most 256 characters. Example for
    /// AWS RDS: `/@" `.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_EXCLUDE_LEN), regex(pattern = r"^[ -~]*$"))]
    pub exclude: Option<String>,

    /// Characters mode: minimum number of characters per class. The sum must
    /// not exceed the effective `length`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require: Option<RequireSpec>,

    /// Random-bytes mode: number of random bytes, 1-1024. Requires
    /// `encoding`. Example: `32` with `base64url` makes a Fernet key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = MIN_RANDOM_LENGTH, max = MAX_RANDOM_LENGTH))]
    pub bytes: Option<u16>,

    /// Random-bytes mode: how the bytes are rendered: `hex`, `base64`,
    /// `base64url` or `base32`. Required with `bytes`. Standard alphabets,
    /// padded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(transform = super::bounds::plain_enum)]
    pub encoding: Option<Encoding>,

    /// Text prepended verbatim to the value. Printable ASCII without spaces,
    /// at most 64 characters. Example: `GK` for an S3-style access key id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_AFFIX_LEN), regex(pattern = r"^[!-~]*$"))]
    pub prefix: Option<String>,

    /// Text appended verbatim to the value. Printable ASCII without spaces,
    /// at most 64 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = MAX_AFFIX_LEN), regex(pattern = r"^[!-~]*$"))]
    pub suffix: Option<String>,
}

// Alphabet for characters mode (plain comment: see the note in `mod.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Charset {
    /// `a-z`, `A-Z`, `0-9`.
    Alphanumeric,
    /// Alphanumeric plus ASCII punctuation.
    Ascii,
    /// `0-9a-f`.
    Hex,
    /// The characters in `alphabet`.
    Custom,
}

// Rendering of random bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    /// Lower-case hexadecimal.
    Hex,
    /// Standard base64, padded.
    Base64,
    /// URL-safe base64, padded.
    Base64url,
    /// Standard base32, padded.
    Base32,
}

/// Minimum number of characters per class in characters mode. Each class that
/// is required must still have at least one character left after `exclude`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequireSpec {
    /// Minimum lower-case letters (`a-z`), 0-1024.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = MAX_REQUIRE_COUNT))]
    pub lower: Option<u16>,

    /// Minimum upper-case letters (`A-Z`), 0-1024.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = MAX_REQUIRE_COUNT))]
    pub upper: Option<u16>,

    /// Minimum digits (`0-9`), 0-1024.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = MAX_REQUIRE_COUNT))]
    pub digits: Option<u16>,

    /// Minimum ASCII punctuation characters, 0-1024.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 0, max = MAX_REQUIRE_COUNT))]
    pub symbols: Option<u16>,
}
