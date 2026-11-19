//! search-tools: bounded, gitignore-aware `find_files` and `search_text` tools designed for
//! small models so file discovery does not depend on error-prone shell flags or blow up context.
//!
//! settings: { maxResults?: number (default 50), maxFileBytes?: number (default 1048576) }

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use globset::{GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};
use regex::RegexBuilder;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_ai::JsonObject;
use sasacode_plugin_api::{PluginApi, ToolContext, ToolDef, ToolKind, ToolResult};
use sasacode_tools::util::resolve_path;

use crate::repair::js::num_setting;

const DEFAULT_MAX_RESULTS: usize = 50;
const CAP_MAX_RESULTS: usize = 200;
const DEFAULT_MAX_FILE_BYTES: usize = 1_048_576;
const MAX_WALK_ENTRIES: usize = 50_000;
const MAX_LINE_CHARS: usize = 200;
const HEAD_BYTES: usize = 8192;

pub(crate) const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".venv",
    "__pycache__",
    ".turbo",
    ".cache",
];

fn str_arg<'a>(args: &'a JsonObject, key: &str) -> &'a str {
    args.get(key).and_then(Value::as_str).unwrap_or("")
}

fn bool_arg(args: &JsonObject, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn limit_arg(args: &JsonObject, default_limit: usize) -> usize {
    let raw = args
        .get("max_results")
        .and_then(Value::as_f64)
        .map(|n| n.trunc() as i64)
        .unwrap_or(default_limit as i64);
    raw.clamp(1, CAP_MAX_RESULTS as i64) as usize
}

/// Normalize path separators to `/` for consistent cross-platform matching and display.
pub(crate) fn slash_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Parse simple positive rules from `.gitignore` into a `GlobSet` relative to `root`.
fn load_gitignore(root: &Path) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    let Ok(bytes) = std::fs::read(root.join(".gitignore")) else {
        return GlobSet::empty();
    };
    let text = String::from_utf8_lossy(&bytes);
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let pat = line.strip_prefix("./").unwrap_or(line);
        let anchored = pat.starts_with('/');
        let pat = pat.strip_prefix('/').unwrap_or(pat);
        let pat = pat.strip_suffix('/').unwrap_or(pat);
        if pat.is_empty() {
            continue;
        }
        let has_slash = pat.contains('/');
        let patterns: Vec<String> = if anchored || has_slash {
            vec![pat.to_string(), format!("{pat}/**")]
        } else {
            vec![
                pat.to_string(),
                format!("**/{pat}"),
                format!("{pat}/**"),
                format!("**/{pat}/**"),
            ]
        };
        for p in patterns {
            if let Ok(g) = GlobBuilder::new(&p).literal_separator(true).build() {
                builder.add(g);
            }
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Deterministic, bounded walk under `root`, returning `/` relative paths sorted by name.
pub(crate) fn walk_project_files(
    root: &Path,
    extra_ignore_root: Option<&Path>,
    max_depth: usize,
    max_entries: usize,
    signal: Option<&CancellationToken>,
) -> Vec<String> {
    let mut ignores = vec![load_gitignore(root)];
    if let Some(extra) = extra_ignore_root
        && extra != root
    {
        ignores.push(load_gitignore(extra));
    }
    struct WalkCtx<'a> {
        max_depth: usize,
        max_entries: usize,
        ignores: &'a [GlobSet],
        signal: Option<&'a CancellationToken>,
        visited: usize,
        out: Vec<String>,
    }

    fn visit(dir: &Path, rel: &str, depth: usize, ctx: &mut WalkCtx<'_>) {
        if depth > ctx.max_depth
            || ctx.visited >= ctx.max_entries
            || ctx.signal.is_some_and(CancellationToken::is_cancelled)
        {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if ctx.visited >= ctx.max_entries || ctx.signal.is_some_and(CancellationToken::is_cancelled) {
                return;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            // Do not traverse symlinked directories to avoid cycles.
            if ft.is_symlink() && ft.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = ft.is_dir();
            if is_dir && SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            let rel_path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if ctx.ignores.iter().any(|ig| ig.is_match(&rel_path)) {
                continue;
            }
            ctx.visited += 1;
            if is_dir {
                visit(&entry.path(), &rel_path, depth + 1, ctx);
            } else if ft.is_file() {
                ctx.out.push(rel_path);
            }
        }
    }

    let mut ctx = WalkCtx {
        max_depth,
        max_entries,
        ignores: &ignores,
        signal,
        visited: 0,
        out: Vec::new(),
    };
    visit(root, "", 0, &mut ctx);
    ctx.out
}

enum Pattern {
    Glob { matcher: GlobMatcher, match_basename: bool },
    Substring(String),
}

impl Pattern {
    fn compile(raw: &str) -> Result<Self, String> {
        let p = raw.trim();
        if p.contains(['*', '?', '[', '{']) {
            let match_basename = !p.contains('/');
            let matcher = GlobBuilder::new(p)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("Invalid glob pattern \"{p}\": {e}"))?
                .compile_matcher();
            Ok(Pattern::Glob {
                matcher,
                match_basename,
            })
        } else {
            Ok(Pattern::Substring(p.to_lowercase()))
        }
    }

    fn matches(&self, rel: &str, display: &str) -> bool {
        match self {
            Pattern::Glob {
                matcher,
                match_basename,
            } => {
                if *match_basename {
                    let base = rel.rsplit('/').next().unwrap_or(rel);
                    matcher.is_match(base) || matcher.is_match(rel)
                } else {
                    matcher.is_match(rel) || matcher.is_match(display)
                }
            }
            Pattern::Substring(sub) => rel.to_lowercase().contains(sub) || display.to_lowercase().contains(sub),
        }
    }
}

fn display_rel(cwd: &Path, target_root: &Path, rel_to_root: &str) -> String {
    let full = target_root.join(rel_to_root);
    if let Ok(rel_cwd) = full.strip_prefix(cwd) {
        let s = slash_path(rel_cwd);
        if !s.is_empty() {
            return s;
        }
    }
    slash_path(&full)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

async fn run_find_files(args: JsonObject, ctx: ToolContext, default_limit: usize) -> anyhow::Result<ToolResult> {
    let raw_path = str_arg(&args, "path");
    let target_str = if raw_path.is_empty() {
        ctx.cwd.to_string_lossy().into_owned()
    } else {
        resolve_path(raw_path, &ctx.cwd)
    };
    let pattern_raw = str_arg(&args, "pattern").trim().to_string();
    let pattern = if pattern_raw.is_empty() {
        None
    } else {
        match Pattern::compile(&pattern_raw) {
            Ok(p) => Some(p),
            Err(msg) => return Ok(ToolResult::error(msg)),
        }
    };
    let limit = limit_arg(&args, default_limit);
    let cwd = ctx.cwd.clone();
    let signal = ctx.signal.clone();

    tokio::task::spawn_blocking(move || {
        let target = Path::new(&target_str);
        let Ok(meta) = std::fs::metadata(target) else {
            return ToolResult::error(format!("Path not found: {target_str}"));
        };
        if meta.is_file() {
            let shown = target
                .strip_prefix(&cwd)
                .map(slash_path)
                .unwrap_or_else(|_| slash_path(target));
            return ToolResult::text(shown);
        }
        if !meta.is_dir() {
            return ToolResult::error(format!("Not a file or directory: {target_str}"));
        }
        let files = walk_project_files(target, Some(&cwd), 32, MAX_WALK_ENTRIES, Some(&signal));
        if signal.is_cancelled() {
            return ToolResult::error("The operation was aborted");
        }
        let mut matched: Vec<String> = files
            .into_iter()
            .filter_map(|rel| {
                let disp = display_rel(&cwd, target, &rel);
                if pattern.as_ref().is_none_or(|p| p.matches(&rel, &disp)) {
                    Some(disp)
                } else {
                    None
                }
            })
            .collect();
        matched.sort();
        matched.dedup();
        if matched.is_empty() {
            return ToolResult::text("No files found.");
        }
        let total = matched.len();
        let shown = total.min(limit);
        let mut out = matched[..shown].join("\n");
        if total > shown {
            out.push_str(&format!(
                "\n[Showing {shown} of {total} matches; narrow pattern or path.]"
            ));
        }
        let mut details = JsonObject::new();
        details.insert("total".into(), json!(total));
        details.insert("shown".into(), json!(shown));
        ToolResult::text(out).with_details(details)
    })
    .await
    .map_err(anyhow::Error::from)
}

async fn run_search_text(
    args: JsonObject,
    ctx: ToolContext,
    default_limit: usize,
    max_file_bytes: usize,
) -> anyhow::Result<ToolResult> {
    let query = str_arg(&args, "query").to_string();
    if query.is_empty() {
        return Ok(ToolResult::error("query must not be empty"));
    }
    let use_regex = bool_arg(&args, "regex");
    let ignore_case = bool_arg(&args, "ignore_case");
    let regex_pat = if use_regex {
        query.clone()
    } else {
        regex::escape(&query)
    };
    let matcher = match RegexBuilder::new(&regex_pat).case_insensitive(ignore_case).build() {
        Ok(r) => r,
        Err(e) => return Ok(ToolResult::error(format!("Invalid regex: {e}"))),
    };
    let glob_raw = str_arg(&args, "glob").trim().to_string();
    let glob_filter = if glob_raw.is_empty() {
        None
    } else {
        match Pattern::compile(&glob_raw) {
            Ok(p) => Some(p),
            Err(msg) => return Ok(ToolResult::error(msg)),
        }
    };
    let raw_path = str_arg(&args, "path");
    let target_str = if raw_path.is_empty() {
        ctx.cwd.to_string_lossy().into_owned()
    } else {
        resolve_path(raw_path, &ctx.cwd)
    };
    let limit = limit_arg(&args, default_limit);
    let cwd = ctx.cwd.clone();
    let signal = ctx.signal.clone();

    tokio::task::spawn_blocking(move || {
        let target = Path::new(&target_str);
        let Ok(meta) = std::fs::metadata(target) else {
            return ToolResult::error(format!("Path not found: {target_str}"));
        };
        let candidates: Vec<(String, std::path::PathBuf)> = if meta.is_file() {
            let disp = target
                .strip_prefix(&cwd)
                .map(slash_path)
                .unwrap_or_else(|_| slash_path(target));
            vec![(disp, target.to_path_buf())]
        } else if meta.is_dir() {
            walk_project_files(target, Some(&cwd), 32, MAX_WALK_ENTRIES, Some(&signal))
                .into_iter()
                .filter_map(|rel| {
                    let disp = display_rel(&cwd, target, &rel);
                    if glob_filter.as_ref().is_none_or(|g| g.matches(&rel, &disp)) {
                        Some((disp, target.join(rel)))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            return ToolResult::error(format!("Not a file or directory: {target_str}"));
        };
        let mut hits: Vec<String> = Vec::new();
        let mut has_more = false;
        let mut seen_files: HashSet<String> = HashSet::new();
        for (disp, path) in candidates {
            if signal.is_cancelled() {
                return ToolResult::error("The operation was aborted");
            }
            if !seen_files.insert(disp.clone()) {
                continue;
            }
            let Ok(m) = std::fs::metadata(&path) else {
                continue;
            };
            if m.len() as usize > max_file_bytes {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if bytes[..bytes.len().min(HEAD_BYTES)].contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (idx, line) in text.split('\n').enumerate() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if matcher.is_match(line) {
                    if hits.len() >= limit {
                        has_more = true;
                        break;
                    }
                    let snippet = truncate_chars(line.trim(), MAX_LINE_CHARS);
                    hits.push(format!("{disp}:{}: {snippet}", idx + 1));
                }
            }
            if has_more {
                break;
            }
        }
        if hits.is_empty() {
            return ToolResult::text(format!("No matches found for \"{query}\"."));
        }
        let shown = hits.len();
        let mut out = hits.join("\n");
        if has_more {
            out.push_str(&format!(
                "\n[Showing first {shown} matches; more exist. Narrow query, glob, or path.]"
            ));
        }
        let mut details = JsonObject::new();
        details.insert("shown".into(), json!(shown));
        details.insert("truncated".into(), json!(has_more));
        ToolResult::text(out).with_details(details)
    })
    .await
    .map_err(anyhow::Error::from)
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let default_limit =
            (num_setting(&settings, "maxResults", DEFAULT_MAX_RESULTS as f64) as usize).clamp(1, CAP_MAX_RESULTS);
        let max_file_bytes = (num_setting(&settings, "maxFileBytes", DEFAULT_MAX_FILE_BYTES as f64) as usize).max(1024);

        let find_paths = Arc::new(|args: &JsonObject, cwd: &Path| {
            let p = str_arg(args, "path");
            vec![if p.is_empty() {
                cwd.to_string_lossy().into_owned()
            } else {
                resolve_path(p, cwd)
            }]
        });
        let find_paths2 = find_paths.clone();

        api.register_tool(
            ToolDef::new(
                "find_files",
                "Find files in the workspace by glob or path substring (respects .gitignore; skips .git/node_modules/target; capped output).",
                json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Glob (e.g. '*.rs', 'src/**/*.ts') or substring" },
                        "path": { "type": "string", "description": "Directory to search (default: working directory)" },
                        "max_results": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum files to return (default 50)" }
                    },
                    "additionalProperties": false
                }),
                ToolKind::Read,
                move |args, ctx| run_find_files(args, ctx, default_limit),
            )
            .concurrent(true)
            .always_load(true)
            .paths(move |a, c| find_paths(a, c))
            .summary(|a| {
                let p = str_arg(a, "pattern");
                if p.is_empty() { ".".into() } else { p.to_string() }
            })
            .into_ref(),
        );

        api.register_tool(
            ToolDef::new(
                "search_text",
                "Search for literal text or a regex across files (respects .gitignore; skips binary and large files; returns path:line: text).",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "minLength": 1, "description": "Literal text or regex to search for" },
                        "path": { "type": "string", "description": "File or directory to search (default: working directory)" },
                        "glob": { "type": "string", "description": "Optional filename or path glob filter (e.g. '*.rs')" },
                        "regex": { "type": "boolean", "description": "Interpret query as a regex (default false)" },
                        "ignore_case": { "type": "boolean", "description": "Case-insensitive search (default false)" },
                        "max_results": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum matches to return (default 50)" }
                    },
                    "required": ["query"],
                    "additionalProperties": false
                }),
                ToolKind::Read,
                move |args, ctx| run_search_text(args, ctx, default_limit, max_file_bytes),
            )
            .concurrent(true)
            .always_load(true)
            .paths(move |a, c| find_paths2(a, c))
            .summary(|a| str_arg(a, "query").to_string())
            .into_ref(),
        );

        Ok(())
    })
}
