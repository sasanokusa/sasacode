//! The pictures a tool's result holds, as base64 (`read` of an image, an MCP tool's): kept as
//! files in the system's temporary folder to be drawn from, each named by what it holds, so that
//! one read again is not written twice.

use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// The file holding the picture `data` (base64) of `media_type`; none for a kind the window does
/// not draw, or one that cannot be decoded or written.
pub fn kept(media_type: &str, data: &str) -> Option<PathBuf> {
    let ext = match media_type {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => return None,
    };
    let bytes = decode(data).filter(|b| !b.is_empty())?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let dir = std::env::temp_dir().join("sasacode-gui-pictures");
    let file = dir.join(format!("{:016x}.{ext}", hasher.finish()));
    if !file.is_file() {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(&file, &bytes).ok()?;
    }
    Some(file)
}

/// Base64, standard or URL-safe, with or without its padding; whitespace is passed over.
fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut held, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        held = (held << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((held >> bits) as u8);
            held &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_read_with_or_without_padding_and_a_picture_is_kept_once() {
        assert_eq!(decode("aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(decode("aGVsbG8").as_deref(), Some(&b"hello"[..]));
        assert_eq!(decode("aGVs\nbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(decode("+/8=").as_deref(), Some(&[0xfb, 0xff][..]));
        assert!(decode("not*base64").is_none());
        // A PNG's first bytes: kept as a .png, the same file the second time.
        let png = "iVBORw0KGgo=";
        let file = kept("image/png", png).unwrap();
        assert_eq!(file.extension().and_then(|e| e.to_str()), Some("png"));
        assert_eq!(std::fs::read(&file).unwrap(), b"\x89PNG\r\n\x1a\n");
        assert_eq!(kept("image/png", png), Some(file));
        // Not a kind drawn, or nothing in it: none.
        assert!(kept("application/pdf", png).is_none());
        assert!(kept("image/png", "").is_none());
    }
}
