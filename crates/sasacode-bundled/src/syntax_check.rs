//! syntax-check: fast syntax verification right after `edit`, `write`, or `hash_edit` succeeds,
//! appending an immediate `[syntax-check]` diagnostic so broken edits are fixed on the next turn.
//!
//! settings: { externalCheck?: boolean (default true), timeoutMs?: number (default 1500), maxBytes?: number (default 524288) }

use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;

use sasacode_ai::UserContent;
use sasacode_plugin_api::{PluginApi, ToolResultResult, apply_child_env, hook};
use sasacode_tools::util::resolve_path;

use crate::repair::js::num_setting;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxIssue {
    pub line: usize,
    pub message: String,
}

/// Exact Rust lexical & delimiter checker (`//`, nested `/* */`, strings, raw strings `r#"..."#`,
/// char literals vs lifetimes `'a` / `'static`, and `()[]{}` balance).
pub fn check_rust_syntax(src: &str) -> Option<SyntaxIssue> {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut stack: Vec<(u8, usize)> = Vec::new();

    while i < n {
        let ch = b[i];
        if ch == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        // Line comment
        if ch == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        // Nested block comment
        if ch == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let start_line = line;
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if b[i] == b'\n' {
                    line += 1;
                    i += 1;
                } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && i + 1 < n && b[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth > 0 {
                return Some(SyntaxIssue {
                    line: start_line,
                    message: "unclosed block comment `/*`".to_string(),
                });
            }
            continue;
        }
        // Raw strings: r"...", r#"..."#, br"...", br#"..."#, cr"...", cr#"..."#
        let prev_ident = i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_');
        if !prev_ident && let Some((prefix_end, hashes)) = raw_string_start(b, i) {
            let start_line = line;
            i = prefix_end;
            let mut closed = false;
            while i < n {
                if b[i] == b'\n' {
                    line += 1;
                    i += 1;
                } else if b[i] == b'"' && i + 1 + hashes <= n && b[i + 1..i + 1 + hashes].iter().all(|&c| c == b'#') {
                    i += 1 + hashes;
                    closed = true;
                    break;
                } else {
                    i += 1;
                }
            }
            if !closed {
                return Some(SyntaxIssue {
                    line: start_line,
                    message: "unclosed raw string literal".to_string(),
                });
            }
            continue;
        }
        // Normal / byte / c strings: "...", b"...", c"..."
        if ch == b'"' || (!prev_ident && (ch == b'b' || ch == b'c') && i + 1 < n && b[i + 1] == b'"') {
            let start_line = line;
            i += if ch == b'"' { 1 } else { 2 };
            let mut closed = false;
            while i < n {
                if b[i] == b'\n' {
                    line += 1;
                    i += 1;
                } else if b[i] == b'\\' {
                    i += 1;
                    if i < n {
                        if b[i] == b'\n' {
                            line += 1;
                        }
                        i += 1;
                    }
                } else if b[i] == b'"' {
                    i += 1;
                    closed = true;
                    break;
                } else {
                    i += 1;
                }
            }
            if !closed {
                return Some(SyntaxIssue {
                    line: start_line,
                    message: "unclosed string literal".to_string(),
                });
            }
            continue;
        }
        // Char / byte-char literal vs lifetime ('a, 'static, '_)
        if ch == b'\'' {
            if let Some(end) = char_literal_end(src, i) {
                i = end;
                continue;
            }
            // Lifetime or label: skip `'ident`
            i += 1;
            continue;
        }
        match ch {
            b'(' | b'[' | b'{' => stack.push((ch, line)),
            b')' | b']' | b'}' => {
                let expected = match ch {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                match stack.pop() {
                    Some((open, _)) if open == expected => {}
                    Some((open, open_line)) => {
                        return Some(SyntaxIssue {
                            line,
                            message: format!(
                                "mismatched delimiter `{}` (expected closing for `{}` opened on line {open_line})",
                                ch as char, open as char
                            ),
                        });
                    }
                    None => {
                        return Some(SyntaxIssue {
                            line,
                            message: format!("unexpected closing delimiter `{}`", ch as char),
                        });
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    if let Some((open, open_line)) = stack.last() {
        return Some(SyntaxIssue {
            line: *open_line,
            message: format!("unclosed delimiter `{}` opened on line {open_line}", *open as char),
        });
    }
    None
}

fn raw_string_start(b: &[u8], i: usize) -> Option<(usize, usize)> {
    let mut k = i;
    if k + 1 < b.len() && (b[k] == b'b' || b[k] == b'c') && b[k + 1] == b'r' {
        k += 2;
    } else if b[k] == b'r' {
        k += 1;
    } else {
        return None;
    }
    let mut hashes = 0usize;
    while k < b.len() && b[k] == b'#' {
        hashes += 1;
        k += 1;
    }
    (k < b.len() && b[k] == b'"').then_some((k + 1, hashes))
}

/// Returns the byte offset just after the closing `'` if `src[i..]` starts a char literal,
/// or `None` if it is a lifetime (`'a`, `'static`, `'_`).
fn char_literal_end(src: &str, i: usize) -> Option<usize> {
    let rest = &src[i + 1..];
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if first == '\\' {
        let (esc_idx, esc) = chars.next()?;
        let after_esc = i + 1 + esc_idx + esc.len_utf8();
        let tail = &src[after_esc..];
        let close = match esc {
            'x' => tail.get(2..)?.starts_with('\'').then_some(after_esc + 3),
            'u' => {
                let end_brace = tail.strip_prefix('{')?.find('}')?;
                let after = after_esc + 1 + end_brace + 1;
                src.get(after..)?.starts_with('\'').then_some(after + 1)
            }
            _ => tail.starts_with('\'').then_some(after_esc + 1),
        };
        return close;
    }
    if first == '\'' || first == '\n' || first == '\r' {
        return None;
    }
    let (_, second) = chars.next()?;
    (second == '\'').then_some(i + 1 + first.len_utf8() + 1)
}

async fn run_external(cmd_name: &str, args: &[&str], timeout_ms: u64) -> Option<String> {
    let mut cmd = tokio::process::Command::new(cmd_name);
    cmd.args(args);
    apply_child_env(&mut cmd, &[]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_millis(timeout_ms), cmd.output())
        .await
        .ok()?
        .ok()?;
    if out.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let combined = if !stderr.is_empty() { stderr } else { stdout };
    (!combined.is_empty()).then_some(combined)
}

fn parse_line_number(msg: &str) -> usize {
    static SPECIFIC: OnceLock<Regex> = OnceLock::new();
    static FALLBACK: OnceLock<Regex> = OnceLock::new();
    let specific = SPECIFIC.get_or_init(|| {
        Regex::new(r#"(?m)(?:^\s*at\s+[^\n]*?:|File\s+"[^"]+",\s*line\s+|^\S[^\s:]*\.[A-Za-z0-9]+:)(\d+)"#)
            .expect("valid regex")
    });
    if let Some(n) = specific
        .captures(msg)
        .and_then(|c| c[1].parse::<usize>().ok())
        .filter(|&l| l > 0)
    {
        return n;
    }
    let fallback = FALLBACK.get_or_init(|| Regex::new(r"(?i)(?:line |:)(\d+)").expect("valid regex"));
    fallback
        .captures(msg)
        .and_then(|c| c[1].parse::<usize>().ok())
        .filter(|&l| l > 0)
        .unwrap_or(1)
}

fn snippet(text: &str, line: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    let idx = line.saturating_sub(1).min(lines.len() - 1);
    let from = idx.saturating_sub(2);
    let to = (idx + 2).min(lines.len() - 1);
    (from..=to)
        .map(|i| {
            let marker = if i == idx { ">" } else { " " };
            format!("{marker} {:>4} | {}", i + 1, lines[i])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn inspect_file(path: &Path, text: &str, external: bool, timeout_ms: u64) -> Option<SyntaxIssue> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "json" => serde_json::from_str::<Value>(text).err().map(|e| SyntaxIssue {
            line: e.line().max(1),
            message: e.to_string(),
        }),
        "rs" => check_rust_syntax(text),
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" if external => {
            let p = path.to_string_lossy();
            let loader = match ext.as_str() {
                "jsx" => "jsx",
                "tsx" => "tsx",
                "ts" | "mts" | "cts" => "ts",
                _ => "js",
            };
            if which::which("bun").is_ok() {
                let script = "new Bun.Transpiler({ loader: process.argv[1] }).transformSync(await Bun.file(process.argv[2]).text())";
                if let Some(err) = run_external("bun", &["-e", script, loader, &p], timeout_ms).await {
                    let summary = err
                        .lines()
                        .find(|l| {
                            let t = l.trim_start();
                            t.starts_with("error:") || t.contains("SyntaxError")
                        })
                        .or_else(|| err.lines().find(|l| !l.trim().is_empty()))
                        .unwrap_or(&err)
                        .trim()
                        .to_string();
                    return Some(SyntaxIssue {
                        line: parse_line_number(&err),
                        message: summary,
                    });
                }
            } else if matches!(ext.as_str(), "js" | "mjs" | "cjs")
                && which::which("node").is_ok()
                && let Some(err) = run_external("node", &["--check", &p], timeout_ms).await
            {
                let summary = err
                    .lines()
                    .find(|l| l.contains("SyntaxError"))
                    .or_else(|| err.lines().next())
                    .unwrap_or(&err)
                    .to_string();
                return Some(SyntaxIssue {
                    line: parse_line_number(&err),
                    message: summary,
                });
            }
            None
        }
        "py" if external => {
            if which::which("python3").is_ok() {
                let p = path.to_string_lossy();
                let script = "import ast, sys; ast.parse(open(sys.argv[1], encoding='utf-8').read(), sys.argv[1])";
                if let Some(err) = run_external("python3", &["-B", "-c", script, &p], timeout_ms).await
                    && (err.contains("SyntaxError") || err.contains("IndentationError") || err.contains("TabError"))
                {
                    let summary = err.lines().rfind(|l| !l.trim().is_empty()).unwrap_or(&err).to_string();
                    return Some(SyntaxIssue {
                        line: parse_line_number(&err),
                        message: summary,
                    });
                }
            }
            None
        }
        _ => None,
    }
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let external = settings.get("externalCheck") != Some(&Value::Bool(false));
        let timeout_ms = num_setting(&settings, "timeoutMs", 1500.0).clamp(100.0, 10_000.0) as u64;
        let max_bytes = num_setting(&settings, "maxBytes", 524_288.0).max(1024.0) as u64;
        let cwd = api.cwd();

        api.on::<hook::ToolResult, _, _>(move |ev, _ctx| {
            let cwd = cwd.clone();
            async move {
                if !ev.ran
                    || ev.result.is_error()
                    || !matches!(ev.call.name.as_str(), "edit" | "write" | "hash_edit")
                {
                    return Ok(None);
                }
                let raw_path = ev
                    .result
                    .details
                    .as_ref()
                    .and_then(|d| d.get("path"))
                    .and_then(Value::as_str)
                    .or_else(|| ev.call.input.get("path").and_then(Value::as_str));
                let Some(raw_path) = raw_path else {
                    return Ok(None);
                };
                let resolved = resolve_path(raw_path, &cwd);
                let path = Path::new(&resolved);
                let Ok(meta) = tokio::fs::metadata(path).await else {
                    return Ok(None);
                };
                if !meta.is_file() || meta.len() > max_bytes {
                    return Ok(None);
                }
                let Ok(bytes) = tokio::fs::read(path).await else {
                    return Ok(None);
                };
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    return Ok(None);
                };
                let Some(issue) = inspect_file(path, text, external, timeout_ms).await else {
                    return Ok(None);
                };
                let disp = path
                    .strip_prefix(&cwd)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_else(|_| resolved.clone());
                let ctx_lines = snippet(text, issue.line);
                let note = format!(
                    "[syntax-check] Syntax error in {disp} (line {}): {}\n{ctx_lines}\nFix this syntax error before continuing.",
                    issue.line, issue.message
                );
                let mut result = ev.result;
                result.content.push(UserContent::text(note));
                Ok(Some(ToolResultResult { result: Some(result) }))
            }
        });

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bun_and_python_line_numbers_past_code_frame_colons() {
        let bun_err = "1 | const url = \"http://localhost:3000\";\n2 | export function f(x: number {\n                                  ^\nerror: Expected \")\" but found \"{\"\n    at input.ts:2:31\n";
        assert_eq!(parse_line_number(bun_err), 2);
        let py_err =
            "  File \"/tmp/mod.py\", line 14\n    return (1 + 2\n           ^\nSyntaxError: '(' was never closed\n";
        assert_eq!(parse_line_number(py_err), 14);
    }
}
