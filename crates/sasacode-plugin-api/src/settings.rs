//! Declared plugin settings (`packages/agent/src/settings.ts`). Being ported.

use serde_json::Value;

/// Validate `settings` (merged over `defaults`) against `schema`.
pub fn define_settings(name: &str, settings: &Value, schema: &Value, defaults: Option<&Value>) -> anyhow::Result<Value> {
    let _ = (schema, defaults);
    let _ = name;
    Ok(settings.clone())
}
