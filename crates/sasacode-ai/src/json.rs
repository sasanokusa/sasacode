//! JSON the way the TypeScript version wrote it (rust/DECISIONS.md ADR-003).
//!
//! serde_json already matches `JSON.stringify` for strings, key order (`preserve_order`) and
//! compact layout. Numbers differ: serde writes an `f64` that holds an integer as `1.0`, where JS
//! writes `1`. Floats go through [`js_number`] / [`js_f64`] so whole values come out as integers.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// A JSON number as JS would print it: an integral value without a fraction.
pub fn js_number(v: f64) -> Value {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 9_007_199_254_740_992.0 {
        Value::from(v as i64)
    } else if v.is_finite() {
        Value::from(v)
    } else {
        // JSON.stringify writes NaN and Infinity as null.
        Value::Null
    }
}

/// `JSON.stringify(value)`: compact, keys in insertion order, numbers as JS prints them.
pub fn stringify(value: &impl Serialize) -> String {
    to_js_string(value).unwrap_or_else(|_| "null".to_string())
}

/// `JSON.stringify(value, null, 2)`.
pub fn stringify_pretty(value: &impl Serialize) -> String {
    write_with(value, JsFormatter(serde_json::ser::PrettyFormatter::with_indent(b"  ")))
        .unwrap_or_else(|_| "null".to_string())
}

/// [`stringify`] that reports a value serde cannot serialize (a map with non-string keys).
pub fn to_js_string(value: &impl Serialize) -> serde_json::Result<String> {
    write_with(value, JsFormatter(serde_json::ser::CompactFormatter))
}

fn write_with<F: serde_json::ser::Formatter>(value: &impl Serialize, f: F) -> serde_json::Result<String> {
    let mut out = Vec::with_capacity(128);
    let mut ser = serde_json::Serializer::with_formatter(&mut out, f);
    value.serialize(&mut ser)?;
    // INFALLIBLE: serde_json writes only UTF-8.
    Ok(String::from_utf8(out).unwrap_or_default())
}

/// `Number.prototype.toString()` for a finite f64 (ECMAScript Number::toString, radix 10):
/// shortest round-trip digits, plain notation from 1e-6 up to 1e21, exponent form outside.
pub fn js_f64_string(v: f64) -> String {
    if !v.is_finite() {
        return "null".to_string();
    }
    if v == 0.0 {
        return "0".to_string();
    }
    let sci = format!("{:e}", v.abs());
    let (mantissa, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1;
    let mut out = String::new();
    if v < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.push_str(&"0".repeat((n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat((-n) as usize));
        out.push_str(&digits);
    } else {
        let e = n - 1;
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if e >= 0 { '+' } else { '-' });
        out.push_str(&e.abs().to_string());
    }
    out
}

/// A serde_json formatter that writes floats as JS does and leaves the layout to `F`.
pub struct JsFormatter<F>(pub F);

impl<F: serde_json::ser::Formatter> serde_json::ser::Formatter for JsFormatter<F> {
    fn write_f64<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: f64) -> std::io::Result<()> {
        w.write_all(js_f64_string(v).as_bytes())
    }
    fn write_f32<W: ?Sized + std::io::Write>(&mut self, w: &mut W, v: f32) -> std::io::Result<()> {
        w.write_all(js_f64_string(v as f64).as_bytes())
    }
    fn begin_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_array(w)
    }
    fn end_array<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_array(w)
    }
    fn begin_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        self.0.begin_array_value(w, first)
    }
    fn end_array_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_array_value(w)
    }
    fn begin_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_object(w)
    }
    fn end_object<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object(w)
    }
    fn begin_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W, first: bool) -> std::io::Result<()> {
        self.0.begin_object_key(w, first)
    }
    fn end_object_key<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object_key(w)
    }
    fn begin_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.begin_object_value(w)
    }
    fn end_object_value<W: ?Sized + std::io::Write>(&mut self, w: &mut W) -> std::io::Result<()> {
        self.0.end_object_value(w)
    }
}

/// `#[serde(with = "js_f64")]` for an `f64` field written like a JS number.
pub mod js_f64 {
    use super::*;

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        js_number(*v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        Ok(Option::<f64>::deserialize(d)?.unwrap_or(0.0))
    }
}

/// `#[serde(with = "double_option")]` for a field that may be absent, `null`, or a value.
/// Use with `default` and `skip_serializing_if = "Option::is_none"`.
pub mod double_option {
    use super::*;

    pub fn serialize<S: Serializer, T: Serialize>(v: &Option<Option<T>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
        Ok(Some(Option::<T>::deserialize(d)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integral_floats_print_without_fraction() {
        assert_eq!(js_number(0.0).to_string(), "0");
        assert_eq!(js_number(3.0).to_string(), "3");
        assert_eq!(js_number(0.25).to_string(), "0.25");
        assert_eq!(js_number(f64::NAN).to_string(), "null");
    }

    #[test]
    fn floats_print_like_number_to_string() {
        // Checked against bun: String(x) for each.
        for (v, js) in [
            (0.0000035, "0.0000035"),
            (0.000001, "0.000001"),
            (0.0000001, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (1e17, "100000000000000000"),
            (1e21, "1e+21"),
            (1.2345e21, "1.2345e+21"),
            (123.456, "123.456"),
            (-0.5, "-0.5"),
            (0.1 + 0.2, "0.30000000000000004"),
            (-0.0, "0"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
        ] {
            assert_eq!(js_f64_string(v), js, "{v:e}");
        }
    }

    #[test]
    fn stringify_matches_json_stringify() {
        let v: Value = serde_json::from_str(r#"{"a":1.0,"b":[0.0000035,1e17,{}],"c":[],"d":"x"}"#).unwrap();
        assert_eq!(
            stringify(&v),
            r#"{"a":1,"b":[0.0000035,100000000000000000,{}],"c":[],"d":"x"}"#
        );
        assert_eq!(
            stringify_pretty(&v),
            "{\n  \"a\": 1,\n  \"b\": [\n    0.0000035,\n    100000000000000000,\n    {}\n  ],\n  \"c\": [],\n  \"d\": \"x\"\n}"
        );
    }
}
