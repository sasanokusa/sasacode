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

/// `JSON.stringify(value)`: compact, keys in insertion order.
pub fn stringify(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
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
}
