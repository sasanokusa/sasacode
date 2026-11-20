//! error-hints: turns dead-end tool errors into actionable next-step hints for small models:
//! - ENOENT on `read` / `edit` -> closest existing paths (`Did you mean: ...`)
//! - `Unknown tool` matching an Agent Skill -> points to `read(path=".../SKILL.md")`, or closest tools
//! - failing test runs -> likely source files under test inferred from test imports / stack traces
//!
//! settings: { paths?: boolean (true), unknownTools?: boolean (true), testTargets?: boolean (true) }

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;

use sasacode_ai::UserContent;
use sasacode_plugin_api::{PluginApi, ToolResult, ToolResultResult, hook, sasacode_home};
use sasacode_tools::util::resolve_path;

use crate::repair::schema::edit_distance;
use crate::search_tools::{slash_path, walk_project_files};

const WALK_DEPTH: usize = 10;
const WALK_LIMIT: usize = 5_000;

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

fn result_text(r: &ToolResult) -> String {
    r.content
        .iter()
        .filter_map(UserContent::as_text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

// ── 1. ENOENT path suggestions ───────────────────────────────────────

fn file_stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

fn shared_parent_segments(a: &str, b: &str) -> usize {
    let ap: Vec<&str> = a.split('/').rev().skip(1).collect();
    let bp: Vec<&str> = b.split('/').rev().skip(1).collect();
    ap.iter().filter(|seg| !seg.is_empty() && bp.contains(seg)).count()
}

pub(crate) fn suggest_paths(cwd: &Path, requested: &str) -> Vec<String> {
    if requested.trim().is_empty() {
        return Vec::new();
    }
    let resolved = resolve_path(requested, cwd);
    let cwd_slash = slash_path(cwd);
    let req_slash = slash_path(Path::new(&resolved));
    let req_rel = req_slash
        .strip_prefix(&format!("{cwd_slash}/"))
        .unwrap_or(requested)
        .replace('\\', "/");
    let req_rel = req_rel.strip_prefix("./").unwrap_or(&req_rel);
    let req_file = req_rel.rsplit('/').next().unwrap_or(req_rel);
    if req_file.is_empty() {
        return Vec::new();
    }
    let req_file_lower = req_file.to_lowercase();
    let req_stem_lower = file_stem(&req_file_lower).to_string();
    let req_rel_lower = req_rel.to_lowercase();

    type PathScore = (u8, Reverse<usize>, usize, usize);
    let files = walk_project_files(cwd, None, WALK_DEPTH, WALK_LIMIT, None);
    let mut ranked: Vec<(PathScore, String)> = Vec::new();
    for cand in files {
        let cand_file = cand.rsplit('/').next().unwrap_or(&cand);
        let cand_lower = cand_file.to_lowercase();
        let cand_stem_lower = file_stem(&cand_lower);
        let dist = edit_distance(&req_file_lower, &cand_lower);
        let tier = if cand_file == req_file {
            Some(0)
        } else if cand_lower == req_file_lower {
            Some(1)
        } else if !req_stem_lower.is_empty() && cand_stem_lower == req_stem_lower {
            Some(2)
        } else if req_file_lower.len() >= 3 && dist <= 3 {
            Some(3)
        } else if (req_file_lower.len() >= 4 && cand_lower.contains(&req_file_lower))
            || (req_rel_lower.contains('/') && cand.to_lowercase().ends_with(&req_rel_lower))
        {
            Some(4)
        } else {
            None
        };
        let Some(tier) = tier else { continue };
        let shared = shared_parent_segments(req_rel, &cand);
        let len_diff = cand.len().abs_diff(req_rel.len());
        ranked.push(((tier, Reverse(shared), dist, len_diff), cand));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    ranked.into_iter().take(3).map(|(_, p)| p).collect()
}

// ── 2. Unknown tool -> skill or closest tool ─────────────────────────

#[derive(Clone, Debug)]
struct SkillEntry {
    name: String,
    path: PathBuf,
}

fn parse_skill_name(text: &str) -> Option<String> {
    let rest = text.strip_prefix("---")?;
    let body = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n'))?;
    let end = body.find("\n---")?;
    for raw in body[..end].lines() {
        let line = raw.trim_end_matches('\r').trim();
        if let Some(after) = line.strip_prefix("name:") {
            let val = after.trim();
            let val = val
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| val.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(val);
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

fn discover_skill_entries(home: &Path, cwd: &Path) -> Vec<SkillEntry> {
    let dirs = [
        home.join("builtin-skills"),
        home.join("skills"),
        cwd.join(".sasacode").join("skills"),
    ];
    let mut by_name: BTreeMap<String, SkillEntry> = BTreeMap::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut names: Vec<_> = rd.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for entry in names {
            let path = dir.join(entry).join("SKILL.md");
            if !path.is_file() {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
            if let Some(name) = parse_skill_name(&head) {
                by_name.insert(name.clone(), SkillEntry { name, path });
            }
        }
    }
    by_name.into_values().collect()
}

fn unknown_tool_hint(called: &str, home: &Path, cwd: &Path, tool_names: &[String]) -> Option<String> {
    let called_norm = norm(called);
    let skills = discover_skill_entries(home, cwd);
    if let Some(skill) = skills
        .iter()
        .find(|s| s.name == called || s.name.eq_ignore_ascii_case(called) || norm(&s.name) == called_norm)
    {
        return Some(format!(
            "[hint] \"{}\" is an Agent Skill, not a tool. Read its instructions with read(path=\"{}\").",
            skill.name,
            slash_path(&skill.path)
        ));
    }
    let mut candidates: Vec<(usize, &str)> = tool_names
        .iter()
        .filter_map(|t| {
            let tn = norm(t);
            let d = edit_distance(&called_norm, &tn);
            if d <= 3 || (called_norm.len() >= 3 && (tn.contains(&called_norm) || called_norm.contains(&tn))) {
                Some((d, t.as_str()))
            } else {
                None
            }
        })
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
    let similar: Vec<&str> = candidates.into_iter().take(3).map(|(_, t)| t).collect();
    (!similar.is_empty()).then(|| format!("[hint] Similar tools: {}", similar.join(", ")))
}

// ── 3. Test failure -> source files under test ───────────────────────

fn is_test_path(rel: &str) -> bool {
    let lower = rel.to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    lower
        .split('/')
        .any(|p| matches!(p, "test" | "tests" | "__tests__" | "spec" | "specs"))
        || [".test.", ".spec.", "_test.", "_spec.", "_tests."]
            .iter()
            .any(|m| name.contains(m))
        || name.starts_with("test_")
}

fn is_source_ext(rel: &str) -> bool {
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "rb"
            | "java"
            | "kt"
            | "c"
            | "cpp"
            | "cc"
            | "h"
            | "hpp"
            | "cs"
            | "swift"
    )
}

fn looks_like_test_run(command: &str, output: &str) -> bool {
    let cmd_l = command.to_lowercase();
    if ["test", "spec", "pytest", "vitest", "jest", "mocha", "ava", "rspec"]
        .iter()
        .any(|k| cmd_l.contains(k))
    {
        return true;
    }
    [
        "test result: FAILED",
        "AssertionError",
        "assertion failed",
        "assert_eq!",
        "expect(",
        "FAIL ",
        "FAILED ",
        "(fail)",
    ]
    .iter()
    .any(|m| output.contains(m))
}

fn resolve_existing_rel(cwd: &Path, raw: &str) -> Option<String> {
    let cleaned = raw
        .trim_matches(|c: char| matches!(c, '\'' | '"' | '`' | '(' | ')' | '[' | ']' | ',' | ';'))
        .trim_end_matches(':');
    let without_loc = re_strip_line_col(cleaned);
    let candidate = Path::new(without_loc);
    let abs = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        PathBuf::from(resolve_path(without_loc, cwd))
    };
    if !abs.is_file() {
        return None;
    }
    abs.strip_prefix(cwd).ok().map(slash_path)
}

fn re_strip_line_col(s: &str) -> &str {
    static LOC: OnceLock<Regex> = OnceLock::new();
    let r = re(&LOC, r"^(.+?)(?::\d+(?::\d+)?)?$");
    r.captures(s).and_then(|c| c.get(1)).map_or(s, |m| m.as_str())
}

fn resolve_relative_import(cwd: &Path, test_dir: &Path, spec: &str) -> Option<String> {
    let base = test_dir.join(spec);
    let candidates: Vec<PathBuf> = if base.extension().is_some() {
        vec![base]
    } else {
        vec![
            base.clone(),
            base.with_extension("ts"),
            base.with_extension("tsx"),
            base.with_extension("js"),
            base.with_extension("jsx"),
            base.with_extension("mjs"),
            base.with_extension("py"),
            base.with_extension("rs"),
            base.join("index.ts"),
            base.join("index.tsx"),
            base.join("index.js"),
            base.join("mod.rs"),
        ]
    };
    for cand in candidates {
        if cand.is_file()
            && let Ok(rel) = cand.strip_prefix(cwd)
        {
            let rel_s = slash_path(rel);
            if !is_test_path(&rel_s) && is_source_ext(&rel_s) {
                return Some(rel_s);
            }
        }
    }
    None
}

fn imports_from_test(cwd: &Path, test_rel: &str) -> Vec<String> {
    static JS_IMP: OnceLock<Regex> = OnceLock::new();
    static PY_IMP: OnceLock<Regex> = OnceLock::new();
    static RS_USE: OnceLock<Regex> = OnceLock::new();
    let test_abs = cwd.join(test_rel);
    let Some(test_dir) = test_abs.parent() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&test_abs) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(32_768)]);
    let head: String = text.lines().take(120).collect::<Vec<_>>().join("\n");
    let mut out = Vec::new();

    for cap in re(&JS_IMP, r#"(?:from|require\s*\(|import\s*\()\s*['"](\.\.?/[^'"]+)['"]"#).captures_iter(&head) {
        if let Some(rel) = resolve_relative_import(cwd, test_dir, &cap[1]) {
            out.push(rel);
        }
    }
    for cap in re(&PY_IMP, r"(?m)^\s*from\s+(\.+)([A-Za-z0-9_.]*)\s+import\b").captures_iter(&head) {
        let dots = cap[1].len();
        let mut base = test_dir.to_path_buf();
        for _ in 1..dots {
            base = base.parent().unwrap_or(cwd).to_path_buf();
        }
        let mod_path = cap[2].replace('.', "/");
        if !mod_path.is_empty()
            && let Some(rel) = resolve_relative_import(cwd, &base, &mod_path)
        {
            out.push(rel);
        }
    }
    // Rust crate-local or workspace crate imports: `use my_crate::mod_name::...;`
    for cap in re(&RS_USE, r"(?m)^\s*use\s+([A-Za-z0-9_]+)::([A-Za-z0-9_]+)").captures_iter(&head) {
        let krate = cap[1].replace('_', "-");
        let module = &cap[2];
        for cand in [
            cwd.join("crates").join(&krate).join("src").join(format!("{module}.rs")),
            cwd.join("crates").join(&krate).join("src").join(module).join("mod.rs"),
            cwd.join("src").join(format!("{module}.rs")),
            cwd.join("src").join(module).join("mod.rs"),
        ] {
            if cand.is_file()
                && let Ok(rel) = cand.strip_prefix(cwd)
            {
                let s = slash_path(rel);
                if !is_test_path(&s) {
                    out.push(s);
                }
            }
        }
    }
    out
}

fn stem_matches_for_test(test_rel: &str, project_files: &[String]) -> Vec<String> {
    let fname = test_rel.rsplit('/').next().unwrap_or(test_rel).to_lowercase();
    let stem = file_stem(&fname)
        .trim_end_matches(".test")
        .trim_end_matches(".spec")
        .trim_end_matches("_test")
        .trim_end_matches("_spec")
        .trim_end_matches("_tests")
        .trim_start_matches("test_")
        .trim_start_matches("core_")
        .trim_start_matches("net_");
    if stem.len() < 3 {
        return Vec::new();
    }
    let test_pkg = test_rel.split('/').take(2).collect::<Vec<_>>().join("/");
    let mut hits: Vec<(usize, String)> = project_files
        .iter()
        .filter(|p| !is_test_path(p) && is_source_ext(p))
        .filter_map(|p| {
            let p_name = p.rsplit('/').next().unwrap_or(p).to_lowercase();
            let p_stem = file_stem(&p_name);
            let stem_clean = p_stem.replace('_', "");
            let req_clean = stem.replace('_', "");
            if p_stem == stem || stem_clean == req_clean {
                let same_pkg = usize::from(!p.starts_with(&test_pkg));
                Some((same_pkg, p.clone()))
            } else {
                None
            }
        })
        .collect();
    hits.sort();
    hits.into_iter().take(2).map(|(_, p)| p).collect()
}

pub(crate) fn test_target_hints(cwd: &Path, command: &str, output: &str) -> Vec<String> {
    if !looks_like_test_run(command, output) {
        return Vec::new();
    }
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    let mut mentioned_tests = BTreeSet::new();
    let mut direct_sources = BTreeSet::new();
    for m in re(&TOKEN, r"([A-Za-z0-9_./\\-]+\.[A-Za-z0-9]{1,8}(?::\d+(?::\d+)?)?)").find_iter(output) {
        let Some(rel) = resolve_existing_rel(cwd, m.as_str()) else {
            continue;
        };
        if !is_source_ext(&rel) {
            continue;
        }
        if is_test_path(&rel) {
            mentioned_tests.insert(rel);
        } else {
            direct_sources.insert(rel);
        }
    }
    let mut ordered: Vec<String> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for test in &mentioned_tests {
        for imp in imports_from_test(cwd, test) {
            if seen.insert(imp.clone()) {
                ordered.push(imp);
            }
        }
    }
    for src in direct_sources {
        if seen.insert(src.clone()) {
            ordered.push(src);
        }
    }
    if ordered.is_empty() && !mentioned_tests.is_empty() {
        let files = walk_project_files(cwd, None, WALK_DEPTH, WALK_LIMIT, None);
        for test in &mentioned_tests {
            for m in stem_matches_for_test(test, &files) {
                if seen.insert(m.clone()) {
                    ordered.push(m);
                }
            }
        }
    }
    ordered.truncate(3);
    ordered
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.settings();
        let on = |k: &str| settings.get(k) != Some(&Value::Bool(false));
        let paths_on = on("paths");
        let unknown_on = on("unknownTools");
        let test_targets_on = on("testTargets");
        let cwd = api.cwd();
        let home = sasacode_home();
        let a = api.clone();

        api.on::<hook::ToolResult, _, _>(move |ev, _ctx| {
            let cwd = cwd.clone();
            let home = home.clone();
            let a = a.clone();
            async move {
                if !ev.result.is_error() {
                    return Ok(None);
                }
                let text = result_text(&ev.result);
                let mut hints: Vec<String> = Vec::new();

                if paths_on
                    && matches!(ev.call.name.as_str(), "read" | "edit")
                    && text.contains("File not found:")
                    && let Some(req) = ev.call.input.get("path").and_then(Value::as_str)
                {
                    let cwd = cwd.clone();
                    let req = req.to_string();
                    let matches = tokio::task::spawn_blocking(move || suggest_paths(&cwd, &req))
                        .await
                        .map_err(anyhow::Error::from)?;
                    if !matches.is_empty() {
                        hints.push(format!("[hint] Did you mean: {}", matches.join(", ")));
                    }
                }

                if unknown_on && !ev.ran && text.starts_with("Unknown tool \"") {
                    let called = text
                        .strip_prefix("Unknown tool \"")
                        .and_then(|rest| rest.split('"').next())
                        .unwrap_or(&ev.call.name)
                        .to_string();
                    let tools: Vec<String> = a.agent().tools().iter().map(|t| t.name().to_string()).collect();
                    let home = home.clone();
                    let cwd = cwd.clone();
                    if let Some(h) =
                        tokio::task::spawn_blocking(move || unknown_tool_hint(&called, &home, &cwd, &tools))
                            .await
                            .map_err(anyhow::Error::from)?
                    {
                        hints.push(h);
                    }
                }

                if test_targets_on
                    && ev.ran
                    && matches!(ev.call.name.as_str(), "bash" | "project_check")
                    && !text.contains("[hint] Likely source file(s)")
                {
                    let cmd = ev
                        .call
                        .input
                        .get("command")
                        .or_else(|| ev.call.input.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if looks_like_test_run(&cmd, &text) {
                        let cwd = cwd.clone();
                        let targets = tokio::task::spawn_blocking(move || test_target_hints(&cwd, &cmd, &text))
                            .await
                            .map_err(anyhow::Error::from)?;
                        if !targets.is_empty() {
                            hints.push(format!(
                                "[hint] Likely source file(s) under test: {}",
                                targets.join(", ")
                            ));
                        }
                    }
                }

                if hints.is_empty() {
                    return Ok(None);
                }
                let mut result = ev.result;
                result.content.push(UserContent::text(hints.join("\n")));
                Ok(Some(ToolResultResult { result: Some(result) }))
            }
        });

        Ok(())
    })
}
