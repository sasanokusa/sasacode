//! Server-Sent Events parsing for the Streamable HTTP transport, as `eventsource-parser` (used by
//! the TS SDK) reads them. Private to this crate until `sasacode_ai::sse` is ported; then this can
//! switch to it.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Event {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: String,
}

/// Feed bytes in; complete events come out. Lines end in CRLF, LF or CR; a BOM at the start is skipped.
#[derive(Default)]
pub(crate) struct Parser {
    buf: Vec<u8>,
    started: bool,
    /// The previous chunk ended in CR: a LF at the start of the next one belongs to it.
    after_cr: bool,
    id: Option<String>,
    data: String,
    event: String,
    /// The last `retry:` value (ms).
    pub retry: Option<u64>,
}

impl Parser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Event> {
        let mut out = Vec::new();
        let mut chunk = chunk;
        if self.after_cr {
            if let Some(rest) = chunk.strip_prefix(b"\n") {
                chunk = rest;
            }
            self.after_cr = false;
        }
        self.buf.extend_from_slice(chunk);
        if !self.started {
            const BOM: &[u8] = b"\xEF\xBB\xBF";
            if self.buf.starts_with(BOM) {
                self.buf.drain(..3);
                self.started = true;
            } else if BOM.starts_with(&self.buf) {
                return out; // could still be a BOM: wait for more
            } else {
                self.started = true;
            }
        }
        let mut start = 0;
        let mut i = 0;
        while i < self.buf.len() {
            let b = self.buf[i];
            if b == b'\n' || b == b'\r' {
                let line = String::from_utf8_lossy(&self.buf[start..i]).into_owned();
                let mut next = i + 1;
                if b == b'\r' {
                    if next < self.buf.len() {
                        if self.buf[next] == b'\n' {
                            next += 1;
                        }
                    } else {
                        self.after_cr = true;
                    }
                }
                self.line(&line, &mut out);
                start = next;
                i = next;
            } else {
                i += 1;
            }
        }
        self.buf.drain(..start);
        out
    }

    fn line(&mut self, line: &str, out: &mut Vec<Event>) {
        if line.is_empty() {
            // Dispatch: an event needs data; the buffers reset either way.
            if !self.data.is_empty() {
                let data = self.data.strip_suffix('\n').unwrap_or(&self.data).to_string();
                let event = if self.event.is_empty() {
                    None
                } else {
                    Some(self.event.clone())
                };
                out.push(Event {
                    id: self.id.clone(),
                    event,
                    data,
                });
            }
            self.id = None;
            self.data.clear();
            self.event.clear();
            return;
        }
        if line.starts_with(':') {
            return; // comment
        }
        let (field, value) = match line.find(':') {
            Some(at) => {
                let v = &line[at + 1..];
                (&line[..at], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line, ""),
        };
        match field {
            "event" => self.event = value.to_string(),
            "data" => {
                self.data.push_str(value);
                self.data.push('\n');
            }
            "id" => {
                self.id = if value.contains('\0') {
                    None
                } else {
                    Some(value.to_string())
                }
            }
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                self.retry = value.parse().ok();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_across_chunks_and_line_endings() {
        let mut p = Parser::default();
        let mut got = p.feed(b"\xEF\xBB\xBFid: 1\r\nevent: message\r\ndata: {\"a\":");
        got.extend(p.feed(b"1}\r"));
        got.extend(p.feed(b"\n\r\n: keep-alive\n\ndata: x\ndata: y\n\nretry: 500\nid: 2\ndata:\n\n"));
        assert_eq!(
            got,
            vec![
                Event {
                    id: Some("1".into()),
                    event: Some("message".into()),
                    data: "{\"a\":1}".into()
                },
                Event {
                    id: None,
                    event: None,
                    data: "x\ny".into()
                },
                Event {
                    id: Some("2".into()),
                    event: None,
                    data: String::new()
                },
            ]
        );
        assert_eq!(p.retry, Some(500));
    }
}
