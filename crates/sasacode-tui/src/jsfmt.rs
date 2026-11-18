//! Numbers printed as the TypeScript printed them.

/// `Number.prototype.toFixed` for a finite value: the exact binary value rounded half up (Rust's
/// `{:.1}` rounds an exact tie to even: 1.25 gives "1.2" where JS gives "1.3").
pub fn to_fixed(x: f64, digits: usize) -> String {
    if !x.is_finite() {
        return x.to_string();
    }
    let negative = x < 0.0;
    // A double's fraction ends within 1074 binary places; 80 decimal places hold every value this
    // UI prints (costs, rates, counts) exactly.
    let exact = format!("{:.*}", digits + 80, x.abs());
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
    let split = kept.len() - digits;
    let (a, b) = kept.split_at(split);
    let (a, b) = (String::from_utf8_lossy(a), String::from_utf8_lossy(b));
    let body = if digits == 0 {
        a.into_owned()
    } else {
        format!("{a}.{b}")
    };
    let zero = body.bytes().all(|c| c == b'0' || c == b'.');
    if negative && !zero { format!("-{body}") } else { body }
}

/// `Math.round`: half toward +∞.
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// `n.toLocaleString("en-US")` for a whole number: thousands grouped with commas.
pub fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_rounds_ties_up_like_js() {
        assert_eq!(to_fixed(1.25, 1), "1.3");
        assert_eq!(to_fixed(3.24, 1), "3.2");
        assert_eq!(to_fixed(0.0125, 3), "0.013");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(0.0, 3), "0.000");
        assert_eq!(to_fixed(9.96, 1), "10.0");
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(1234), "1,234");
        assert_eq!(group_thousands(1234567), "1,234,567");
    }
}
