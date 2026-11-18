//! Small JS-semantics helpers shared by the web, usage, image and job plugins: string lengths and slices in UTF-16 units, JS whitespace,
//! `Number(...)` of a setting, and `Math.round`.

use serde_json::Value;

/// `s.length` in JS: UTF-16 code units.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// The byte offset of UTF-16 offset `at` in `s`, moved back to the start of a character it falls
/// inside (a surrogate pair stays whole). Past the end: `s.len()`.
pub(crate) fn utf16_byte(s: &str, at: usize) -> usize {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= at {
            return i;
        }
        units += c.len_utf16();
        if units > at {
            return i;
        }
    }
    s.len()
}

/// `s.slice(start, end)` with UTF-16 offsets (clamped), cut on character boundaries.
pub(crate) fn utf16_slice(s: &str, start: usize, end: usize) -> &str {
    let a = utf16_byte(s, start);
    let b = utf16_byte(s, end.max(start));
    &s[a..b.max(a)]
}

/// `s.slice(0, n)` with a UTF-16 count.
pub(crate) fn utf16_prefix(s: &str, n: usize) -> &str {
    &s[..utf16_byte(s, n)]
}

/// JS `\s`: whitespace and line terminators, the BOM included (U+0085 is not one).
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
    )
}

/// JS `trim()`.
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// `s.trim().split(/\s+/)` without the empty word an empty string gives.
pub(crate) fn js_words(s: &str) -> Vec<&str> {
    s.split(is_js_space).filter(|w| !w.is_empty()).collect()
}

/// `s.replace(/\s+/g, " ")`.
pub(crate) fn collapse_space(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if is_js_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// JS `Number(v)` of a setting that is not undefined: NaN for what is not a number.
pub(crate) fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => {
            let t = js_trim(s);
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Value::Array(a) if a.is_empty() => 0.0,
        Value::Array(a) if a.len() == 1 => js_number(&a[0]),
        _ => f64::NAN,
    }
}

/// `Number(settings[key] ?? fallback)`.
pub(crate) fn setting_number(settings: &Value, key: &str, fallback: f64) -> f64 {
    match settings.get(key) {
        None | Some(Value::Null) => fallback,
        Some(v) => js_number(v),
    }
}

/// `Math.round`: halves go up (towards +∞).
pub(crate) fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// A JS number as `String(n)` prints it.
pub(crate) fn js_num_str(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    sasacode_ai::json::js_f64_string(x)
}

/// A string setting (`settings[key] as string | undefined`), only when it is a string.
pub(crate) fn setting_str(settings: &Value, key: &str) -> Option<String> {
    settings.get(key).and_then(Value::as_str).map(str::to_string)
}

/// `Number.prototype.toFixed` for a positive finite value: the exact binary value rounded half up
/// (Rust's `{:.2}` rounds an exact tie to even).
pub(crate) fn to_fixed(x: f64, digits: usize) -> String {
    let exact = format!("{x:.*}", digits + 80);
    let (int_part, frac) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut kept: Vec<u8> = int_part.bytes().chain(frac.bytes().take(digits)).collect();
    if frac.as_bytes().get(digits).is_some_and(|d| *d >= b'5') {
        let mut i = kept.len();
        loop {
            if i == 0 {
                kept.insert(0, b'1');
                break;
            }
            i -= 1;
            if kept[i] == b'9' {
                kept[i] = b'0';
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    let (a, b) = kept.split_at(kept.len() - digits);
    let (a, b) = (String::from_utf8_lossy(a), String::from_utf8_lossy(b));
    if digits == 0 {
        a.into_owned()
    } else {
        format!("{a}.{b}")
    }
}

/// `Buffer.from(s, "base64")`: either alphabet, padding optional, other characters skipped, and a
/// dangling last character (no whole byte) dropped.
pub(crate) fn decode_base64(s: &str) -> Vec<u8> {
    use base64::Engine;
    let mut clean: String = s
        .chars()
        .filter_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' => Some(c),
            '-' => Some('+'),
            '_' => Some('/'),
            _ => None,
        })
        .collect();
    if clean.len() % 4 == 1 {
        clean.pop();
    }
    let engine = base64::engine::GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
    );
    engine.decode(clean.as_bytes()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_slices_keep_characters_whole() {
        assert_eq!(utf16_len("a😀b"), 4);
        assert_eq!(utf16_slice("abcdef", 2, 4), "cd");
        assert_eq!(utf16_slice("日本語です", 1, 3), "本語");
        assert_eq!(utf16_prefix("a😀b", 2), "a");
        assert_eq!(utf16_prefix("a😀b", 3), "a😀");
        assert_eq!(utf16_slice("abc", 5, 9), "");
        assert_eq!(utf16_slice("abc", 2, 1), "");
    }

    #[test]
    fn js_whitespace_and_numbers() {
        assert_eq!(js_trim("\u{feff} x \u{3000}"), "x");
        assert_eq!(js_words("  a  b\tc "), vec!["a", "b", "c"]);
        assert!(js_words("   ").is_empty());
        assert_eq!(collapse_space("a \n b"), "a b");
        assert_eq!(js_number(&serde_json::json!("12")), 12.0);
        assert!(js_number(&serde_json::json!("12px")).is_nan());
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_num_str(10.0), "10");
        assert_eq!(to_fixed(1.125, 2), "1.13");
        assert_eq!(to_fixed(0.0123, 3), "0.012");
        assert_eq!(decode_base64("aGk"), b"hi");
        assert_eq!(decode_base64("aGk=\n"), b"hi");
    }
}
