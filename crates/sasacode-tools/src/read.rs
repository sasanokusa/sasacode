//! The read tool (`packages/tools/src/read.ts`).
//!
//! Limits the model sees count JS string length, i.e. UTF-16 units: `MAX_LINE`, the `N chars` of a
//! truncated line, and `MAX_TOTAL` over the formatted lines.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use sasacode_ai::types::ImageContent;
use sasacode_plugin_api::{JsonObject, Tool, ToolContext, ToolKind, ToolResult, UserContent};

use crate::util::{
    fs_error, js_number_text, js_string_arg, num_arg, resolve_path, split_lines, str_arg, truthy_arg, utf16_len,
    utf16_prefix,
};

const DEFAULT_LIMIT: i64 = 2000;
const MAX_LINE: usize = 2000;
/// Characters one call returns at most: 2,000 lines of 200 would fill a small model's whole window (#66).
const MAX_TOTAL: usize = 100_000;
/// Larger files are read line by line up to what is shown, not loaded whole.
const WHOLE_FILE_BYTES: u64 = 10_000_000;
/// The providers' limit for one image (Anthropic's is 5 MB); a larger one would fail every request after it.
const MAX_IMAGE_BYTES: u64 = 5_000_000;
const HEAD_BYTES: usize = 8192;

fn image_type(path: &str) -> Option<&'static str> {
    // Node's extname: the last dot of the base name, not a leading one.
    let ext = Path::new(path).extension()?.to_str()?.to_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    })
}

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> String {
        format!(
            "Read a file. Text is returned with line numbers (cat -n style), up to {DEFAULT_LIMIT} lines or {MAX_TOTAL} characters per call; use offset/limit for more. Images (png, jpg, gif, webp) are returned as images."
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path, absolute or relative to the working directory" },
                "offset": { "type": "integer", "minimum": 1, "description": "1-based line to start from" },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines" }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn always_load(&self) -> bool {
        true
    }
    fn paths(&self, args: &JsonObject, cwd: &Path) -> Option<Vec<String>> {
        Some(vec![resolve_path(str_arg(args, "path"), cwd)])
    }
    fn summary(&self, args: &JsonObject) -> Option<String> {
        let offset = if truthy_arg(args, "offset") {
            format!(":{}", js_string_arg(args, "offset"))
        } else {
            String::new()
        };
        Some(js_string_arg(args, "path") + &offset)
    }
    async fn execute(&self, args: JsonObject, ctx: ToolContext) -> anyhow::Result<ToolResult> {
        let path = resolve_path(str_arg(&args, "path"), &ctx.cwd);
        let Ok(st) = tokio::fs::metadata(&path).await else {
            return Ok(ToolResult::error(format!("File not found: {path}")));
        };
        if st.is_dir() {
            return Ok(ToolResult::error(format!(
                "{path} is a directory. List it with bash (ls)."
            )));
        }
        // A FIFO or a device would block until someone writes to it, or never end.
        if !st.is_file() {
            return Ok(ToolResult::error(format!(
                "{path} is not a regular file (a pipe, socket or device); use bash if you really need it."
            )));
        }
        let size = st.len();
        if let Some(media_type) = image_type(&path) {
            if size > MAX_IMAGE_BYTES {
                return Ok(ToolResult::error(format!(
                    "{path} is {size} bytes, over the {MAX_IMAGE_BYTES} bytes an image may have. Make a smaller copy with bash (e.g. sips -Z 1600 or magick -resize) and read that."
                )));
            }
            let data = tokio::fs::read(&path).await.map_err(|e| fs_error(&e, "open", &path))?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
            return Ok(ToolResult {
                content: vec![
                    UserContent::text(format!("Image {path} ({} bytes)", data.len())),
                    UserContent::Image(ImageContent {
                        media_type: media_type.into(),
                        data: Arc::from(encoded),
                    }),
                ],
                is_error: None,
                details: None,
            });
        }
        let whole = if size > WHOLE_FILE_BYTES {
            None
        } else {
            Some(tokio::fs::read(&path).await.map_err(|e| fs_error(&e, "open", &path))?)
        };
        let head = match &whole {
            Some(w) => w[..w.len().min(HEAD_BYTES)].to_vec(),
            None => read_head(&path).await.map_err(|e| fs_error(&e, "open", &path))?,
        };
        if head.contains(&0) {
            return Ok(ToolResult::error(format!(
                "{path} looks like a binary file ({size} bytes)."
            )));
        }
        let offset = num_arg(&args, "offset");
        let start = (offset.unwrap_or(1.0).trunc() as i64 - 1).max(0) as usize;
        let wanted = start
            + num_arg(&args, "limit")
                .map_or(DEFAULT_LIMIT, |l| l.trunc() as i64)
                .max(0) as usize;
        let decoded;
        let streamed;
        let (lines, more): (Vec<&str>, bool) = match &whole {
            Some(w) => {
                decoded = String::from_utf8_lossy(w);
                (split_lines(&decoded), false)
            }
            None => {
                let (got, more) = read_lines(&path, wanted, &ctx.signal).await?;
                streamed = got;
                (streamed.iter().map(String::as_str).collect(), more)
            }
        };
        Ok(render(&path, size, offset, start, wanted, &lines, more, &whole, &head))
    }
}

/// The part of `execute` after the lines are known.
#[allow(clippy::too_many_arguments)]
fn render(
    path: &str,
    size: u64,
    offset: Option<f64>,
    start: usize,
    wanted: usize,
    lines: &[&str],
    more: bool,
    whole: &Option<Vec<u8>>,
    head: &[u8],
) -> ToolResult {
    if start >= lines.len() && !lines.is_empty() {
        let offset = offset.map_or_else(|| "undefined".into(), js_number_text);
        return ToolResult::error(format!(
            "offset {offset} is past the end of the file ({} lines).",
            lines.len()
        ));
    }
    let mut end = lines.len().min(wanted);
    // Size only grows, so this keeps the whole lines that fit in MAX_TOTAL and drops every one after (#66).
    let mut total = 0;
    let mut shown = Vec::new();
    for (i, l) in lines[start.min(end)..end].iter().enumerate() {
        let n = start + i + 1;
        let units = utf16_len(l);
        let line = if units <= MAX_LINE {
            format!("{n:>6}\t{l}")
        } else {
            // Not after the first half of a surrogate pair: a lone one fails every later request (#88).
            format!("{n:>6}\t{}… [line truncated, {units} chars]", utf16_prefix(l, MAX_LINE))
        };
        total += utf16_len(&line) + 1;
        if total > MAX_TOTAL {
            break;
        }
        shown.push(line);
    }
    end = start + shown.len();
    let cut = lines[start.min(end)..end]
        .iter()
        .filter(|l| utf16_len(l) > MAX_LINE)
        .count();
    let mut notes = Vec::new();
    if more {
        let mb = (size as f64 / 1_000_000.0).round();
        notes.push(format!(
            "[Showing lines {}-{end} of a {} MB file. Use offset={} to continue.]",
            start + 1,
            js_number_text(mb),
            end + 1
        ));
    } else if end < lines.len() || start > 0 {
        let go_on = if end < lines.len() {
            format!(" Use offset={} to continue.", end + 1)
        } else {
            String::new()
        };
        notes.push(format!(
            "[Showing lines {}-{end} of {}.{go_on}]",
            start + 1,
            lines.len()
        ));
    }
    let checked = match whole {
        Some(w) => &w[..],
        // A large file by its whole first lines (#137).
        None => &head[..head.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1)],
    };
    if std::str::from_utf8(checked).is_err() {
        // Shown with U+FFFD: edit refuses it, write would keep them (#89).
        notes.push(format!(
            "[{path} is not valid UTF-8 (e.g. Shift_JIS, EUC-JP or Latin-1): undecodable bytes are shown as U+FFFD. edit refuses it, and writing back what is shown would lose them; use bash (e.g. iconv) to change it.]"
        ));
    }
    if cut > 0 {
        notes.push(format!(
            "[{cut} long line(s) truncated at {MAX_LINE} chars; use bash (e.g. cut, sed) to see them in full.]"
        ));
    }
    if lines.is_empty() || (lines.len() == 1 && lines[0].is_empty()) {
        return ToolResult::text(format!("{path} is empty."));
    }
    let mut parts = vec![shown.join("\n")];
    parts.extend(notes);
    ToolResult::text(parts.join("\n"))
}

/// The first bytes of a file, for the binary check.
async fn read_head(path: &str) -> std::io::Result<Vec<u8>> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut head = vec![0u8; HEAD_BYTES];
    let mut n = 0;
    while n < HEAD_BYTES {
        let got = f.read(&mut head[n..]).await?;
        if got == 0 {
            break;
        }
        n += got;
    }
    head.truncate(n);
    Ok(head)
}

/// The first `count` lines of a large file; `more` when the file goes on after them. Lines end as
/// Node's readline ends them: `\r\n`, `\n`, a lone `\r`, U+2028 or U+2029; text after the last
/// break is a line, an empty one is not.
async fn read_lines(path: &str, count: usize, signal: &CancellationToken) -> anyhow::Result<(Vec<String>, bool)> {
    let mut f = tokio::fs::File::open(path)
        .await
        .map_err(|e| fs_error(&e, "open", path))?;
    let mut decoder = encoding_rs::UTF_8.new_decoder_without_bom_handling();
    let mut buf = vec![0u8; 64 * 1024];
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut after_cr = false;
    loop {
        if signal.is_cancelled() {
            anyhow::bail!("The operation was aborted");
        }
        let n = f.read(&mut buf).await.map_err(|e| fs_error(&e, "read", path))?;
        let last = n == 0;
        let mut chunk = String::with_capacity(decoder.max_utf8_buffer_length(n).unwrap_or(n * 3 + 16));
        let _ = decoder.decode_to_string(&buf[..n], &mut chunk, last);
        for c in chunk.chars() {
            if std::mem::take(&mut after_cr) && c == '\n' {
                continue; // the \n of a \r\n already ended the line
            }
            match c {
                '\r' | '\n' | '\u{2028}' | '\u{2029}' => {
                    if lines.len() >= count {
                        return Ok((lines, true));
                    }
                    lines.push(std::mem::take(&mut line));
                    after_cr = c == '\r';
                }
                _ => line.push(c),
            }
        }
        if last {
            break;
        }
    }
    if !line.is_empty() {
        if lines.len() >= count {
            return Ok((lines, true));
        }
        lines.push(line);
    }
    Ok((lines, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_types_by_extension() {
        assert_eq!(image_type("/a/b.PNG"), Some("image/png"));
        assert_eq!(image_type("x.jpeg"), Some("image/jpeg"));
        assert_eq!(image_type(".png"), None);
        assert_eq!(image_type("a.txt"), None);
    }
}
