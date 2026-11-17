//! Token counts and context sizes, the same everywhere (`packages/ai/src/format.ts`).

/// 1534 → "1.5k", 12_345 → "12k", 131072 → "128k" (windows are often powers of two),
/// 1_050_000 → "1.05M", 2_300_000_000 → "2.3B" (`fmtTokens`).
pub fn fmt_tokens(n: u64) -> String {
    let x = n as f64;
    if x >= 1e9 {
        return format!("{}B", trim(&to_fixed(x / 1e9, 2)));
    }
    if x >= 1e6 {
        return format!("{}M", trim(&to_fixed(x / 1e6, if x < 1e7 { 2 } else { 1 })));
    }
    if n >= 1024 && !n.is_multiple_of(1000) && n.is_multiple_of(1024) {
        return format!("{}k", n / 1024);
    }
    if n >= 1000 {
        return format!("{}k", trim(&to_fixed(x / 1000.0, if n < 10_000 { 1 } else { 0 })));
    }
    n.to_string()
}

/// `Number.prototype.toFixed` for a positive finite value: rounds the exact binary value half up
/// (Rust's `{:.1}` rounds an exact tie to even: 1.25 → "1.2" where JS gives "1.3").
fn to_fixed(x: f64, digits: usize) -> String {
    // Every fraction digit of a double ≥ 1 fits in 60 places, so this expansion is exact.
    let exact = format!("{x:.*}", digits + 60);
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
    if digits == 0 {
        a.into_owned()
    } else {
        format!("{a}.{b}")
    }
}

/// `+s` printed again: trailing fraction zeros (and a bare point) go.
fn trim(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values printed by the TS `fmtTokens`, ties and near-ties included.
    #[test]
    fn matches_the_typescript() {
        let cases: [(u64, &str); 32] = [
            (0, "0"),
            (999, "999"),
            (1000, "1k"),
            (1024, "1k"),
            (1250, "1.3k"),
            (1534, "1.5k"),
            (1950, "1.9k"),
            (9950, "9.9k"),
            (9999, "10k"),
            (10_000, "10k"),
            (12_345, "12k"),
            (12_500, "13k"),
            (131_072, "128k"),
            (200_000, "200k"),
            (999_999, "1000k"),
            (1_000_000, "1M"),
            (1_005_000, "1M"),
            (1_050_000, "1.05M"),
            (1_234_567, "1.23M"),
            (9_995_000, "9.99M"),
            (9_999_999, "10M"),
            (10_000_000, "10M"),
            (10_050_000, "10.1M"),
            (12_345_678, "12.3M"),
            (1_000_000_000, "1B"),
            (1_005_000_000, "1B"),
            (2_300_000_000, "2.3B"),
            (2_305_000_000, "2.31B"),
            (3072, "3k"),
            (2_048_000, "2.05M"),
            (1_048_576, "1.05M"),
            (1125, "1.1k"),
        ];
        for (n, want) in cases {
            assert_eq!(fmt_tokens(n), want, "{n}");
        }
    }
}
