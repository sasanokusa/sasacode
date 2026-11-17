//! Shared JSON Schema subset for tool arguments and declared plugin settings
//! (`packages/agent/src/validate.ts`). Being ported.

use serde_json::Value;

/// Problems with `value` against `schema`, each prefixed with `path`. Empty: valid.
pub fn validate(schema: &Value, value: &Value, path: &str) -> Vec<String> {
    let _ = (schema, value, path);
    Vec::new()
}
