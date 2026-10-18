// SPDX-License-Identifier: Apache-2.0

//! Schema transforms for bounds that schemars attributes cannot express.

use schemars::Schema;
use schemars::transform::Transform;
use serde_json::{Value, json};

/// Bounds a `map<string,string>`: `maxProperties` on the map and `maxLength`
/// on its values. Keys cannot be bounded in an `OpenAPI` schema; that is a CEL
/// rule.
pub(crate) fn string_map(max_properties: usize, max_value_length: usize) -> impl Transform {
    move |schema: &mut Schema| {
        schema.insert("maxProperties".to_owned(), json!(max_properties));
        if let Some(Value::Object(values)) = schema.get_mut("additionalProperties") {
            values.insert("maxLength".to_owned(), json!(max_value_length));
        }
    }
}

/// Drops the `null` that `Option<_>` adds to an `enum` list; `nullable: true`
/// already expresses it.
pub(crate) fn strip_null_enum(schema: &mut Schema) {
    if let Some(Value::Array(values)) = schema.get_mut("enum") {
        values.retain(|v| !v.is_null());
    }
}

/// Removes a `default` that schemars derived from `#[serde(default)]`.
///
/// Used where deserialisation needs a default but the stored object should not
/// get the value materialised (empty maps).
pub(crate) fn no_schema_default(schema: &mut Schema) {
    schema.remove("default");
}
