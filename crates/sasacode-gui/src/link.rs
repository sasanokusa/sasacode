//! A link to a folder's window, and to a conversation in it: `sasacode://open?folder=…&session=…`.
//! The window copies one for the conversation shown; given one where it takes a folder, it opens
//! there. Nothing here tells the system to send such links to the window: that is the installer's
//! to do, or the user's.

use std::path::{Path, PathBuf};

pub const SCHEME: &str = "sasacode://";

/// Where a link goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub folder: Option<PathBuf>,
    pub session: Option<String>,
}

/// The link to `folder`, and to the conversation `session` in it.
pub fn make(folder: &Path, session: Option<&str>) -> String {
    let mut url = format!("{SCHEME}open?folder={}", encode(&folder.to_string_lossy()));
    if let Some(id) = session {
        url.push_str(&format!("&session={}", encode(id)));
    }
    url
}

/// Where `url` goes, if it is a link of this window's: `open` with a folder, a session or both.
pub fn parse(url: &str) -> Option<Link> {
    let rest = url.strip_prefix(SCHEME)?;
    let (verb, query) = rest.split_once('?').unwrap_or((rest, ""));
    if verb.trim_end_matches('/') != "open" {
        return None;
    }
    let mut link = Link {
        folder: None,
        session: None,
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let value = decode(value)?;
        if value.is_empty() {
            continue;
        }
        match key {
            "folder" => link.folder = Some(PathBuf::from(value)),
            "session" => link.session = Some(value),
            _ => {}
        }
    }
    (link.folder.is_some() || link.session.is_some()).then_some(link)
}

/// `text` with all but the unreserved characters as `%XX`.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~/".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `text` with its `%XX` (and `+`, a space) read back; none when that is not UTF-8.
fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = text.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_made_for_a_conversation_is_read_back_whatever_its_folder_is_called() {
        let folder = Path::new("/Users/me/作業 中/orders-api");
        let url = make(folder, Some("0a1b2c3d-1"));
        assert!(url.starts_with("sasacode://open?folder=/Users/me/%E4%BD%9C"));
        assert!(!url.contains(' '));
        assert_eq!(
            parse(&url),
            Some(Link {
                folder: Some(folder.to_path_buf()),
                session: Some("0a1b2c3d-1".into()),
            })
        );
        assert_eq!(parse(&make(folder, None)).unwrap().session, None);
    }

    #[test]
    fn what_is_not_a_link_of_the_window_opens_nothing() {
        assert_eq!(parse("https://example.com/open?folder=/x"), None);
        assert_eq!(parse("sasacode://delete?folder=/x"), None);
        assert_eq!(parse("sasacode://open"), None);
        assert_eq!(parse("sasacode://open?folder=%ZZ"), None);
        assert_eq!(
            parse("sasacode://open/?session=abc&other=1")
                .unwrap()
                .session
                .as_deref(),
            Some("abc")
        );
    }
}
