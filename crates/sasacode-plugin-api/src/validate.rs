//! Shared JSON Schema subset for tool arguments and declared plugin settings
//! (`packages/agent/src/validate.ts`). No coercion.
//!
//! The error texts reach the model and must stay identical to the TypeScript ones. JS semantics
//! kept on purpose: `1.0` is an integer, lengths count code points, `pattern` is a JS regex with the
//! `u` flag (`\d`, `\w`, `\b` ASCII, `.` stops at every line terminator), unknown type names match,
//! numbers compare as numbers (`1` equals `1.0`), and object keys are visited in JS order
//! (integer-like keys first).

use serde_json::{Map, Value};

/// Problems with `value` against `schema`, each prefixed with `path`. Empty: valid.
pub fn validate(schema: &Value, value: &Value, path: &str) -> Vec<String> {
    let empty = Map::new();
    // A sub-schema that is not an object (true, an array, ...) has no keywords: anything is valid.
    let schema = schema.as_object().unwrap_or(&empty);
    let mut errors = Vec::new();
    if let Some(ty) = schema.get("type").filter(|t| truthy(t)) {
        let types: Vec<&Value> = match ty {
            Value::Array(a) => a.iter().collect(),
            other => vec![other],
        };
        if !types.iter().any(|t| type_matches(t, value)) {
            let names: Vec<String> = types.iter().map(|t| join_element(t)).collect();
            errors.push(format!(
                "{path} must be {}, got {}",
                names.join(" or "),
                describe(value)
            ));
            return errors;
        }
    }
    if let Some(c) = schema.get("const")
        && !equal(c, value)
    {
        errors.push(format!("{path} must match const"));
    }
    for keyword in ["anyOf", "oneOf", "allOf"] {
        let Some(Value::Array(branches)) = schema.get(keyword) else {
            continue;
        };
        let matches = branches.iter().filter(|s| validate(s, value, path).is_empty()).count();
        if (keyword == "anyOf" && matches == 0)
            || (keyword == "oneOf" && matches != 1)
            || (keyword == "allOf" && matches != branches.len())
        {
            errors.push(format!("{path} must match {keyword}"));
        }
    }
    if let Some(e @ Value::Array(options)) = schema.get("enum")
        && !options.iter().any(|v| equal(v, value))
    {
        errors.push(format!("{path} must be one of {}", js_stringify(e)));
    }
    if let Value::Object(obj) = value {
        let props = schema.get("properties").and_then(Value::as_object);
        if let Some(Value::Array(required)) = schema.get("required") {
            for r in required {
                let r = js_string(r);
                if !obj.contains_key(&r) {
                    errors.push(format!("{path}.{r} is required"));
                }
            }
        }
        let additional = schema.get("additionalProperties");
        for k in js_keys(obj) {
            let v = &obj[k];
            if let Some(p) = props.and_then(|p| p.get(k)) {
                errors.extend(validate(p, v, &format!("{path}.{k}")));
            } else if additional == Some(&Value::Bool(false)) {
                errors.push(format!("{path}.{k} is not an allowed property"));
            } else if let Some(ap @ (Value::Object(_) | Value::Array(_))) = additional {
                errors.extend(validate(ap, v, &format!("{path}.{k}")));
            }
        }
    }
    if let Value::Array(items) = value
        && let Some(item_schema) = schema.get("items").filter(|s| truthy(s))
    {
        for (i, v) in items.iter().enumerate() {
            errors.extend(validate(item_schema, v, &format!("{path}[{i}]")));
        }
    }
    if let Some(n) = value.as_f64() {
        if !n.is_finite() {
            errors.push(format!("{path} must be finite"));
        }
        bound(&mut errors, schema, path, "minimum", n, true);
        bound(&mut errors, schema, path, "maximum", n, false);
        if let Some(min) = schema.get("exclusiveMinimum").and_then(Value::as_f64)
            && n <= min
        {
            errors.push(format!("{path} violates exclusiveMinimum"));
        }
        if let Some(max) = schema.get("exclusiveMaximum").and_then(Value::as_f64)
            && n >= max
        {
            errors.push(format!("{path} violates exclusiveMaximum"));
        }
    }
    if let Value::String(s) = value {
        let len = s.chars().count() as f64;
        bound(&mut errors, schema, path, "minLength", len, true);
        bound(&mut errors, schema, path, "maxLength", len, false);
        if let Some(Value::String(pattern)) = schema.get("pattern") {
            match compile_pattern(pattern) {
                Some(re) if re.is_match(s) => {}
                Some(_) => errors.push(format!("{path} does not match pattern")),
                None => errors.push(format!("{path} has an invalid schema pattern")),
            }
        }
    }
    match value {
        Value::Array(a) => {
            bound(&mut errors, schema, path, "minItems", a.len() as f64, true);
            bound(&mut errors, schema, path, "maxItems", a.len() as f64, false);
        }
        Value::Object(o) => {
            bound(&mut errors, schema, path, "minProperties", o.len() as f64, true);
            bound(&mut errors, schema, path, "maxProperties", o.len() as f64, false);
        }
        _ => {}
    }
    errors
}

/// `bound(key, actual, smaller)`: a numeric limit in the schema that `actual` falls short of / exceeds.
fn bound(errors: &mut Vec<String>, schema: &Map<String, Value>, path: &str, key: &str, actual: f64, smaller: bool) {
    if let Some(limit) = schema.get(key).and_then(Value::as_f64)
        && (if smaller { actual < limit } else { actual > limit })
    {
        errors.push(format!("{path} violates {key} {}", sasacode_ai::json::js_f64_string(limit)));
    }
}

/// `isObject`: a JSON object (not an array, not null).
pub fn is_object(v: &Value) -> bool {
    v.is_object()
}

/// `Number.isInteger`: `1.0` is an integer too.
pub fn is_integer(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.is_finite() && f.fract() == 0.0),
        _ => false,
    }
}

fn type_matches(t: &Value, v: &Value) -> bool {
    match js_string(t).as_str() {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => is_integer(v),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => is_object(v),
        "null" => v.is_null(),
        _ => true,
    }
}

fn describe(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Array(_) => "array",
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Object(_) => "object",
    }
}

/// Deep equality with JS number semantics (`1` equals `1.0`) and key order ignored.
pub(crate) fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| equal(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| equal(v, w)))
        }
        _ => a == b,
    }
}

/// JS truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `String(v)` in JS.
pub fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => i.to_string(),
            (_, Some(u)) => u.to_string(),
            _ => sasacode_ai::json::js_f64_string(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(join_element).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// An element as `Array.prototype.join` writes it: null becomes the empty string.
fn join_element(v: &Value) -> String {
    if v.is_null() { String::new() } else { js_string(v) }
}

/// `JSON.stringify` of a value: numbers in JS form (`1` for `1.0`).
pub fn js_stringify(v: &Value) -> String {
    fn normalize(v: &Value) -> Value {
        match v {
            Value::Number(n) if !(n.is_i64() || n.is_u64()) => sasacode_ai::json::js_number(n.as_f64().unwrap_or(0.0)),
            Value::Array(a) => Value::Array(a.iter().map(normalize).collect()),
            Value::Object(o) => Value::Object(js_keys(o).into_iter().map(|k| (k.clone(), normalize(&o[k]))).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&normalize(v)).unwrap_or_else(|_| "null".into())
}

/// An array index key as JS orders it (`"0"`, `"17"`; not `"01"` or past 2^32 - 2).
fn array_index(k: &str) -> Option<u32> {
    if k.is_empty() || (k.len() > 1 && k.starts_with('0')) || !k.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    k.parse::<u32>().ok().filter(|&n| n != u32::MAX)
}

/// The keys of an object in JS iteration order: integer-like keys ascending, then the rest in
/// insertion order.
pub fn js_keys(o: &Map<String, Value>) -> Vec<&String> {
    let mut indexed: Vec<(u32, &String)> = o.keys().filter_map(|k| array_index(k).map(|n| (n, k))).collect();
    if indexed.is_empty() {
        return o.keys().collect();
    }
    indexed.sort_by_key(|(n, _)| *n);
    let mut keys: Vec<&String> = indexed.into_iter().map(|(_, k)| k).collect();
    keys.extend(o.keys().filter(|k| array_index(k).is_none()));
    keys
}

/// A compiled `pattern`, like `new RegExp(pattern, "u")`.
pub(crate) enum Pattern {
    Plain(regex::Regex),
    Fancy(Box<fancy_regex::Regex>),
}

impl Pattern {
    pub(crate) fn is_match(&self, s: &str) -> bool {
        match self {
            Pattern::Plain(re) => re.is_match(s),
            // A backtracking limit reached is no match, as a JS engine that gave up would never say yes.
            Pattern::Fancy(re) => re.is_match(s).unwrap_or(false),
        }
    }
}

/// Compile a JS `u`-flag pattern: `regex` first, `fancy-regex` for lookaround and backreferences.
/// `None`: the pattern is invalid.
pub(crate) fn compile_pattern(pattern: &str) -> Option<Pattern> {
    let plain = translate_js_pattern(pattern, false)?;
    if let Ok(re) = regex::Regex::new(&plain) {
        return Some(Pattern::Plain(re));
    }
    let fancy = translate_js_pattern(pattern, true)?;
    fancy_regex::Regex::new(&fancy)
        .ok()
        .map(|re| Pattern::Fancy(Box::new(re)))
}

const JS_SPACE_CLASS: &str =
    r"\t\n\x0B\x0C\r \xA0\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// Rewrite the parts of JS regex syntax whose meaning differs in Rust: ASCII `\d \w \b`, JS `\s`,
/// `.` excluding every line terminator, `\uXXXX`, `\u{X}`, `\cX`, `\/`, `[]`, `[^]`, and the
/// characters that start nested classes or set operations inside a Rust class. `None`: not a
/// valid JS pattern as far as this can tell (e.g. a dangling backslash).
fn translate_js_pattern(p: &str, fancy: bool) -> Option<String> {
    let chars: Vec<char> = p.chars().collect();
    let mut out = String::with_capacity(p.len() + 16);
    let mut in_class = false;
    let mut i = 0;
    let word = "0-9A-Za-z_";
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                let n = *chars.get(i)?;
                i += 1;
                match n {
                    'd' if in_class => out.push_str("0-9"),
                    'd' => out.push_str("[0-9]"),
                    'D' => out.push_str("[^0-9]"),
                    'w' if in_class => out.push_str(word),
                    'w' => out.push_str("[0-9A-Za-z_]"),
                    'W' => out.push_str("[^0-9A-Za-z_]"),
                    's' if in_class => out.push_str(JS_SPACE_CLASS),
                    's' => out.push_str(&format!("[{JS_SPACE_CLASS}]")),
                    'S' => out.push_str(&format!("[^{JS_SPACE_CLASS}]")),
                    'b' if in_class => out.push_str(r"\x08"),
                    'b' if fancy => {
                        out.push_str(r"(?:(?<=[0-9A-Za-z_])(?![0-9A-Za-z_])|(?<![0-9A-Za-z_])(?=[0-9A-Za-z_]))")
                    }
                    'b' => out.push_str(r"(?-u:\b)"),
                    'B' if fancy => {
                        out.push_str(r"(?:(?<=[0-9A-Za-z_])(?=[0-9A-Za-z_])|(?<![0-9A-Za-z_])(?![0-9A-Za-z_]))")
                    }
                    'B' => out.push_str(r"(?-u:\B)"),
                    'u' => {
                        let (cp, used) = unicode_escape(&chars[i..])?;
                        i += used;
                        out.push_str(&format!(r"\x{{{cp:X}}}"));
                    }
                    'c' => {
                        let letter = chars.get(i).filter(|l| l.is_ascii_alphabetic())?;
                        i += 1;
                        out.push_str(&format!(r"\x{{{:X}}}", (*letter as u32) % 32));
                    }
                    '/' => out.push('/'),
                    '-' if !in_class => return None, // not an identity escape under `u`
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '[' if !in_class => {
                if chars.get(i) == Some(&']') {
                    i += 1;
                    out.push_str(r"[^\x00-\x{10FFFF}]"); // [] matches nothing
                } else if chars.get(i) == Some(&'^') && chars.get(i + 1) == Some(&']') {
                    i += 2;
                    out.push_str(r"[\x00-\x{10FFFF}]"); // [^] matches anything
                } else {
                    in_class = true;
                    out.push('[');
                    if chars.get(i) == Some(&'^') {
                        i += 1;
                        out.push('^');
                    }
                }
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '[' | '&' | '~' if in_class => {
                out.push('\\');
                out.push(c);
            }
            '.' if !in_class => out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
            _ => out.push(c),
        }
    }
    if in_class {
        return None;
    }
    Some(out)
}

/// The code point of `XXXX`, `{X...}`, or a `\uD83D\uDE00` surrogate pair; and the chars used.
fn unicode_escape(rest: &[char]) -> Option<(u32, usize)> {
    let hex = |cs: &[char]| -> Option<u32> {
        if cs.is_empty() || !cs.iter().all(char::is_ascii_hexdigit) {
            return None;
        }
        u32::from_str_radix(&cs.iter().collect::<String>(), 16).ok()
    };
    if rest.first() == Some(&'{') {
        let close = rest.iter().position(|&c| c == '}')?;
        let cp = hex(&rest[1..close]).filter(|&cp| cp <= 0x10FFFF)?;
        return Some((cp, close + 1));
    }
    let high = hex(rest.get(..4)?)?;
    if (0xD800..0xDC00).contains(&high)
        && rest.get(4) == Some(&'\\')
        && rest.get(5) == Some(&'u')
        && let Some(low) = rest.get(6..10).and_then(hex).filter(|l| (0xDC00..0xE000).contains(l))
    {
        return Some((0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00), 10));
    }
    Some((high, 4))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // From packages/agent/test/agent.test.ts ("input.path must be string") and
    // packages/bundled/test/repair.test.ts ("input.overwrite is not an allowed property"): the texts
    // tool-exec builds from validate().
    #[test]
    fn tool_argument_errors() {
        let schema = json!({
            "type": "object",
            "properties": { "path": { "type": "string" }, "content": { "type": "string" } },
            "required": ["path", "content"],
            "additionalProperties": false
        });
        assert_eq!(
            validate(&schema, &json!({ "path": 1, "content": "x" }), "input"),
            ["input.path must be string, got number"]
        );
        assert_eq!(
            validate(
                &schema,
                &json!({ "path": "a", "content": "x", "overwrite": true }),
                "input"
            ),
            ["input.overwrite is not an allowed property"]
        );
        assert_eq!(
            validate(&schema, &json!({ "path": "a" }), "input"),
            ["input.content is required"]
        );
        assert_eq!(
            validate(&schema, &json!([]), "input"),
            ["input must be object, got array"]
        );
        assert!(validate(&schema, &json!({ "path": "a", "content": "b" }), "input").is_empty());
    }

    // New (no TS test calls validate directly): the JS semantics the port has to keep.
    #[test]
    fn integers_lengths_and_numbers_follow_js() {
        let int = json!({ "type": "integer", "minimum": 1 });
        assert!(validate(&int, &serde_json::from_str::<Value>("2.0").unwrap(), "x").is_empty());
        assert_eq!(validate(&int, &json!(1.5), "x"), ["x must be integer, got number"]);
        assert_eq!(validate(&int, &json!(0), "x"), ["x violates minimum 1"]);
        assert_eq!(
            validate(&json!({ "maximum": 0.5 }), &json!(1), "x"),
            ["x violates maximum 0.5"]
        );
        assert_eq!(
            validate(&json!({ "exclusiveMinimum": 1 }), &json!(1), "x"),
            ["x violates exclusiveMinimum"]
        );
        assert_eq!(
            validate(&json!({ "exclusiveMaximum": 1 }), &json!(1), "x"),
            ["x violates exclusiveMaximum"]
        );
        // Code points, not bytes or UTF-16 units: "😀あ" is 2.
        let len = json!({ "minLength": 3, "maxLength": 2 });
        assert_eq!(validate(&len, &json!("😀あ"), "s"), ["s violates minLength 3"]);
        assert_eq!(validate(&len, &json!("😀あい"), "s"), ["s violates maxLength 2"]);
        assert_eq!(
            validate(&json!({ "minItems": 2 }), &json!([1]), "a"),
            ["a violates minItems 2"]
        );
        assert_eq!(
            validate(&json!({ "maxProperties": 0 }), &json!({ "k": 1 }), "o"),
            ["o violates maxProperties 0"]
        );
    }

    #[test]
    fn types_const_enum_and_combinations() {
        // Unknown type names match anything; a list names every type it wanted.
        assert!(validate(&json!({ "type": "whatever" }), &json!(1), "x").is_empty());
        assert_eq!(
            validate(&json!({ "type": ["null", "string"] }), &json!(true), "x"),
            ["x must be null or string, got boolean"]
        );
        assert!(
            validate(
                &json!({ "const": 1 }),
                &serde_json::from_str::<Value>("1.0").unwrap(),
                "x"
            )
            .is_empty()
        );
        assert!(
            validate(
                &json!({ "const": { "a": [1, 2], "b": null } }),
                &json!({ "b": null, "a": [1, 2] }),
                "x"
            )
            .is_empty()
        );
        assert_eq!(
            validate(&json!({ "const": [1] }), &json!({ "0": 1 }), "x"),
            ["x must match const"]
        );
        let e = json!({ "enum": [1.0, "a", null] });
        assert_eq!(validate(&e, &json!("b"), "x"), [r#"x must be one of [1,"a",null]"#]);
        assert!(validate(&e, &json!(1), "x").is_empty());
        let one = json!({ "oneOf": [{ "type": "number" }, { "type": "integer" }] });
        assert_eq!(validate(&one, &json!(1), "x"), ["x must match oneOf"]);
        assert!(validate(&one, &json!(1.5), "x").is_empty());
        let all = json!({ "allOf": [{ "type": "number" }, { "minimum": 2 }] });
        assert_eq!(validate(&all, &json!(1), "x"), ["x must match allOf"]);
        // Items and schema-valued additionalProperties carry the path.
        let nested = json!({ "items": { "type": "object", "additionalProperties": { "type": "string" } } });
        assert_eq!(
            validate(&nested, &json!([{}, { "k": 1 }]), "v"),
            ["v[1].k must be string, got number"]
        );
    }

    #[test]
    fn errors_come_in_ts_order() {
        // required, then properties in JS key order (integer-like keys first), then the bounds.
        let schema = json!({
            "required": ["z"],
            "properties": { "b": { "type": "string" }, "1": { "type": "string" } },
            "minProperties": 5
        });
        assert_eq!(
            validate(&schema, &json!({ "b": 1, "1": 2 }), "o"),
            [
                "o.z is required",
                "o.1 must be string, got number",
                "o.b must be string, got number",
                "o violates minProperties 5"
            ]
        );
        let s = json!({ "minLength": 5, "pattern": "^[0-9]+$", "enum": ["x"] });
        assert_eq!(
            validate(&s, &json!("ab"), "s"),
            [
                r#"s must be one of ["x"]"#,
                "s violates minLength 5",
                "s does not match pattern"
            ]
        );
        let n = json!({ "minimum": 5, "exclusiveMinimum": 5 });
        assert_eq!(
            validate(&n, &json!(1), "n"),
            ["n violates minimum 5", "n violates exclusiveMinimum"]
        );
    }

    #[test]
    fn patterns_are_js_unicode_regexes() {
        let digits = json!({ "pattern": r"^\d+$" });
        assert!(validate(&digits, &json!("123"), "p").is_empty());
        // JS \d is ASCII even with the u flag: fullwidth digits do not match.
        assert_eq!(validate(&digits, &json!("１２３"), "p"), ["p does not match pattern"]);
        assert_eq!(
            validate(&json!({ "pattern": r"^\w+$" }), &json!("é"), "p"),
            ["p does not match pattern"]
        );
        // `.` stops at \r as well as \n.
        assert_eq!(
            validate(&json!({ "pattern": "^a.b$" }), &json!("a\rb"), "p"),
            ["p does not match pattern"]
        );
        assert!(validate(&json!({ "pattern": r"^\u{1F600}$" }), &json!("😀"), "p").is_empty());
        assert!(validate(&json!({ "pattern": r"^\uD83D\uDE00$" }), &json!("😀"), "p").is_empty());
        // Lookaround and backreferences go through fancy-regex.
        assert!(validate(&json!({ "pattern": r"^(?=a)(\w)\1$" }), &json!("aa"), "p").is_empty());
        assert_eq!(
            validate(&json!({ "pattern": r"(?<!x)y" }), &json!("xy"), "p"),
            ["p does not match pattern"]
        );
        assert_eq!(
            validate(&json!({ "pattern": "[" }), &json!("a"), "p"),
            ["p has an invalid schema pattern"]
        );
        // Unanchored, like RegExp.test.
        assert!(validate(&json!({ "pattern": "b" }), &json!("abc"), "p").is_empty());
    }

    #[test]
    fn non_object_schemas_have_no_keywords() {
        assert!(validate(&json!(true), &json!(1), "x").is_empty());
        assert!(validate(&json!({ "items": true }), &json!([1, "a"]), "x").is_empty());
        assert!(validate(&json!({ "properties": null }), &json!({ "a": 1 }), "x").is_empty());
    }

    // New: [schema, value, errors] produced by the TS validate() (bun, packages/agent/src/validate.ts).
    const TS_ORACLE: &str = r#"[[{"type":"integer","minimum":1},2,[]],[{"type":"integer","minimum":1},1.5,["x must be integer, got number"]],[{"type":"integer","minimum":1},0,["x violates minimum 1"]],[{"maximum":0.5},1,["x violates maximum 0.5"]],[{"minLength":3,"maxLength":2},"\ud83d\ude00\u3042",["x violates minLength 3"]],[{"minLength":3,"maxLength":2},"\ud83d\ude00\u3042\u3044",["x violates maxLength 2"]],[{"type":["null","string"]},true,["x must be null or string, got boolean"]],[{"const":[1]},{"0":1},["x must match const"]],[{"enum":[1,"a",null]},"b",["x must be one of [1,\"a\",null]"]],[{"oneOf":[{"type":"number"},{"type":"integer"}]},1,["x must match oneOf"]],[{"allOf":[{"type":"number"},{"minimum":2}]},1,["x must match allOf"]],[{"items":{"type":"object","additionalProperties":{"type":"string"}}},[{},{"k":1}],["x[1].k must be string, got number"]],[{"required":["z"],"properties":{"1":{"type":"string"},"b":{"type":"string"}},"minProperties":5},{"1":2,"b":1},["x.z is required","x.1 must be string, got number","x.b must be string, got number","x violates minProperties 5"]],[{"minLength":5,"pattern":"^[0-9]+$","enum":["x"]},"ab",["x must be one of [\"x\"]","x violates minLength 5","x does not match pattern"]],[{"minimum":5,"exclusiveMinimum":5},1,["x violates minimum 5","x violates exclusiveMinimum"]],[{"pattern":"^\\d+$"},"\uff11\uff12\uff13",["x does not match pattern"]],[{"pattern":"^\\w+$"},"\u00e9",["x does not match pattern"]],[{"pattern":"^a.b$"},"a\rb",["x does not match pattern"]],[{"pattern":"^\\u{1F600}$"},"\ud83d\ude00",[]],[{"pattern":"^\\uD83D\\uDE00$"},"\ud83d\ude00",[]],[{"pattern":"^(?=a)(\\w)\\1$"},"aa",[]],[{"pattern":"(?<!x)y"},"xy",["x does not match pattern"]],[{"pattern":"["},"a",["x has an invalid schema pattern"]],[{"pattern":"\\-"},"-",["x has an invalid schema pattern"]],[{"pattern":"[\\-a]"},"-",[]],[{"pattern":"^[^]$"},"\n",[]],[{"pattern":"^[]$"},"",["x does not match pattern"]],[{"pattern":"\\b\u00e9"},"a\u00e9",[]],[{"pattern":"^\\s$"},"\ufeff",[]],[{"pattern":"^\\s$"},"\u0085",["x does not match pattern"]],[{"pattern":"^[\\d\\s]+$"},"1 2",[]],[{"pattern":"^\\cJ$"},"\n",[]],[{"type":[]},1,["x must be , got number"]],[{"type":""},1,[]],[{"enum":[{"b":1.5,"a":[2]}]},3,["x must be one of [{\"b\":1.5,\"a\":[2]}]"]],[{"maximum":1e+21},1e+22,["x violates maximum 1e+21"]],[{"maximum":1.5e-7},1,["x violates maximum 1.5e-7"]],[{"type":"object","properties":{"a":{"type":"integer"}}},{"a":1e+300},[]]]"#;

    #[test]
    fn matches_the_ts_oracle() {
        let cases: Vec<(Value, Value, Vec<String>)> = serde_json::from_str(TS_ORACLE).unwrap();
        for (schema, value, expected) in cases {
            assert_eq!(
                validate(&schema, &value, "x"),
                expected,
                "schema {schema} value {value}"
            );
        }
    }

    #[test]
    fn js_number_strings() {
        use sasacode_ai::json::js_f64_string as js_number_string;
        assert_eq!(js_number_string(1.0), "1");
        assert_eq!(js_number_string(0.3), "0.3");
        assert_eq!(js_number_string(-2.5), "-2.5");
        assert_eq!(js_number_string(1e21), "1e+21");
        assert_eq!(js_number_string(1.5e-7), "1.5e-7");
        assert_eq!(js_number_string(0.0000015), "0.0000015");
        assert_eq!(js_number_string(123456789012.0), "123456789012");
    }
}
