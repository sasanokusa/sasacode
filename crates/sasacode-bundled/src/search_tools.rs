//! search-tools: bounded, gitignore-aware `find_files` and `search_text` tools designed for
//! small models so file discovery does not depend on error-prone shell flags or blow up context.
//!
//! settings: { maxResults?: number (default 50), maxFileBytes?: number (default 1048576) }

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
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

/// Secret-looking filenames always excluded from directory walks.
///
/// Limitation: `PluginApi` / `HostServices` does not expose `PermissionPolicy` or user `deny` rules
/// (such as `read(secrets/**)`) to plugins (`api.settings()` only returns this plugin's own
/// settings). While `.permissions_as("read")` enforces `read(...)` rules on the tool's declared
/// `path` (protecting single-file calls like `search_text(path=".env", ...)`), directory walks
/// cannot evaluate arbitrary user `read` deny rules per candidate and therefore unconditionally
/// skip common secret files here.
fn is_secret_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        ".env" | ".envrc" | ".npmrc" | ".pypirc" | ".netrc" | ".pgpass"
    ) || (lower.starts_with(".env.") && !matches!(lower.as_str(), ".env.example" | ".env.sample"))
    {
        return true;
    }
    lower.ends_with(".env")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower.ends_with(".jks")
        || lower.ends_with(".keystore")
        || lower.starts_with("id_rsa")
        || lower.starts_with("id_ed25519")
        || lower.starts_with("id_ecdsa")
        || lower.starts_with("id_dsa")
}

struct GitignoreRule {
    negated: bool,
    dir_only: bool,
}

struct GitignoreLayer {
    /// Byte offset into `rel_from_origin` where paths relative to this `.gitignore`'s directory start.
    prefix_len: usize,
    rules: Vec<GitignoreRule>,
    rule_of_glob: Vec<usize>,
    set: GlobSet,
}

impl GitignoreLayer {
    fn match_state(&self, entry_from_origin: &str, is_dir: bool, scratch: &mut Vec<usize>) -> Option<bool> {
        let rel = entry_from_origin.get(self.prefix_len..)?;
        if rel.is_empty() {
            return None;
        }
        scratch.clear();
        self.set.matches_into(rel, scratch);
        let latest = scratch
            .iter()
            .filter_map(|&gi| {
                let rule_idx = self.rule_of_glob[gi];
                let rule = &self.rules[rule_idx];
                if rule.dir_only && !is_dir { None } else { Some(rule_idx) }
            })
            .max()?;
        Some(!self.rules[latest].negated)
    }
}

/// Escape literal `{` and `}` so `globset` does not treat `.gitignore` braces as alternation.
fn escape_gitignore_glob(pat: &str) -> String {
    let mut out = String::with_capacity(pat.len());
    let mut chars = pat.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                out.push('\\');
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '{' | '}' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Parse `.gitignore` rules (including `!pattern` negation and trailing `/` dir-only) at `dir`.
fn load_gitignore(dir: &Path, prefix_len: usize) -> Option<GitignoreLayer> {
    let bytes = std::fs::read(dir.join(".gitignore")).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut builder = GlobSetBuilder::new();
    let mut rules = Vec::new();
    let mut rule_of_glob = Vec::new();

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (negated, rest) = if let Some(r) = line.strip_prefix('!') {
            (true, r)
        } else {
            (false, line)
        };
        let pat = rest.strip_prefix("./").unwrap_or(rest);
        let anchored = pat.starts_with('/');
        let pat = pat.trim_start_matches('/');
        let dir_only = pat.ends_with('/');
        let pat = pat.trim_end_matches('/');
        if pat.is_empty() {
            continue;
        }
        let has_slash = pat.contains('/');
        let pat = escape_gitignore_glob(pat);

        let mut add_rule = |globs: Vec<String>, rule_dir_only: bool| {
            let rule_idx = rules.len();
            let mut added = false;
            for p in globs {
                if let Ok(g) = GlobBuilder::new(&p)
                    .literal_separator(true)
                    .backslash_escape(true)
                    .build()
                {
                    builder.add(g);
                    rule_of_glob.push(rule_idx);
                    added = true;
                }
            }
            if added {
                rules.push(GitignoreRule {
                    negated,
                    dir_only: rule_dir_only,
                });
            }
        };

        if anchored || has_slash {
            add_rule(vec![pat], dir_only);
        } else {
            add_rule(vec![pat.clone(), format!("**/{pat}")], dir_only);
        }
    }

    if rules.is_empty() {
        return None;
    }
    let set = builder.build().ok()?;
    Some(GitignoreLayer {
        prefix_len,
        rules,
        rule_of_glob,
        set,
    })
}

fn is_ignored(layers: &[GitignoreLayer], entry_from_origin: &str, is_dir: bool, scratch: &mut Vec<usize>) -> bool {
    let mut ignored = false;
    for layer in layers {
        if let Some(decision) = layer.match_state(entry_from_origin, is_dir, scratch) {
            ignored = decision;
        }
    }
    ignored
}

/// Deterministic, bounded walk under `root`, returning `/` relative paths sorted by name.
pub(crate) fn walk_project_files(
    root: &Path,
    extra_ignore_root: Option<&Path>,
    max_depth: usize,
    max_entries: usize,
    signal: Option<&CancellationToken>,
) -> Vec<String> {
    let mut layers = Vec::new();
    let mut scratch = Vec::new();
    let mut initial_rel = String::new();
    if let Some(extra) = extra_ignore_root
        && let Ok(rel) = root.strip_prefix(extra)
    {
        let mut segs: Vec<&OsStr> = Vec::new();
        let mut escaped = false;
        for comp in rel.components() {
            match comp {
                Component::Normal(s) if !s.is_empty() && s != "." => segs.push(s),
                Component::CurDir => {}
                Component::ParentDir if segs.pop().is_none() => {
                    escaped = true;
                    break;
                }
                _ => {}
            }
        }
        if !escaped && !segs.is_empty() {
            if let Some(layer) = load_gitignore(extra, 0) {
                layers.push(layer);
            }
            let mut cur = extra.to_path_buf();
            let last_idx = segs.len() - 1;
            for (idx, seg_os) in segs.iter().enumerate() {
                cur.push(seg_os);
                let seg = seg_os.to_string_lossy();
                if !initial_rel.is_empty() {
                    initial_rel.push('/');
                }
                initial_rel.push_str(&seg);
                if is_ignored(&layers, &initial_rel, true, &mut scratch) {
                    return Vec::new();
                }
                if idx < last_idx
                    && let Some(layer) = load_gitignore(&cur, initial_rel.len() + 1)
                {
                    layers.push(layer);
                }
            }
        }
    }
    let target_prefix_len = if initial_rel.is_empty() {
        0
    } else {
        initial_rel.len() + 1
    };

    struct WalkCtx<'a> {
        max_depth: usize,
        max_entries: usize,
        target_prefix_len: usize,
        layers: Vec<GitignoreLayer>,
        scratch: Vec<usize>,
        signal: Option<&'a CancellationToken>,
        visited: usize,
        out: Vec<String>,
    }

    fn visit(dir: &Path, rel_from_origin: &str, depth: usize, ctx: &mut WalkCtx<'_>) {
        if depth > ctx.max_depth
            || ctx.visited >= ctx.max_entries
            || ctx.signal.is_some_and(CancellationToken::is_cancelled)
        {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let layer_prefix_len = if rel_from_origin.is_empty() {
            0
        } else {
            rel_from_origin.len() + 1
        };
        let pushed_layer = if let Some(layer) = load_gitignore(dir, layer_prefix_len) {
            ctx.layers.push(layer);
            true
        } else {
            false
        };

        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if ctx.visited >= ctx.max_entries || ctx.signal.is_some_and(CancellationToken::is_cancelled) {
                break;
            }
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            // Symlinks are not followed (DirEntry::file_type does not traverse them, avoiding cycles and escapes).
            if ft.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = ft.is_dir();
            if is_dir && SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            if !is_dir && is_secret_file(&name) {
                continue;
            }
            let entry_from_origin = if rel_from_origin.is_empty() {
                name.clone()
            } else {
                format!("{rel_from_origin}/{name}")
            };
            if is_ignored(&ctx.layers, &entry_from_origin, is_dir, &mut ctx.scratch) {
                continue;
            }
            ctx.visited += 1;
            if is_dir {
                visit(&entry.path(), &entry_from_origin, depth + 1, ctx);
            } else if ft.is_file() {
                let rel_to_target = &entry_from_origin[ctx.target_prefix_len..];
                ctx.out.push(rel_to_target.to_string());
            }
        }

        if pushed_layer {
            ctx.layers.pop();
        }
    }

    let mut ctx = WalkCtx {
        max_depth,
        max_entries,
        target_prefix_len,
        layers,
        scratch,
        signal,
        visited: 0,
        out: Vec::new(),
    };
    visit(root, &initial_rel, 0, &mut ctx);
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

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !out.has_root() {
                    out.push(c.as_os_str());
                }
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

fn display_path(cwd: &Path, full_path: &Path) -> String {
    let full = normalize_lexical(full_path);
    let norm_cwd = normalize_lexical(cwd);
    if let Ok(rel_cwd) = full.strip_prefix(&norm_cwd) {
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
            return ToolResult::text(display_path(&cwd, target));
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
                let disp = display_path(&cwd, &target.join(&rel));
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
            vec![(display_path(&cwd, target), target.to_path_buf())]
        } else if meta.is_dir() {
            walk_project_files(target, Some(&cwd), 32, MAX_WALK_ENTRIES, Some(&signal))
                .into_iter()
                .filter_map(|rel| {
                    let full = target.join(&rel);
                    let disp = display_path(&cwd, &full);
                    if glob_filter.as_ref().is_none_or(|g| g.matches(&rel, &disp)) {
                        Some((disp, full))
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
            .permissions_as("read")
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
            .permissions_as("read")
            .paths(move |a, c| find_paths2(a, c))
            .summary(|a| str_arg(a, "query").to_string())
            .into_ref(),
        );

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_file_names_cover_new_and_existing_patterns() {
        for secret in [
            ".env",
            ".env.local",
            ".envrc",
            ".npmrc",
            ".pypirc",
            ".netrc",
            ".pgpass",
            "prod.env",
            "STAGING.ENV",
            "server.pem",
            "private.key",
            "cert.p12",
            "cert.pfx",
            "keystore.jks",
            "app.JKS",
            "release.keystore",
            "id_rsa",
            "id_ed25519",
            "id_ecdsa",
            "id_dsa",
        ] {
            assert!(is_secret_file(secret), "expected secret: {secret}");
        }
        for allowed in [
            ".env.example",
            ".env.sample",
            "prod.env.example",
            "main.rs",
            "README.md",
        ] {
            assert!(!is_secret_file(allowed), "expected non-secret: {allowed}");
        }
    }

    #[test]
    fn walk_project_files_normalizes_double_slash_and_non_ascii_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let target_real = root.join("日本語/サブ/c");
        std::fs::create_dir_all(&target_real).unwrap();
        std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(root.join("日本語/サブ/.gitignore"), "!keep.log\n*.tmp\n").unwrap();
        std::fs::write(target_real.join("keep.log"), "ok\n").unwrap();
        std::fs::write(target_real.join("drop.log"), "no\n").unwrap();
        std::fs::write(target_real.join("drop.tmp"), "no\n").unwrap();
        std::fs::write(target_real.join("note.txt"), "ok\n").unwrap();

        let unnorm = PathBuf::from(format!("{}/日本語//サブ/./c", root.display()));
        let mut found = walk_project_files(&unnorm, Some(root), 32, MAX_WALK_ENTRIES, None);
        found.sort();
        assert_eq!(found, vec!["keep.log", "note.txt"]);

        let triple_slash = PathBuf::from(format!("{}/日本語///サブ/c", root.display()));
        let mut found3 = walk_project_files(&triple_slash, Some(root), 32, MAX_WALK_ENTRIES, None);
        found3.sort();
        assert_eq!(found3, vec!["keep.log", "note.txt"]);

        // Escaping extra_ignore_root via `..` must not apply extra_ignore_root's layers as a prefix.
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("drop.log"), "still here\n").unwrap();
        let escaped = PathBuf::from(format!("{}/日本語/../outside", root.display()));
        assert_eq!(
            walk_project_files(&escaped, Some(&root.join("日本語")), 32, MAX_WALK_ENTRIES, None),
            vec!["drop.log"]
        );
    }
}
