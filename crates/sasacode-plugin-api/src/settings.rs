//! Declared plugin settings (`packages/agent/src/settings.ts`): a plugin declares a schema and
//! defaults, the user's `plugins.settings.<name>` is merged over the defaults and validated.

use serde_json::{Map, Value};

use crate::validate::{compile_pattern, is_integer, is_object, js_keys, validate};

const ANNOTATIONS: [&str; 4] = ["title", "description", "$comment", "examples"];
const TYPES: [&str; 7] = ["object", "array", "string", "number", "integer", "boolean", "null"];

/// A deliberately bounded JSON Schema subset; never silently ignore a misspelled constraint.
fn check_schema(raw: &Value, path: &str, depth: usize) -> Result<(), String> {
    if depth > 32 {
        return Err(format!("{path}: schema nesting exceeds 32"));
    }
    let Value::Object(schema) = raw else {
        return Err(format!("{path} must be a schema object"));
    };
    let bad = |key: &str| format!("{path}.{key}: invalid schema constraint");
    for key in js_keys(schema) {
        let value = &schema[key];
        let key = key.as_str();
        if ANNOTATIONS.contains(&key) {
            continue;
        }
        match key {
            "type" => {
                let values: Vec<&Value> = match value {
                    Value::Array(a) => a.iter().collect(),
                    other => vec![other],
                };
                if values.is_empty() || values.iter().any(|v| !v.as_str().is_some_and(|s| TYPES.contains(&s))) {
                    return Err(bad(key));
                }
            }
            "properties" => {
                let Value::Object(props) = value else {
                    return Err(bad(key));
                };
                for name in js_keys(props) {
                    check_schema(&props[name], &format!("{path}.properties.{name}"), depth + 1)?;
                }
            }
            "additionalProperties" => {
                if !value.is_boolean() {
                    check_schema(value, &format!("{path}.{key}"), depth + 1)?;
                }
            }
            "items" => check_schema(value, &format!("{path}.items"), depth + 1)?,
            "anyOf" | "oneOf" | "allOf" => {
                let Value::Array(branches) = value else {
                    return Err(bad(key));
                };
                if branches.is_empty() {
                    return Err(bad(key));
                }
                for (i, s) in branches.iter().enumerate() {
                    check_schema(s, &format!("{path}.{key}[{i}]"), depth + 1)?;
                }
            }
            "required" => {
                let Value::Array(names) = value else {
                    return Err(bad(key));
                };
                let mut seen = std::collections::HashSet::new();
                if names.iter().any(|v| !v.is_string()) || !names.iter().all(|v| seen.insert(v.as_str())) {
                    return Err(bad(key));
                }
            }
            "enum" => {
                if !value.as_array().is_some_and(|a| !a.is_empty()) {
                    return Err(bad(key));
                }
            }
            "const" => {} // any JSON value
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                if !value.as_f64().is_some_and(f64::is_finite) {
                    return Err(bad(key));
                }
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties" | "maxProperties" => {
                if !is_integer(value) || value.as_f64().is_none_or(|n| n < 0.0) {
                    return Err(bad(key));
                }
            }
            "pattern" => {
                let Some(p) = value.as_str() else { return Err(bad(key)) };
                if compile_pattern(p).is_none() {
                    return Err(bad(key));
                }
            }
            _ => return Err(format!("{path}.{key}: unsupported schema keyword")),
        }
    }
    Ok(())
}

/// `value` over `defaults`: objects merge key by key, anything else (arrays, null) replaces.
fn merge(defaults: &Value, value: &Value, depth: usize) -> Result<Value, String> {
    if depth > 64 {
        return Err("settings nesting exceeds 64".into());
    }
    let (Value::Object(d), Value::Object(v)) = (defaults, value) else {
        return Ok(value.clone());
    };
    // Defaults' keys, then the ones only the settings have; then JS's own order (integer-like
    // keys first), as Object.fromEntries would iterate the result.
    let mut keys = Map::new();
    for k in js_keys(d).into_iter().chain(js_keys(v)) {
        keys.entry(k.clone()).or_insert(Value::Null);
    }
    let mut out = Map::new();
    for key in js_keys(&keys) {
        let merged = match (d.get(key), v.get(key)) {
            (Some(dv), Some(vv)) => merge(dv, vv, depth + 1)?,
            (_, Some(vv)) => vv.clone(),
            (Some(dv), None) => dv.clone(),
            (None, None) => continue,
        };
        out.insert(key.clone(), merged);
    }
    Ok(Value::Object(out))
}

/// Validate `settings` (merged over `defaults`) against `schema`. Errors name the field and the
/// constraint, never the (possibly secret) value: `Plugin <name> settings: ...`.
pub fn define_settings(
    name: &str,
    settings: &Value,
    schema: &Value,
    defaults: Option<&Value>,
) -> anyhow::Result<Value> {
    define(name, settings, schema, defaults).map_err(|e| anyhow::anyhow!("Plugin {name} settings: {e}"))
}

fn define(name: &str, settings: &Value, schema: &Value, defaults: Option<&Value>) -> Result<Value, String> {
    check_schema(schema, "schema", 0)?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err("schema.type must be object".into());
    }
    if let Some(d) = defaults
        && !is_object(d)
    {
        return Err("defaults must be an object".into());
    }
    let empty = Value::Object(Map::new());
    let merged = merge(defaults.unwrap_or(&empty), settings, 0)?;
    let errors = validate(schema, &merged, &format!("plugins.settings.{name}"));
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn err(r: anyhow::Result<Value>) -> String {
        match r {
            Ok(v) => panic!("expected an error, got {v}"),
            Err(e) => e.to_string(),
        }
    }

    // plugin-api-v19.test.ts: "defineSettings merges nested defaults, replaces arrays/null, and does
    // not mutate either source"
    #[test]
    fn merges_nested_defaults_and_replaces_arrays_and_null() {
        let raw = json!({ "options": { "timeout": 30 }, "values": [2], "nullable": null });
        let defaults =
            json!({ "options": { "timeout": 10, "enabled": true }, "values": [1, 3], "nullable": "default" });
        let schema = json!({
            "type": "object",
            "properties": {
                "options": { "type": "object" },
                "values": { "type": "array", "items": { "type": "integer" } },
                "nullable": { "type": ["null", "string"] }
            },
            "additionalProperties": false
        });
        let mut value = define_settings("demo", &raw, &schema, Some(&defaults)).unwrap();
        assert_eq!(
            value,
            json!({ "options": { "timeout": 30, "enabled": true }, "values": [2], "nullable": null })
        );
        value["options"]["timeout"] = json!(77);
        assert_eq!(raw["options"]["timeout"], 30);
        assert_eq!(defaults["options"]["timeout"], 10);
    }

    // plugin-api-v19.test.ts: "settings reject unknown keys, wrong types/ranges and unsupported
    // schema keywords; no coercion or secret echo" (the plugin-loader part, a failed load showing a
    // notice, belongs to the agent's plugin host).
    #[test]
    fn rejects_unknown_keys_types_ranges_and_keywords_without_echo() {
        let schema = json!({ "type": "object", "properties": { "timeout": { "type": "integer", "minimum": 1 } } });
        let e = err(define_settings(
            "demo",
            &json!({ "timeout": "private-token" }),
            &schema,
            None,
        ));
        assert_eq!(
            e,
            "Plugin demo settings: plugins.settings.demo.timeout must be integer, got string"
        );
        assert!(!e.contains("private-token"));
        let closed = json!({ "type": "object", "additionalProperties": false });
        assert!(err(define_settings("demo", &json!({ "extra": true }), &closed, None)).contains("extra"));
        let min = json!({ "type": "object", "properties": { "timeout": { "minimum": 1 } } });
        assert!(err(define_settings("demo", &json!({ "timeout": 0 }), &min, None)).contains("minimum"));
        let refs = json!({ "type": "object", "$ref": "https://example.invalid" });
        assert!(err(define_settings("demo", &json!({}), &refs, None)).contains("unsupported"));
        let pattern = json!({ "type": "object", "properties": { "x": { "pattern": "[" } } });
        let e = err(define_settings("demo", &json!({}), &pattern, None));
        assert!(e.contains("invalid schema"));
        assert_eq!(
            e,
            "Plugin demo settings: schema.properties.x.pattern: invalid schema constraint"
        );
    }

    // plugin-api-v19.test.ts: "settings support schema-valued maps and combinations without
    // prototype pollution"
    #[test]
    fn schema_valued_maps_and_combinations() {
        let schema = json!({
            "type": "object",
            "additionalProperties": {
                "anyOf": [{ "const": false }, { "type": "string", "minLength": 2, "maxLength": 8, "pattern": "^[a-z]+$" }]
            }
        });
        let good = json!({ "a": false, "b": "good" });
        assert_eq!(define_settings("x", &good, &schema, None).unwrap(), good);
        assert!(err(define_settings("x", &json!({ "a": "X" }), &schema, None)).contains("anyOf"));
        let raw: Value = serde_json::from_str(r#"{"__proto__":{"polluted":true},"constructor":"own"}"#).unwrap();
        let value = define_settings("x", &raw, &json!({ "type": "object" }), Some(&json!({}))).unwrap();
        assert!(value.as_object().unwrap().contains_key("__proto__"));
        let closed = json!({ "type": "object", "additionalProperties": false });
        assert!(err(define_settings("x", &raw, &closed, None)).contains("not an allowed property"));
    }

    // New: the schema checks and messages not covered above.
    #[test]
    fn schema_checks() {
        let s = |schema: Value| err(define_settings("p", &json!({}), &schema, None));
        assert_eq!(
            s(json!({ "type": "array" })),
            "Plugin p settings: schema.type must be object"
        );
        assert_eq!(
            s(json!({ "type": ["object"] })),
            "Plugin p settings: schema.type must be object"
        );
        assert_eq!(
            s(json!({ "type": "map" })),
            "Plugin p settings: schema.type: invalid schema constraint"
        );
        assert_eq!(s(json!(1)), "Plugin p settings: schema must be a schema object");
        assert_eq!(
            s(json!({ "type": "object", "required": ["a", "a"] })),
            "Plugin p settings: schema.required: invalid schema constraint"
        );
        assert_eq!(
            s(json!({ "type": "object", "properties": { "a": { "anyOf": [] } } })),
            "Plugin p settings: schema.properties.a.anyOf: invalid schema constraint"
        );
        assert_eq!(
            s(json!({ "type": "object", "properties": { "a": { "oneOf": [{ "minLength": -1 }] } } })),
            "Plugin p settings: schema.properties.a.oneOf[0].minLength: invalid schema constraint"
        );
        assert_eq!(
            s(json!({ "type": "object", "items": { "maxItems": 1.5 } })),
            "Plugin p settings: schema.items.maxItems: invalid schema constraint"
        );
        let mut deep = json!({ "type": "object" });
        for _ in 0..33 {
            deep = json!({ "type": "object", "additionalProperties": deep });
        }
        assert!(s(deep).ends_with(": schema nesting exceeds 32"));
        assert_eq!(
            err(define_settings(
                "p",
                &json!({}),
                &json!({ "type": "object" }),
                Some(&Value::Null)
            )),
            "Plugin p settings: defaults must be an object"
        );
        // Annotations and 1.0 as an integer constraint are fine.
        let ok = json!({ "type": "object", "title": "t", "description": "d", "$comment": "c", "examples": [],
            "properties": { "n": { "type": "string", "maxLength": 2.0 } } });
        assert_eq!(
            define_settings("p", &json!({ "n": "ab" }), &ok, None).unwrap(),
            json!({ "n": "ab" })
        );
        let mut nested = json!(1);
        for _ in 0..66 {
            nested = json!({ "a": nested });
        }
        assert_eq!(
            err(define_settings(
                "p",
                &nested,
                &json!({ "type": "object" }),
                Some(&nested)
            )),
            "Plugin p settings: settings nesting exceeds 64"
        );
        // Keys come out as JS would order them: integer-like first, then defaults', then the settings' own.
        let merged = define_settings(
            "p",
            &json!({ "2": 1, "b": 2, "1": 3 }),
            &json!({ "type": "object" }),
            Some(&json!({ "z": 0, "5": 1 })),
        )
        .unwrap();
        assert_eq!(merged.to_string(), r#"{"1":3,"2":1,"5":1,"z":0,"b":2}"#);
        // Several problems are joined.
        let two = json!({ "type": "object", "properties": { "a": { "type": "string" }, "b": { "type": "string" } } });
        assert_eq!(
            err(define_settings("p", &json!({ "a": 1, "b": 2 }), &two, None)),
            "Plugin p settings: plugins.settings.p.a must be string, got number; plugins.settings.p.b must be string, got number"
        );
    }
}
