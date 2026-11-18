//! Server-Sent Events, decoded the way the SDKs' `LineDecoder` + `SSEDecoder` did
//! (`@anthropic-ai/sdk` and `openai`, `core/streaming.ts`; rust/research/llm-sdks.md section 4).
//!
//! - Lines end at `\r\n`, `\r` or `\n`; each line is decoded on its own, lossily (bad UTF-8 → U+FFFD),
//!   so a character split across network chunks is put back together.
//! - An empty line dispatches the event if an `event` name or any `data` line was seen; `:` lines are
//!   comments; a field is split at the first `:` and one leading space of the value is dropped; `data`
//!   lines are joined with `\n`; `id`, `retry` and unknown fields are ignored.
//! - At the end of the body, the openai SDK dispatches an event that lacked its closing blank line;
//!   the anthropic one drops it ([`SseDecoder::finish`]).

use serde_json::Value;

/// One dispatched event: its `event:` name (`None` when there was none) and its `data` lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental decoder: feed it the body's bytes as they arrive.
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// Bytes of the line being read.
    line: Vec<u8>,
    /// The last byte was `\r`: a `\n` right after it ends nothing more.
    after_cr: bool,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decode a chunk of the body; returns the events it completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        for &b in bytes {
            match b {
                b'\n' if self.after_cr => self.after_cr = false,
                b'\n' | b'\r' => {
                    self.after_cr = b == b'\r';
                    let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                    if let Some(ev) = self.decode_line(&line) {
                        out.push(ev);
                    }
                }
                _ => {
                    self.after_cr = false;
                    self.line.push(b);
                }
            }
        }
        out
    }

    /// The body ended: decode a last line without its line break, then, when `flush_pending`
    /// (openai), dispatch an event that was never closed by a blank line.
    pub fn finish(&mut self, flush_pending: bool) -> Vec<SseEvent> {
        let mut out = Vec::new();
        if !self.line.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
            if let Some(ev) = self.decode_line(&line) {
                out.push(ev);
            }
        }
        if flush_pending && let Some(ev) = self.decode_line("") {
            out.push(ev);
        }
        out
    }

    fn decode_line(&mut self, line: &str) -> Option<SseEvent> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            // `!this.event`: an empty name counts as none.
            if self.event.as_deref().is_none_or(str::is_empty) && self.data.is_empty() {
                return None;
            }
            let ev = SseEvent {
                event: self.event.take(),
                data: std::mem::take(&mut self.data).join("\n"),
            };
            return Some(ev);
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.find(':') {
            Some(i) => (&line[..i], &line[i + 1..]),
            None => (line, ""),
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        None
    }
}

/// `JSON.parse` for event data. JS accepts escaped lone surrogates (`"\ud83d"`), serde_json does
/// not, and a Rust string cannot hold one: such an escape becomes U+FFFD instead of ending the stream.
pub fn parse_json(text: &str) -> serde_json::Result<Value> {
    match serde_json::from_str(text) {
        Ok(v) => Ok(v),
        Err(e) if text.contains("\\u") => {
            let fixed = replace_lone_surrogates(text);
            if fixed == text {
                Err(e)
            } else {
                serde_json::from_str(&fixed)
            }
        }
        Err(e) => Err(e),
    }
}

/// Rewrite `\uD800`–`\uDFFF` escapes that are not part of a surrogate pair as `�`.
fn replace_lone_surrogates(text: &str) -> String {
    let hex4 = |s: &str| -> Option<u32> {
        let h = s.get(..4)?;
        if h.bytes().all(|b| b.is_ascii_hexdigit()) {
            u32::from_str_radix(h, 16).ok()
        } else {
            None
        }
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // A backslash always starts a two-character escape inside a JSON string.
        let Some(next) = rest[1..].chars().next() else {
            out.push_str(rest);
            return out;
        };
        if next != 'u' {
            out.push('\\');
            out.push(next);
            rest = &rest[1 + next.len_utf8()..];
            continue;
        }
        let Some(cp) = hex4(&rest[2..]) else {
            out.push_str("\\u");
            rest = &rest[2..];
            continue;
        };
        if (0xD800..0xDC00).contains(&cp) {
            let low = rest.get(6..8).filter(|s| *s == "\\u").and_then(|_| hex4(&rest[8..]));
            if low.is_some_and(|l| (0xDC00..0xE000).contains(&l)) {
                out.push_str(&rest[..12]);
                rest = &rest[12..];
            } else {
                out.push_str("\\ufffd");
                rest = &rest[6..];
            }
        } else if (0xDC00..0xE000).contains(&cp) {
            out.push_str("\\ufffd");
            rest = &rest[6..];
        } else {
            out.push_str(&rest[..6]);
            rest = &rest[6..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: Option<&str>, data: &str) -> SseEvent {
        SseEvent {
            event: event.map(str::to_string),
            data: data.to_string(),
        }
    }

    fn decode_bytewise(input: &[u8], flush: bool) -> Vec<SseEvent> {
        let mut d = SseDecoder::new();
        let mut out = Vec::new();
        for b in input {
            out.extend(d.feed(std::slice::from_ref(b)));
        }
        out.extend(d.finish(flush));
        out
    }

    #[test]
    fn events_lines_and_fields() {
        let input = b"event: a\ndata: 1\ndata:2\n\n: comment\nid: 7\nretry: 5\ndata: x\n\nevent: only\n\n";
        let want = vec![ev(Some("a"), "1\n2"), ev(None, "x"), ev(Some("only"), "")];
        assert_eq!(decode_bytewise(input, false), want);
        let mut d = SseDecoder::new();
        let mut whole = d.feed(input);
        whole.extend(d.finish(false));
        assert_eq!(whole, want);
    }

    #[test]
    fn crlf_and_cr_line_endings() {
        assert_eq!(
            decode_bytewise(b"data: a\r\n\r\ndata: b\r\rdata: c\n\n", false),
            vec![ev(None, "a"), ev(None, "b"), ev(None, "c")]
        );
        // A field without a colon is a name with an empty value; only one leading space is dropped.
        assert_eq!(
            decode_bytewise(b"data\ndata:  two\n\n", false),
            vec![ev(None, "\n two")]
        );
    }

    #[test]
    fn a_character_split_across_chunks_is_kept() {
        let input = "data: 日本語🙂\n\n".as_bytes();
        assert_eq!(decode_bytewise(input, false), vec![ev(None, "日本語🙂")]);
        // Bad bytes become U+FFFD rather than failing.
        assert_eq!(
            decode_bytewise(b"data: a\xffb\n\n", false),
            vec![ev(None, "a\u{fffd}b")]
        );
    }

    #[test]
    fn an_unterminated_last_event_is_kept_only_when_flushed() {
        assert_eq!(
            decode_bytewise(b"data: done\n\ndata: tail", false),
            vec![ev(None, "done")]
        );
        assert_eq!(
            decode_bytewise(b"data: done\n\ndata: tail", true),
            vec![ev(None, "done"), ev(None, "tail")]
        );
        assert_eq!(decode_bytewise(b"data: done\n\n", true), vec![ev(None, "done")]);
        // An empty event name with no data is nothing.
        assert_eq!(decode_bytewise(b"event:\n\n", true), vec![]);
    }

    #[test]
    fn lone_surrogate_escapes_parse() {
        assert_eq!(parse_json(r#"{"a":"x\ud83d"}"#).unwrap()["a"], "x\u{fffd}");
        assert_eq!(parse_json(r#"{"a":"\ude00y"}"#).unwrap()["a"], "\u{fffd}y");
        assert_eq!(parse_json(r#"{"a":"😀"}"#).unwrap()["a"], "😀");
        assert_eq!(parse_json(r#"{"a":"\\ud83d"}"#).unwrap()["a"], "\\ud83d");
        assert_eq!(parse_json(r#"{"a":"\ud83d\n"}"#).unwrap()["a"], "\u{fffd}\n");
        assert!(parse_json("{not json").is_err());
    }
}
