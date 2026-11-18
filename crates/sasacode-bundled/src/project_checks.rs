//! project-checks (`plugins/project-checks/index.ts`): named project checks (test, lint,
//! typecheck, build) the model can list and run one at a time through bash, under bash's
//! permission rules. Off until `plugins.enabled` names it.
//!
//! `verifyOnFinish` (off unless set) runs the checks it names when the model ends a turn without
//! tool calls after writing files or after a check failed, and sends the end of a failing check's
//! output back so the run goes on (at most twice a run). Changes to test files only, or added
//! markers that skip tests, get one note. The failures it answers are in docs/benchmarks.md.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{Value, json};

use sasacode_ai::{JsonObject, UserContent, now_ms};
use sasacode_plugin_api::{
    Command, Lang, PluginApi, ShowTextOptions, TextFormat, Tool, ToolContext, ToolDef, ToolKind, ToolResult,
    ToolResultEvent, TurnEndResult, hook,
};
use sasacode_tools::BashTool;

use crate::repair::js::{js_trim, utf16_len};

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub command: String,
    /// Seconds.
    pub timeout: f64,
}

fn settings_schema() -> Value {
    let command = json!({ "type": "string", "minLength": 1, "maxLength": 8000, "pattern": "\\S" });
    json!({
        "type": "object", "additionalProperties": false, "properties": {
            "checks": { "type": "object", "additionalProperties": { "anyOf": [command, {
                "type": "object", "required": ["command"], "additionalProperties": false,
                "properties": { "command": command, "timeout": { "type": "number", "minimum": 1, "maximum": 3600 } },
            }] } },
            "verifyOnFinish": { "type": "array", "maxItems": 10, "items": { "type": "string" } },
        },
    })
}

/// `/^[a-z][a-z0-9-]{0,39}$/`.
pub(crate) fn valid_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 40
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

pub fn load_checks(cwd: &Path, settings: &Value) -> anyhow::Result<IndexMap<String, Check>> {
    let mut checks = IndexMap::new();
    let Some(raw_checks) = settings.get("checks") else {
        // Only known script names, and commands are frozen until the next plugin load.
        let text = match std::fs::read(cwd.join("package.json")) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(checks),
            Err(e) => return Err(e.into()),
        };
        let manifest: Value = serde_json::from_str(&text)?;
        let scripts = match manifest.get("scripts") {
            Some(Value::Null) | None => json!({}),
            Some(s) => s.clone(),
        };
        let managers: [(&str, &[&str]); 4] = [
            ("bun", &["bun.lock", "bun.lockb"]),
            ("pnpm", &["pnpm-lock.yaml"]),
            ("yarn", &["yarn.lock"]),
            ("npm", &["package-lock.json", "npm-shrinkwrap.json"]),
        ];
        let found: Vec<&str> = managers
            .iter()
            .filter(|(_, locks)| locks.iter().any(|lock| cwd.join(lock).exists()))
            .map(|(m, _)| *m)
            .collect();
        if found.len() > 1 {
            anyhow::bail!("project-checks: multiple package manager lockfiles; configure checks explicitly");
        }
        let manager = found.first().copied().unwrap_or("npm");
        for name in ["test", "lint", "typecheck", "check", "build"] {
            if scripts.get(name).is_some_and(Value::is_string) {
                checks.insert(
                    name.to_string(),
                    Check {
                        command: format!("{manager} run {name}"),
                        timeout: 120.0,
                    },
                );
            }
        }
        return Ok(checks);
    };
    let Value::Object(entries) = raw_checks else {
        anyhow::bail!("checks must be an object");
    };
    for (name, raw) in entries {
        if !valid_name(name) {
            anyhow::bail!("Invalid check name: {name}");
        }
        let value = match raw {
            Value::String(s) => json!({ "command": s }),
            v => v.clone(),
        };
        let Value::Object(value) = value else {
            anyhow::bail!("Invalid check: {name}");
        };
        let command = match value.get("command") {
            Some(Value::String(c)) if !js_trim(c).is_empty() && utf16_len(c) <= 8000 => c.clone(),
            _ => anyhow::bail!("Invalid command: {name}"),
        };
        let timeout = match value.get("timeout") {
            None => 120.0,
            Some(t) => match t.as_f64() {
                Some(t) if t.is_finite() && (1.0..=3600.0).contains(&t) => t,
                _ => anyhow::bail!("Invalid timeout: {name} (1..3600 seconds)"),
            },
        };
        checks.insert(name.clone(), Check { command, timeout });
    }
    Ok(checks)
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let cwd = api.cwd();
        let settings = api.define_settings(&settings_schema(), None)?;
        let checks = Arc::new(load_checks(&cwd, &settings)?);
        let finish = finish_checks(&settings, &checks)?;
        let ja = api.ui().lang() == Lang::Ja;
        let listing = {
            let checks = checks.clone();
            Arc::new(move || {
                if checks.is_empty() {
                    return if ja {
                        "チェック未設定。plugins.settings.project-checks.checks にコマンドを設定してください。"
                            .to_string()
                    } else {
                        "No checks. Configure plugins.settings.project-checks.checks.".to_string()
                    };
                }
                checks
                    .iter()
                    .map(|(name, c)| {
                        format!(
                            "{name}: {} (timeout {}s)",
                            c.command,
                            sasacode_ai::json::js_f64_string(c.timeout)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        };
        let (a, l) = (api.clone(), listing.clone());
        api.register_command(Command::new(
            "checks",
            if ja {
                "利用できるプロジェクト検査を表示"
            } else {
                "List available project checks"
            },
            move |_c| {
                let (a, l) = (a.clone(), l.clone());
                async move {
                    a.ui()
                        .show_text(ShowTextOptions {
                            title: "Project checks".into(),
                            text: l(),
                            format: TextFormat::Text,
                        })
                        .await
                }
            },
        ));
        let (l, manifest) = (listing.clone(), cwd.join("package.json").to_string_lossy().into_owned());
        api.register_tool(
            ToolDef::new(
                "project_checks",
                "List configured project checks and their exact commands without executing them.",
                json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                ToolKind::Read,
                move |_args, _ctx| {
                    let l = l.clone();
                    async move { Ok(ToolResult::text(l())) }
                },
            )
            .concurrent(true)
            .paths(move |_a, _c| vec![manifest.clone()])
            .into_ref(),
        );
        if checks.is_empty() {
            return Ok(());
        }
        let name_of = |a: &JsonObject| a.get("name").and_then(Value::as_str).unwrap_or_default().to_string();
        let (c1, c2, c3) = (checks.clone(), checks.clone(), checks.clone());
        api.register_tool(
            ToolDef::new(
                "project_check",
                "Run ONE named project check (test/lint/typecheck/build). Returns PASS/FAIL, exit code, duration and bounded log; may modify files as its command does. Does not imply other checks passed. Inspect project_checks first.",
                json!({
                    "type": "object",
                    "properties": { "name": { "type": "string", "enum": checks.keys().collect::<Vec<_>>() } },
                    "required": ["name"], "additionalProperties": false,
                }),
                ToolKind::Exec,
                move |args, ctx| {
                    let checks = c1.clone();
                    async move {
                        let Some(call_tool) = ctx.call_tool.clone() else {
                            anyhow::bail!("project-checks requires a host tool context with callTool (API 1.9+)");
                        };
                        let name = name_of(&args);
                        let Some(check) = checks.get(&name).cloned() else {
                            anyhow::bail!("Unknown check: {name}");
                        };
                        let started = now_ms();
                        let mut input = JsonObject::new();
                        input.insert("command".into(), json!(check.command));
                        input.insert("timeout".into(), sasacode_ai::json::js_number(check.timeout));
                        let result = call_tool("bash".into(), input).await;
                        let duration_ms = now_ms() - started;
                        let mut content = vec![UserContent::text(format!(
                            "[{name}: {}; {duration_ms}ms including approval]",
                            if result.is_error() { "FAIL" } else { "PASS" }
                        ))];
                        content.extend(result.content.iter().cloned());
                        let mut details = result.details.clone().unwrap_or_default();
                        details.insert("name".into(), json!(name));
                        details.insert("command".into(), json!(check.command));
                        details.insert("durationMs".into(), json!(duration_ms));
                        Ok(ToolResult { content, is_error: result.is_error, details: Some(details) })
                    }
                },
            )
            .permissions_as("bash")
            .concurrent(false)
            .match_target(move |a| c2.get(&name_of(a)).map(|c| c.command.clone()).unwrap_or_default())
            .summary(move |a| {
                let name = name_of(a);
                format!("{name}: {}", c3.get(&name).map(|c| c.command.as_str()).unwrap_or_default())
            })
            .into_ref(),
        );
        if !finish.is_empty() {
            verify_on_finish(&api, cwd, checks, finish);
        }
        Ok(())
    })
}

// ── verifyOnFinish ───────────────────────────────────────────────────

/// Reports of a failing check sent back in one run; after these the run may end as the model says.
const MAX_REPORTS: u32 = 2;
/// Markers that skip tests or narrow a run to some. Each starts a word: `submit.skip(` is no
/// `it.skip(`, `process.exit(` no `xit(`, and an iterator's `.skip(1)` is none of them.
const SKIP_MARKERS: [&str; 11] = [
    "it.skip(",
    "test.skip(",
    "describe.skip(",
    "it.only(",
    "test.only(",
    "describe.only(",
    "xit(",
    "xdescribe(",
    "#[ignore]",
    "@pytest.mark.skip",
    "@unittest.skip",
];

/// The checks `verifyOnFinish` names, in its order. They run with no permission prompt, so only
/// checks written in `checks`: never scripts found in a package.json the user may not have read.
pub fn finish_checks(settings: &Value, checks: &IndexMap<String, Check>) -> anyhow::Result<Vec<(String, Check)>> {
    let names: Vec<&str> = match settings.get("verifyOnFinish") {
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ => return Ok(Vec::new()),
    };
    if !names.is_empty() && settings.get("checks").is_none() {
        anyhow::bail!(
            "project-checks: verifyOnFinish runs checks without asking, so it names only checks written in `checks`, not scripts found in package.json"
        );
    }
    names
        .into_iter()
        .map(|name| match checks.get(name) {
            Some(check) => Ok((name.to_string(), check.clone())),
            None => Err(anyhow::anyhow!(
                "project-checks: verifyOnFinish names an unknown check: {name}"
            )),
        })
        .collect()
}

/// What one run (one user prompt) has done that the end of it should answer.
#[derive(Default)]
struct RunState {
    /// Files write or edit changed, relative to the project when inside it.
    changed: BTreeSet<String>,
    /// Files changed since every finish check last passed.
    unverified: bool,
    /// Finish checks that passed since the last change.
    passed: BTreeSet<String>,
    /// Checks whose latest run failed: project_check, bash running a check's command, or ours.
    failing: BTreeSet<String>,
    /// Skip markers the changes added.
    skips: BTreeSet<&'static str>,
    reports: u32,
    noted: bool,
}

fn lock(state: &Mutex<RunState>) -> MutexGuard<'_, RunState> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl RunState {
    fn check_ran(&mut self, name: &str, failed: bool, finish: &[(String, Check)]) {
        if failed {
            self.failing.insert(name.to_string());
            self.passed.remove(name);
        } else {
            self.failing.remove(name);
            self.passed.insert(name.to_string());
            if finish.iter().all(|(n, _)| self.passed.contains(n)) {
                self.unverified = false;
            }
        }
    }

    /// The note for a run that changed only tests, or added skip markers; once a run.
    fn note(&mut self) -> Option<String> {
        let only_tests = !self.changed.is_empty() && self.changed.iter().all(|p| is_test_file(p));
        if self.noted || (!only_tests && self.skips.is_empty()) {
            return None;
        }
        self.noted = true;
        let mut parts = Vec::new();
        if only_tests {
            let files: Vec<&str> = self.changed.iter().map(String::as_str).collect();
            parts.push(format!(
                "only test files changed in this run ({}). If the task was to fix the code, the fix belongs in the code under test; if changing the tests was the point, say so.",
                files.join(", ")
            ));
        }
        if !self.skips.is_empty() {
            let found: Vec<String> = self.skips.iter().map(|m| format!("`{m}`")).collect();
            parts.push(format!(
                "this run added markers that skip tests ({}). Passing checks say little while tests are skipped: remove them, or say why they are there.",
                found.join(", ")
            ));
        }
        Some(format!("[project-checks] Note: {}", parts.join(" Also, ")))
    }
}

/// Whether a bash command runs `check` as one of its commands: where a command starts (the start,
/// or after `;`, `&`, `|`, `(` or a newline), and not run on into a longer word. `cd app && bun test
/// 2>&1` and `bun test src/a.test.ts` run `bun test`; `grep true src` does not run `true`.
pub fn runs_check(command: &str, check: &str) -> bool {
    command.match_indices(check).any(|(i, _)| {
        let before = command[..i].trim_end_matches([' ', '\t']);
        let after = &command[i + check.len()..];
        (before.is_empty() || before.ends_with([';', '&', '|', '(', '\n']))
            && !after.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '-')
    })
}

/// A test file by its path in the project: under a test directory, or named as one.
fn is_test_file(path: &str) -> bool {
    let rel = path.replace('\\', "/");
    let name = rel.rsplit('/').next().unwrap_or_default();
    rel.split('/')
        .any(|part| matches!(part, "test" | "tests" | "__tests__" | "spec" | "specs"))
        || [".test.", ".spec.", "_test.", "_spec.", "_tests."]
            .iter()
            .any(|m| name.contains(m))
        || name.starts_with("test_")
}

/// Each skip marker in `line`, once per place it starts a word.
fn markers(line: &str) -> impl Iterator<Item = &'static str> + '_ {
    SKIP_MARKERS.into_iter().flat_map(move |m| {
        line.match_indices(m)
            .filter(|(i, _)| !line[..*i].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '.'))
            .map(move |_| m)
    })
}

/// A successful write or edit: the file it changed, and the skip markers it added (in the diff's
/// added lines but not its removed ones; a new file's whole content).
fn record_change(run: &mut RunState, cwd: &Path, ev: &ToolResultEvent) {
    let details = ev.result.details.as_ref();
    if details.and_then(|d| d.get("changed")).and_then(Value::as_bool) == Some(false) {
        return;
    }
    let arg = |k: &str| ev.call.input.get(k).and_then(Value::as_str).unwrap_or_default();
    let path = details
        .and_then(|d| d.get("path"))
        .and_then(Value::as_str)
        .unwrap_or(arg("path"));
    let rel = Path::new(path)
        .strip_prefix(cwd)
        .map_or(path.to_string(), |p| p.to_string_lossy().into_owned());
    run.changed.insert(rel);
    run.unverified = true;
    run.passed.clear();
    let (mut added, mut removed) = (Vec::new(), Vec::new());
    match details.and_then(|d| d.get("diff")).and_then(Value::as_str) {
        Some(diff) => {
            for line in diff.lines().skip(1) {
                if let Some(l) = line.strip_prefix('+') {
                    added.extend(markers(l));
                } else if let Some(l) = line.strip_prefix('-') {
                    removed.extend(markers(l));
                }
            }
        }
        None => arg("content").lines().for_each(|l| added.extend(markers(l))),
    }
    for m in SKIP_MARKERS {
        if added.iter().filter(|&&a| a == m).count() > removed.iter().filter(|&&r| r == m).count() {
            run.skips.insert(m);
        }
    }
}

/// The last lines of a check's output: at most 40 lines and 3,000 bytes.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let out = lines[lines.len().saturating_sub(40)..].join("\n");
    if out.len() <= 3000 {
        return out;
    }
    let mut cut = out.len() - 3000;
    while !out.is_char_boundary(cut) {
        cut += 1;
    }
    format!("…{}", &out[cut..])
}

/// Run each finish check in order through bash's own runner (its environment without sasacode's
/// secrets, its timeout, its process-group kill, its bounded output), stopping at the first that
/// fails. The report for the model of the one that failed.
async fn run_finish_checks(cwd: &Path, finish: &[(String, Check)], state: &Mutex<RunState>) -> Option<String> {
    for (name, check) in finish {
        let mut input = JsonObject::new();
        input.insert("command".into(), json!(check.command));
        input.insert("timeout".into(), sasacode_ai::json::js_number(check.timeout));
        let result = BashTool
            .execute(input, ToolContext::new(cwd))
            .await
            .unwrap_or_else(|e| ToolResult::error(format!("{e:#}")));
        let failed = result.is_error();
        lock(state).check_ran(name, failed, finish);
        if failed {
            let output: String = result.content.iter().filter_map(UserContent::as_text).collect();
            return Some(format!(
                "[project-checks] The run was about to end, so the check `{name}` ({}) ran, and it failed. The end of its output:\n{}\nKeep working until it passes. If it cannot pass, say so plainly instead of reporting the work as done.",
                check.command,
                tail(&output)
            ));
        }
    }
    None
}

fn verify_on_finish(api: &PluginApi, cwd: PathBuf, checks: Arc<IndexMap<String, Check>>, finish: Vec<(String, Check)>) {
    let state: Arc<Mutex<RunState>> = Arc::default();
    let (cwd, finish) = (Arc::new(cwd), Arc::new(finish));

    let st = state.clone();
    api.on::<hook::UserPrompt, _, _>(move |_e, ctx| {
        if ctx.agent.is_none() {
            *lock(&st) = RunState::default();
        }
        async { Ok(None) }
    });

    // Subagents' calls count too: their changes are this run's.
    let (st, dir, fin) = (state.clone(), cwd.clone(), finish.clone());
    api.on::<hook::ToolResult, _, _>(move |ev, _ctx| {
        if ev.ran {
            let mut run = lock(&st);
            let failed = ev.result.is_error();
            let arg = |k: &str| ev.call.input.get(k).and_then(Value::as_str).unwrap_or_default();
            let check = match ev.call.name.as_str() {
                "project_check" => checks.get_key_value(arg("name")).map(|(n, _)| n.clone()),
                "bash" => checks
                    .iter()
                    .find(|(_, c)| runs_check(arg("command"), &c.command))
                    .map(|(n, _)| n.clone()),
                _ => None,
            };
            if let Some(name) = check {
                run.check_ran(&name, failed, &fin);
            } else if !failed && matches!(ev.call.name.as_str(), "write" | "edit") {
                record_change(&mut run, &dir, &ev);
            }
        }
        async { Ok(None) }
    });

    let a = api.clone();
    api.on::<hook::TurnEnd, _, _>(move |ev, ctx| {
        let (st, cwd, finish, a) = (state.clone(), cwd.clone(), finish.clone(), a.clone());
        async move {
            if ctx.agent.is_some() || ev.message.tool_calls().next().is_some() {
                return Ok(None);
            }
            let due = {
                let run = lock(&st);
                (run.unverified || !run.failing.is_empty()) && run.reports < MAX_REPORTS
            };
            let mut report = None;
            if due {
                let names: Vec<&str> = finish.iter().map(|(n, _)| n.as_str()).collect();
                a.ui()
                    .set_status("verify", Some(format!("checking before the end: {}", names.join(", "))));
                report = run_finish_checks(&cwd, &finish, &st).await;
                a.ui().set_status("verify", None);
                let mut run = lock(&st);
                if report.is_some() {
                    run.reports += 1;
                } else {
                    // Every finish check passed: an earlier failure of any check is answered.
                    run.failing.clear();
                }
            }
            let note = lock(&st).note();
            let inject: Vec<String> = [report, note].into_iter().flatten().collect();
            Ok((!inject.is_empty()).then(|| TurnEndResult {
                inject: Some(inject.join("\n\n")),
                stop: None,
            }))
        }
    });
}
