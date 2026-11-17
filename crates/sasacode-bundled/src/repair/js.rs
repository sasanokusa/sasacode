//! JS string and value semantics the bundled plugins rely on: lengths and slices in UTF-16 units,
//! `String(v)`, `trim()`. Shared by the ports of `packages/bundled` and `plugins/*`.

use serde_json::Value;

/// `s.length`: UTF-16 code units.
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(start, end)` in UTF-16 units, both clamped to the length. A cut inside a surrogate pair
/// leaves U+FFFD for the half, which is what `toWellFormed()` made of the lone surrogate.
pub fn utf16_slice(s: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = end.min(units.len());
    let start = start.min(end);
    String::from_utf16_lossy(&units[start..end])
}

/// `s.slice(0, n)`.
pub fn utf16_prefix(s: &str, n: usize) -> String {
    if s.len() <= n {
        // At most n bytes, so at most n UTF-16 units.
        return s.to_string();
    }
    utf16_slice(s, 0, n)
}

/// `s.slice(-n)` (the whole string when n is 0, as `slice(-0)` is `slice(0)`).
pub fn utf16_suffix(s: &str, n: usize) -> String {
    let len = utf16_len(s);
    if n == 0 || n >= len {
        return s.to_string();
    }
    utf16_slice(s, len - n, len)
}

/// Whitespace as JS `trim()` and `\s` see it: Unicode White_Space plus the BOM.
pub fn js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// `s.trim()`.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(js_space)
}

/// `String(v)` for a JSON value.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (_, Some(u)) => u.to_string(),
            _ => sasacode_ai::json::js_f64_string(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| if x.is_null() { String::new() } else { js_string(x) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// JS truthiness of a JSON value (`undefined` is the caller's `None`).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `typeof v === "number" ? v : d` for a setting.
pub fn num_setting(settings: &Value, key: &str, d: f64) -> f64 {
    settings.get(key).and_then(Value::as_f64).unwrap_or(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn utf16_slices_and_strings() {
        assert_eq!(utf16_len("a😀b"), 4);
        assert_eq!(utf16_slice("a😀b", 0, 2), "a\u{fffd}");
        assert_eq!(utf16_prefix("日本語です", 2), "日本");
        assert_eq!(utf16_suffix("abcdef", 2), "ef");
        assert_eq!(utf16_suffix("abc", 0), "abc");
        assert_eq!(js_string(&json!(1.5)), "1.5");
        assert_eq!(js_string(&json!(20)), "20");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_trim("\u{feff} x \n"), "x");
    }
}
