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

/// Turns an `Option<enum>` property into a plain, non-nullable `enum`.
///
/// `nullable: true` does not exempt `null` from the `enum` check: the API
/// server rejects an explicit `null` with `Unsupported value: null`, even when
/// `null` is listed in the `enum` (verified on kind 1.34 and 1.37). A
/// non-nullable property instead has an explicit `null` pruned before
/// validation, so `charset: null` (e.g. rendered by Helm from an unset value)
/// is the same as omitting it.
///
/// Handles both shapes an `Option<enum>` has when the transform runs: schemars'
/// `anyOf: [{oneOf: [{const}...]}, {type: null}]` for documented unit enums
/// (flattened here; `kube` drops the per-variant descriptions anyway) and an
/// inline `enum` with a nullable `type` (`RsaBits`).
pub(crate) fn plain_enum(schema: &mut Schema) {
    if let Some(Value::Array(branches)) = schema.get("anyOf")
        && let [Value::Object(variants), Value::Object(null)] = branches.as_slice()
        && null.get("type") == Some(&json!("null"))
        && let Some(Value::Array(one_of)) = variants.get("oneOf")
    {
        let consts: Vec<Value> = one_of
            .iter()
            .filter_map(|v| v.get("const").cloned())
            .collect();
        if consts.len() == one_of.len() {
            schema.remove("anyOf");
            schema.insert("type".to_owned(), json!("string"));
            schema.insert("enum".to_owned(), Value::Array(consts));
        }
    }
    if let Some(Value::Array(values)) = schema.get_mut("enum") {
        values.retain(|v| !v.is_null());
    }
    if let Some(Value::Array(types)) = schema.get("type")
        && let Some(first) = types.iter().find(|t| *t != "null").cloned()
    {
        schema.insert("type".to_owned(), first);
    }
}

/// Removes a `default` that schemars derived from `#[serde(default)]`.
///
/// Used where deserialisation needs a default but the stored object should not
/// get the value materialised (empty maps).
pub(crate) fn no_schema_default(schema: &mut Schema) {
    schema.remove("default");
}
