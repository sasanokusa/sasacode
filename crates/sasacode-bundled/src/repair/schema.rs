//! Bring parsed arguments in line with a tool's JSON Schema, but only where the intent is
//! unambiguous: one candidate key, one obvious type conversion (`repair/schema.ts`). Anything else
//! is left for validation to report.

use serde_json::Value;

use sasacode_ai::JsonObject;
use sasacode_plugin_api::validate::is_integer;

use super::Repaired;
use super::js::{js_string, js_trim, utf16_prefix};

/// Names and their alternative spellings, in the order the TS object literal listed them.
pub type Synonyms<'a> = &'a [(&'a str, &'a [&'a str])];

/// Common alternative names small models use for well-known argument names.
const SYNONYMS: Synonyms<'static> = &[
    (
        "path",
        &[
            "file",
            "filepath",
            "filename",
            "file_name",
            "filePath",
            "file_path",
            "target",
            "target_file",
        ],
    ),
    ("command", &["cmd", "shell", "script", "bash", "commandLine"]),
    (
        "old_string",
        &[
            "old", "old_str", "oldText", "old_text", "search", "find", "from", "original",
        ],
    ),
    (
        "new_string",
        &["new", "new_str", "newText", "new_text", "replace", "replacement", "to"],
    ),
    (
        "content",
        &["contents", "text", "data", "body", "file_content", "file_text", "code"],
    ),
    ("query", &["q", "search", "keywords", "search_query", "term"]),
    ("url", &["link", "uri", "href", "address"]),
];

/// Names other harnesses give the four built-in tools, which small models carry over (Cline,
/// Cursor, Gemini CLI, Codex, Anthropic's text editor, SWE-agent). Compared like closest_name does,
/// so `read_file`, `readFile` and `read-file` are one entry. Each spelling is listed under one tool
/// only; words that name other things too (`open`, `run`, `create`, `replace`) are left out.
pub const TOOL_SYNONYMS: Synonyms<'static> = &[
    (
        "read",
        &[
            "read_file",
            "view_file",
            "open_file",
            "file_read",
            "read_text_file",
            "cat",
            "view",
        ],
    ),
    (
        "write",
        &[
            "write_file",
            "create_file",
            "save_file",
            "file_write",
            "write_to_file",
            "new_file",
        ],
    ),
    (
        "edit",
        &[
            "edit_file",
            "file_edit",
            "str_replace",
            "str_replace_editor",
            "str_replace_based_edit_tool",
            "replace_in_file",
            "apply_edit",
            "search_replace",
        ],
    ),
    (
        "bash",
        &[
            "run_command",
            "execute_command",
            "execute_bash",
            "run_shell_command",
            "run_terminal_cmd",
            "shell",
            "local_shell",
            "terminal",
            "exec",
            "bash_command",
        ],
    ),
];

/// `s.toLowerCase().replace(/[^a-z0-9]/g, "")`.
fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

/// Edit distance where swapping two neighbouring characters counts as one edit (raed → read).
/// Counted in UTF-16 units, as the TS indexed strings.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<u16> = a.encode_utf16().collect();
    let b: Vec<u16> = b.encode_utf16().collect();
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[a.len()][b.len()]
}

/// The single best match for `name` among `candidates`, or None when none or several fit.
/// `typos: false` skips the edit-distance pass.
pub fn closest_name<S: AsRef<str>>(name: &str, candidates: &[S], synonyms: Synonyms, typos: bool) -> Option<String> {
    let n = norm(name);
    let pick = |hits: Vec<&str>| (hits.len() == 1).then(|| hits[0].to_string());
    let exact: Vec<&str> = candidates.iter().map(AsRef::as_ref).filter(|c| norm(c) == n).collect();
    if !exact.is_empty() {
        return pick(exact);
    }
    let syn: Vec<&str> = candidates
        .iter()
        .map(AsRef::as_ref)
        .filter(|c| {
            synonyms
                .iter()
                .find(|(k, _)| k == c)
                .is_some_and(|(_, list)| list.iter().any(|s| norm(s) == n))
        })
        .collect();
    if !syn.is_empty() || !typos {
        return pick(syn);
    }
    let limit = if n.len() < 5 { 1 } else { 2 };
    let scored: Vec<(&str, usize)> = candidates
        .iter()
        .map(|c| (c.as_ref(), edit_distance(&n, &norm(c.as_ref()))))
        .filter(|(_, d)| *d <= limit)
        .collect();
    let best = scored.iter().map(|(_, d)| *d).min()?;
    pick(scored.iter().filter(|(_, d)| *d == best).map(|(c, _)| *c).collect())
}

fn types_of(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::Array(a)) => a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect(),
        Some(Value::String(t)) => vec![t.clone()],
        _ => Vec::new(),
    }
}

fn matches(t: &str, v: &Value) -> bool {
    match t {
        "integer" => is_integer(v),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        "string" => v.is_string(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        _ => false,
    }
}

/// What may be done beyond renaming and converting.
#[derive(Debug, Clone, Copy, Default)]
pub struct FitOptions {
    /// Drop arguments the schema does not know. Only safe where ignoring a constraint cannot change
    /// what happens.
    pub drop_unknown: bool,
}

/// `/^\s*-?\d+(\.\d+)?\s*$/`.
fn numeric(s: &str) -> bool {
    let t = js_trim(s);
    let t = t.strip_prefix('-').unwrap_or(t);
    let (int, frac) = match t.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (t, None),
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    digits(int) && frac.is_none_or(digits)
}

/// Convert one value toward the schema's type. Returns the value unchanged when no safe conversion exists.
fn convert(v: &Value, schema: &Value, at: &str, fixes: &mut Vec<String>, o: FitOptions) -> Value {
    let types = types_of(schema);
    if let (Some(Value::Array(options)), Value::String(s)) = (schema.get("enum"), v)
        && !options.iter().any(|e| e.as_str() == Some(s))
    {
        let lower = s.to_lowercase();
        let hit: Vec<&str> = options
            .iter()
            .filter_map(Value::as_str)
            .filter(|e| e.to_lowercase() == lower)
            .collect();
        if hit.len() == 1 {
            fixes.push(format!("{at}: \"{s}\" → \"{}\"", hit[0]));
            return Value::String(hit[0].to_string());
        }
    }
    if types.is_empty() || types.iter().any(|t| matches(t, v)) {
        return if types.iter().any(|t| t == "object" || t == "array") {
            recurse(v.clone(), schema, at, fixes, o)
        } else {
            v.clone()
        };
    }
    let show = |x: &Value| utf16_prefix(&sasacode_ai::json::stringify(x), 40);
    for t in &types {
        let out: Option<Value> = match (t.as_str(), v) {
            ("number" | "integer", Value::String(s)) if numeric(s) => {
                js_trim(s).parse::<f64>().ok().map(sasacode_ai::json::js_number)
            }
            ("boolean", Value::String(s))
                if js_trim(s).eq_ignore_ascii_case("true") || js_trim(s).eq_ignore_ascii_case("false") =>
            {
                Some(Value::Bool(js_trim(s).eq_ignore_ascii_case("true")))
            }
            ("string", Value::Number(_) | Value::Bool(_)) => Some(Value::String(js_string(v))),
            ("object" | "array", Value::String(s)) => serde_json::from_str::<Value>(s).ok(),
            ("array", x) if !x.is_null() => Some(Value::Array(vec![x.clone()])),
            _ => None,
        };
        if let Some(out) = out
            && matches(t, &out)
        {
            fixes.push(format!("{at}: {} → {}", show(v), show(&out)));
            return recurse(out, schema, at, fixes, o);
        }
    }
    v.clone()
}

fn recurse(v: Value, schema: &Value, at: &str, fixes: &mut Vec<String>, o: FitOptions) -> Value {
    if let (Value::Array(items), Some(item_schema)) = (&v, schema.get("items"))
        && super::js::truthy(item_schema)
    {
        return Value::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, x)| convert(x, item_schema, &format!("{at}[{i}]"), fixes, o))
                .collect(),
        );
    }
    if let (Value::Object(obj), Some(props)) = (&v, schema.get("properties"))
        && super::js::truthy(props)
    {
        return Value::Object(fix_object(obj, schema, &format!("{at}."), fixes, o));
    }
    v
}

fn fix_object(input: &JsonObject, schema: &Value, prefix: &str, fixes: &mut Vec<String>, o: FitOptions) -> JsonObject {
    let empty = JsonObject::new();
    let props = schema.get("properties").and_then(Value::as_object).unwrap_or(&empty);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut out = JsonObject::new();
    let unknown: Vec<&String> = input.keys().filter(|k| !props.contains_key(*k)).collect();
    for (k, v) in input {
        if props.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    for k in unknown {
        let free: Vec<&str> = props
            .keys()
            .map(String::as_str)
            .filter(|p| !out.contains_key(*p))
            .collect();
        let free_required: Vec<&str> = free.iter().copied().filter(|p| required.contains(p)).collect();
        // Prefer filling a missing required argument over an optional one.
        let target = closest_name(k, &free_required, SYNONYMS, true).or_else(|| closest_name(k, &free, SYNONYMS, true));
        if let Some(target) = target {
            fixes.push(format!("argument \"{prefix}{k}\" → \"{prefix}{target}\""));
            out.insert(target, input[k].clone());
        } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) && o.drop_unknown {
            fixes.push(format!("dropped unknown argument \"{prefix}{k}\""));
        } else {
            // Kept: validation reports it and the model decides.
            out.insert(k.clone(), input[k].clone());
        }
    }
    let keys: Vec<String> = out.keys().cloned().collect();
    for k in keys {
        let v = out[&k].clone();
        let prop = props.get(&k);
        if v.is_null()
            && !required.contains(&k.as_str())
            && !types_of(prop.unwrap_or(&Value::Null)).iter().any(|t| t == "null")
        {
            out.shift_remove(&k);
            fixes.push(format!("dropped null \"{prefix}{k}\""));
        } else if let Some(p) = prop {
            let converted = convert(&v, p, &format!("{prefix}{k}"), fixes, o);
            out.insert(k, converted);
        }
    }
    out
}

/// Rename and convert arguments so they fit `schema`; each change is listed in `fixes`.
/// Unknown arguments are dropped only with `drop_unknown` (read-only tools): on a tool that changes
/// things, an argument like `overwrite: false` may be a constraint the model meant, so it is left
/// for validation to report instead of being silently ignored.
pub fn fit_to_schema(input: &JsonObject, schema: &Value, opts: FitOptions) -> Repaired<JsonObject> {
    let mut fixes = Vec::new();
    let value = fix_object(input, schema, "", &mut fixes, opts);
    Repaired { value, fixes }
}
