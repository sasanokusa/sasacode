//! Lenient JSON for tool arguments written by small models (`repair/json.ts`). One pass over the
//! text that knows whether it is inside a string, so fixes never touch string contents.

use serde_json::Value;

use sasacode_ai::JsonObject;

use super::Repaired;
use super::js::{js_space, js_trim};

/// Python-style literals and their JSON spelling.
fn py_literal(word: &str) -> Option<&'static str> {
    match word {
        "True" => Some("true"),
        "False" => Some("false"),
        "None" => Some("null"),
        "undefined" => Some("null"),
        _ => None,
    }
}

/// Insertion-ordered set of fixes (the TS used a Set).
#[derive(Default)]
struct Fixes(Vec<String>);

impl Fixes {
    fn add(&mut self, f: impl Into<String>) {
        let f = f.into();
        if !self.0.contains(&f) {
            self.0.push(f);
        }
    }
}

fn parse(s: &str) -> Option<Value> {
    serde_json::from_str(s).ok()
}

/// "```lang\n…\n```" around the whole text: the inside, trimmed.
fn strip_fence(src: &str) -> Option<&str> {
    let rest = src.strip_prefix("```")?;
    let body = rest.strip_suffix("```")?;
    let lang = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(body.len());
    Some(body[lang..].trim_matches(js_space))
}

/// Repair and parse `raw` as a JSON object. None when it cannot be made into one.
pub fn repair_json(raw: &str) -> Option<Repaired<JsonObject>> {
    let mut fixes = Fixes::default();
    let mut src: &str = js_trim(raw);

    // Arguments sometimes arrive as a JSON string holding the JSON object.
    match parse(src) {
        Some(Value::String(inner)) => {
            let inner = repair_json(&inner)?;
            let mut all = vec!["decoded double-encoded JSON".to_string()];
            all.extend(inner.fixes);
            return Some(Repaired {
                value: inner.value,
                fixes: all,
            });
        }
        Some(Value::Object(o)) => {
            return Some(Repaired {
                value: o,
                fixes: Vec::new(),
            });
        }
        _ => {}
    }

    if let Some(inner) = strip_fence(src) {
        src = inner;
        fixes.add("removed code fence");
    }
    let start = src.find(['{', '['])?;
    if start > 0 {
        src = &src[start..];
        fixes.add("dropped text before the JSON");
    }

    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<char> = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut end = src.len();

    let drop_trailing_comma = |out: &mut Vec<String>, fixes: &mut Fixes| {
        for j in (0..out.len()).rev() {
            if out[j].chars().any(js_space) {
                continue;
            }
            if out[j] == "," {
                out.remove(j);
                fixes.add("removed trailing comma");
            }
            return;
        }
    };

    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut k = 0;
    while k < chars.len() {
        let (i, ch) = chars[k];
        k += 1;
        if let Some(q) = quote {
            if escaped {
                escaped = false;
                // \' is not a JSON escape; inside a converted single-quoted string it is just a quote.
                out.push(if ch == '\'' && q == '\'' {
                    "'".to_string()
                } else {
                    format!("\\{ch}")
                });
            } else if ch == '\\' {
                escaped = true;
            } else if ch == q {
                quote = None;
                out.push("\"".into());
            } else if ch == '"' {
                out.push("\\\"".into());
            } else if ch == '\n' || ch == '\r' || ch == '\t' {
                out.push(
                    match ch {
                        '\n' => "\\n",
                        '\r' => "\\r",
                        _ => "\\t",
                    }
                    .into(),
                );
                fixes.add("escaped control characters in strings");
            } else {
                out.push(ch.to_string());
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            if ch == '\'' {
                fixes.add("converted single quotes");
            }
            out.push("\"".into());
        } else if ch == '{' || ch == '[' {
            stack.push(if ch == '{' { '}' } else { ']' });
            out.push(ch.to_string());
        } else if ch == '}' || ch == ']' {
            drop_trailing_comma(&mut out, &mut fixes);
            stack.pop();
            out.push(ch.to_string());
            if stack.is_empty() {
                end = i + 1;
                break;
            }
        } else if ch.is_ascii_alphabetic() || ch == '_' || ch == '$' {
            // [A-Za-z_$][\w$-]*
            let rest = &src[i..];
            let len = rest
                .char_indices()
                .skip(1)
                .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '$' || *c == '-'))
                .map_or(rest.len(), |(at, _)| at);
            let word = &rest[..len];
            let after = rest[len..].trim_start_matches(js_space);
            if after.starts_with(':') {
                out.push(sasacode_ai::json::stringify(&word));
                fixes.add("quoted keys");
            } else if let Some(lit) = py_literal(word) {
                out.push(lit.to_string());
                fixes.add(format!("{word} → {lit}"));
            } else {
                out.push(word.to_string());
            }
            // The word is ASCII: one char per byte.
            k += len - 1;
        } else {
            out.push(ch.to_string());
        }
    }

    if end < src.len() && !js_trim(&src[end..]).is_empty() {
        fixes.add("dropped text after the JSON");
    }
    if quote.is_some() {
        out.push("\"".into());
        fixes.add("closed an unterminated string");
    }
    if !stack.is_empty() {
        drop_trailing_comma(&mut out, &mut fixes);
        out.extend(stack.iter().rev().map(|c| c.to_string()));
        fixes.add("closed brackets");
    }

    match parse(&out.concat()) {
        Some(Value::Object(value)) => Some(Repaired { value, fixes: fixes.0 }),
        _ => None,
    }
}
