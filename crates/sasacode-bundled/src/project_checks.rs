//! project-checks (`plugins/project-checks/index.ts`): named project checks (test, lint,
//! typecheck, build) the model can list and run one at a time through bash, under bash's
//! permission rules. Off until `plugins.enabled` names it.

use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{Value, json};

use sasacode_ai::{JsonObject, UserContent, now_ms};
use sasacode_plugin_api::{Command, Lang, PluginApi, ShowTextOptions, TextFormat, ToolDef, ToolKind, ToolResult};

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
        let checks = Arc::new(load_checks(&cwd, &api.define_settings(&settings_schema(), None)?)?);
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
        Ok(())
    })
}
