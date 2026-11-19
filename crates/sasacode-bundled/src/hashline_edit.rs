//! hashline-edit (opt-in via `plugins.enabled`): adds short line-number+hash anchors (`42:a3|...`)
//! to `read` results and provides `hash_edit` so small models can edit lines reliably without
//! copying exact whitespace indentation. Also strips accidental `N:hh|` prefixes sent to `write`.

use std::sync::OnceLock;

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::{Value, json};

use sasacode_ai::JsonObject;
use sasacode_plugin_api::{
    PluginApi, SystemPromptResult, ToolCallRawResult, ToolContext, ToolDef, ToolKind, ToolResult, ToolResultResult,
    UserContent, hook,
};
use sasacode_tools::util::{line_diff, resolve_path, split_lines};

const MAX_DISK_HASH_BYTES: u64 = 2_000_000;

/// Compute a 2-hex-digit hash for a single source line (without trailing `\r` or `\n`).
pub fn line_hash(line: &str) -> String {
    let clean = line.strip_suffix('\r').unwrap_or(line);
    let mut h: u32 = 0x811c_9dc5;
    for &b in clean.as_bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    let folded = (h ^ (h >> 8) ^ (h >> 16) ^ (h >> 24)) & 0xff;
    format!("{folded:02x}")
}

fn read_line_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^ *(\d+)\t(.*)$").expect("valid regex"))
}

fn hashline_prefix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s*(\d+):([0-9a-fA-F]{2})\|(.*)$").expect("valid regex"))
}

fn transform_read_text(text: &str, disk_lines: Option<&[&str]>) -> Option<String> {
    let re = read_line_re();
    let mut matched = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if let Some(cap) = re.captures(line)
            && let Ok(n) = cap[1].parse::<usize>()
            && n >= 1
        {
            matched = true;
            let body = &cap[2];
            let raw = disk_lines.and_then(|d| d.get(n - 1).copied()).unwrap_or(body);
            let h = line_hash(raw);
            out.push(format!("{n}:{h}|{body}"));
        } else {
            out.push(line.to_string());
        }
    }
    matched.then(|| out.join("\n"))
}

/// Strip sequential `N:hh|` prefixes if a model copies hashline-annotated `read` output into `write`.
pub fn strip_hashline_prefixes(content: &str) -> Option<String> {
    let trimmed = content.strip_suffix('\n').unwrap_or(content);
    let rows: Vec<&str> = trimmed.split('\n').collect();
    if rows.is_empty() {
        return None;
    }
    let re = hashline_prefix_re();
    let mut expected: Option<usize> = None;
    let mut body: Vec<&str> = Vec::with_capacity(rows.len());
    for row in rows {
        let cap = re.captures(row)?;
        let n: usize = cap[1].parse().ok()?;
        match expected {
            None => expected = Some(n),
            Some(prev) if n == prev + 1 => expected = Some(n),
            _ => return None,
        }
        body.push(cap.get(3).map_or("", |m| m.as_str()));
    }
    Some(body.join("\n") + if content.ends_with('\n') { "\n" } else { "" })
}

fn parse_anchor(raw: &str) -> Result<(usize, String), String> {
    let head = raw.trim().split('|').next().unwrap_or("").trim();
    let Some((num_part, hash_part)) = head.split_once(':') else {
        return Err(format!(
            "Invalid anchor \"{raw}\": expected '<line>:<2-hex-hash>' (e.g. '42:a3')."
        ));
    };
    let line: usize = num_part
        .trim()
        .parse()
        .ok()
        .filter(|&n| n >= 1)
        .ok_or_else(|| format!("Invalid line number in anchor \"{raw}\" (must be >= 1)."))?;
    let hash = hash_part.trim().to_ascii_lowercase();
    if hash.len() != 2 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "Invalid hash in anchor \"{raw}\": expected 2 hex digits (e.g. '42:a3')."
        ));
    }
    Ok((line, hash))
}

fn context_window_lines(lines: &[String], center: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let idx = center.saturating_sub(1).min(lines.len() - 1);
    let from = idx.saturating_sub(3);
    let to = (idx + 3).min(lines.len() - 1);
    (from..=to)
        .map(|i| format!("{}:{}|{}", i + 1, line_hash(&lines[i]), lines[i]))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn execute_hash_edit(args: JsonObject, ctx: ToolContext) -> anyhow::Result<ToolResult> {
    let raw_path = args.get("path").and_then(Value::as_str).unwrap_or("");
    let path = resolve_path(raw_path, &ctx.cwd);
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return Ok(ToolResult::error(format!(
            "File not found: {path}. Use write to create it."
        )));
    };
    let Ok(before) = String::from_utf8(bytes) else {
        return Ok(ToolResult::error(format!(
            "{path} is not valid UTF-8; edit it with bash instead."
        )));
    };
    if before.is_empty() {
        return Ok(ToolResult::error(format!("{path} is empty. Use write to populate it.")));
    }
    let start_raw = args.get("start").and_then(Value::as_str).unwrap_or("");
    let (start_line, start_hash) = match parse_anchor(start_raw) {
        Ok(v) => v,
        Err(msg) => return Ok(ToolResult::error(msg)),
    };
    let (end_line, end_hash) = match args.get("end").and_then(Value::as_str) {
        Some(end_raw) if !end_raw.trim().is_empty() => match parse_anchor(end_raw) {
            Ok(v) => v,
            Err(msg) => return Ok(ToolResult::error(msg)),
        },
        _ => (start_line, start_hash.clone()),
    };
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("replace");
    let content = args.get("content").and_then(Value::as_str).unwrap_or("");

    let has_crlf = before.contains("\r\n");
    let trailing_nl = before.ends_with('\n');
    let lines: Vec<String> = split_lines(&before)
        .into_iter()
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect();
    if start_line > lines.len() || end_line > lines.len() {
        return Ok(ToolResult::error(format!(
            "Line out of range in {path} ({} lines total; got {start_line}..={end_line}). Near end:\n{}",
            lines.len(),
            context_window_lines(&lines, lines.len())
        )));
    }
    if end_line < start_line {
        return Ok(ToolResult::error(format!(
            "end line ({end_line}) must be >= start line ({start_line})."
        )));
    }
    let actual_start = line_hash(&lines[start_line - 1]);
    if actual_start != start_hash {
        return Ok(ToolResult::error(format!(
            "Hash mismatch at {path} line {start_line}: expected {start_hash}, found {actual_start}. Current lines:\n{}",
            context_window_lines(&lines, start_line)
        )));
    }
    let actual_end = line_hash(&lines[end_line - 1]);
    if actual_end != end_hash {
        return Ok(ToolResult::error(format!(
            "Hash mismatch at {path} line {end_line}: expected {end_hash}, found {actual_end}. Current lines:\n{}",
            context_window_lines(&lines, end_line)
        )));
    }
    if mode != "replace" && content.is_empty() {
        return Ok(ToolResult::error(format!(
            "content must not be empty for mode \"{mode}\"."
        )));
    }

    let stripped = content.strip_suffix('\n').unwrap_or(content);
    let insert_lines: Vec<String> = if content.is_empty() {
        Vec::new()
    } else {
        stripped
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect()
    };

    let mut next_lines = Vec::with_capacity(lines.len() + insert_lines.len());
    match mode {
        "replace" => {
            next_lines.extend_from_slice(&lines[..start_line - 1]);
            next_lines.extend(insert_lines);
            next_lines.extend_from_slice(&lines[end_line..]);
        }
        "insert_after" => {
            next_lines.extend_from_slice(&lines[..end_line]);
            next_lines.extend(insert_lines);
            next_lines.extend_from_slice(&lines[end_line..]);
        }
        "insert_before" => {
            next_lines.extend_from_slice(&lines[..start_line - 1]);
            next_lines.extend(insert_lines);
            next_lines.extend_from_slice(&lines[start_line - 1..]);
        }
        other => return Ok(ToolResult::error(format!("Unknown mode \"{other}\""))),
    }

    let sep = if has_crlf { "\r\n" } else { "\n" };
    let mut after = next_lines.join(sep);
    if trailing_nl && !next_lines.is_empty() {
        after.push_str(sep);
    }
    if after == before {
        return Ok(ToolResult::error(
            "No changes made (replacement matches existing lines).",
        ));
    }
    if ctx.signal.is_cancelled() {
        return Ok(ToolResult::error("Interrupted by the user; the file was not changed."));
    }
    tokio::fs::write(&path, &after)
        .await
        .map_err(|e| sasacode_tools::util::fs_error(&e, "open", &path))?;

    let diff = line_diff(&before, &after);
    let mut details = JsonObject::new();
    details.insert("path".into(), json!(path));
    details.insert("changed".into(), json!(true));
    details.insert("diff".into(), json!(diff));
    Ok(ToolResult::text(format!("Edited {path} (lines {start_line}-{end_line}, {mode}).")).with_details(details))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let cwd = api.cwd();

        api.on::<hook::SystemPrompt, _, _>(|ev, _ctx| async move {
            let prompt = format!(
                "{}\n\nNote: `read` prefixes each line with `<line>:<hash>|`. You can use `hash_edit` with `start`/`end` anchors (e.g. `42:a3`) instead of copying `old_string` verbatim.",
                ev.prompt
            );
            Ok(Some(SystemPromptResult { prompt: Some(prompt) }))
        });

        api.on::<hook::ToolCallRaw, _, _>(|ev, _ctx| async move {
            if ev.name != "write" {
                return Ok(None);
            }
            let Some(Value::String(content)) = ev.input.get("content") else {
                return Ok(None);
            };
            let Some(plain) = strip_hashline_prefixes(content) else {
                return Ok(None);
            };
            let mut input = ev.input.clone();
            input.insert("content".into(), Value::String(plain));
            Ok(Some(ToolCallRawResult {
                name: None,
                input: Some(input),
                note: Some("removed hashline prefixes (N:hh|) from content; write takes the file's plain text".into()),
            }))
        });

        let cwd_res = cwd.clone();
        api.on::<hook::ToolResult, _, _>(move |ev, _ctx| {
            let cwd = cwd_res.clone();
            async move {
                if !ev.ran || ev.result.is_error() || ev.call.name != "read" {
                    return Ok(None);
                }
                let disk_text: Option<String> = match ev.call.input.get("path").and_then(Value::as_str) {
                    Some(raw) => {
                        let resolved = resolve_path(raw, &cwd);
                        let meta = tokio::fs::metadata(&resolved).await.ok();
                        if meta.is_some_and(|m| m.is_file() && m.len() <= MAX_DISK_HASH_BYTES) {
                            tokio::fs::read(&resolved)
                                .await
                                .ok()
                                .map(|b| String::from_utf8_lossy(&b).into_owned())
                        } else {
                            None
                        }
                    }
                    None => None,
                };
                let disk_lines: Option<Vec<&str>> = disk_text.as_deref().map(split_lines);

                let mut changed = false;
                let mut next_content = Vec::with_capacity(ev.result.content.len());
                for item in &ev.result.content {
                    if let UserContent::Text(tc) = item
                        && let Some(updated) = transform_read_text(&tc.text, disk_lines.as_deref())
                    {
                        changed = true;
                        next_content.push(UserContent::text(updated));
                    } else {
                        next_content.push(item.clone());
                    }
                }
                if !changed {
                    return Ok(None);
                }
                let mut result = ev.result;
                result.content = next_content;
                Ok(Some(ToolResultResult { result: Some(result) }))
            }
        });

        api.register_tool(
            ToolDef::new(
                "hash_edit",
                "Edit a file by line:hash anchors shown by read (e.g. start='42:a3', end='45:f1'). Avoids whitespace mismatch errors.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "File path, absolute or relative to working directory" },
                        "start": { "type": "string", "description": "Start line anchor '<line>:<hash>' (e.g. '42:a3')" },
                        "end": { "type": "string", "description": "Optional inclusive end anchor '<line>:<hash>' (defaults to start)" },
                        "content": { "type": "string", "description": "Replacement text (or empty string to delete)" },
                        "mode": { "type": "string", "enum": ["replace", "insert_after", "insert_before"], "description": "Edit mode (default replace)" }
                    },
                    "required": ["path", "start", "content"],
                    "additionalProperties": false
                }),
                ToolKind::Edit,
                execute_hash_edit,
            )
            .always_load(true)
            .paths(|args, cwd| vec![resolve_path(args.get("path").and_then(Value::as_str).unwrap_or(""), cwd)])
            .summary(|args| {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                let start = args.get("start").and_then(Value::as_str).unwrap_or("");
                format!("{path}@{start}")
            })
            .into_ref(),
        );

        Ok(())
    })
}
