//! plugins/project-checks/index.test.ts.
//!
//! The last TS test set SASACODE_HOME and a variable of its own in the process environment. Setting
//! one needs `unsafe` in edition 2024, so the hidden variable is CARGO_PKG_NAME (which cargo sets
//! for the test run), and the part about long output saved under SASACODE_HOME is left out: it
//! would write to the real ~/.sasacode.

mod support_core;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_agent::permission::{PermissionCheck, PermissionPolicy};
use sasacode_bundled::project_checks::{self, Check, load_checks};
use sasacode_plugin_api::{
    CommandContext, Decision, PermissionMode, PermissionRules, Tool, ToolContext, ToolRef, ToolResult,
    hide_from_children,
};
use sasacode_tools::BashTool;
use support_core::*;

fn context(root: &Path, signal: CancellationToken) -> ToolContext {
    let mut c = ToolContext::new(root);
    c.signal = signal;
    c
}

async fn bash(command: &str, timeout: f64, ctx: ToolContext) -> ToolResult {
    BashTool
        .execute(obj(json!({ "command": command, "timeout": timeout })), ctx)
        .await
        .unwrap()
}

fn content(r: &ToolResult) -> String {
    serde_json::to_string(&r.content).unwrap()
}

fn detail(r: &ToolResult, key: &str) -> Value {
    r.details
        .as_ref()
        .and_then(|d| d.get(key))
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn detects_script_names_and_package_manager_rejects_ambiguous_lockfiles() {
    let root = temp_dir("project-checks-");
    let cwd = tempfile::Builder::new()
        .prefix("auto-")
        .tempdir_in(root.path())
        .unwrap();
    let cwd = cwd.path();
    std::fs::write(
        cwd.join("package.json"),
        json!({ "scripts": { "test": "bun test", "lint": "lint", "deploy": "publish" } }).to_string(),
    )
    .unwrap();
    std::fs::write(cwd.join("bun.lock"), "").unwrap();
    let checks: Vec<(String, Check)> = load_checks(cwd, &json!({})).unwrap().into_iter().collect();
    assert_eq!(
        checks,
        vec![
            (
                "test".to_string(),
                Check {
                    command: "bun run test".into(),
                    timeout: 120.0
                }
            ),
            (
                "lint".to_string(),
                Check {
                    command: "bun run lint".into(),
                    timeout: 120.0
                }
            ),
        ]
    );
    std::fs::write(cwd.join("pnpm-lock.yaml"), "").unwrap();
    let e = load_checks(cwd, &json!({})).unwrap_err();
    assert!(e.to_string().contains("multiple"), "{e}");
    assert_eq!(load_checks(cwd, &json!({ "checks": {} })).unwrap().len(), 0);
}

#[test]
fn validates_explicit_commands_and_timeouts() {
    let root = temp_dir("project-checks-");
    let checks = load_checks(root.path(), &json!({ "checks": { "types": "bun run typecheck" } })).unwrap();
    assert_eq!(checks["types"].timeout, 120.0);
    for checks in [
        json!({ "bad": { "command": "true", "timeout": -1 } }),
        json!({ "bad": "" }),
        json!({ "bad name": "true" }),
        json!([]),
    ] {
        assert!(
            load_checks(root.path(), &json!({ "checks": checks })).is_err(),
            "{checks}"
        );
    }
}

#[tokio::test]
async fn host_uses_real_shell_command_for_bash_permission_rules() {
    let root = temp_dir("project-checks-");
    let cwd = path_str(root.path());
    let h = load_plugin(
        "project-checks",
        project_checks::plugin,
        &cwd,
        json!({ "checks": { "test": "printf tested" } }),
    )
    .await;
    let tool = h.tool("project_check");
    let check = |tool: &ToolRef| PermissionCheck {
        tool: Arc::clone(tool),
        args: obj(json!({ "name": "test" })),
        cwd: cwd.clone(),
    };
    let policy = PermissionPolicy::new(
        PermissionMode::Auto,
        &PermissionRules {
            deny: vec!["bash(printf *)".into()],
            ..PermissionRules::default()
        },
    )
    .unwrap();
    assert_eq!(policy.check(&check(&tool)).decision, Decision::Deny);
    assert_eq!(h.agent.permissions().check(&check(&tool)).decision, Decision::Ask);
    assert_eq!(
        tool.match_target(&obj(json!({ "name": "test" }))).as_deref(),
        Some("printf tested")
    );
    let checks = h
        .host
        .commands()
        .into_iter()
        .find(|c| c.command.name == "checks")
        .unwrap();
    (checks.command.run)(CommandContext { args: String::new() })
        .await
        .unwrap();
    let notices = h.notices.lock().unwrap().join(",");
    assert!(notices.contains("printf tested"), "{notices}");
    h.agent.set_tool(Arc::new(BashTool));
    h.agent.permissions().set_mode(PermissionMode::Auto);
    let result = h.invoke("project_check", json!({ "name": "test" })).await;
    assert_eq!(result.is_error, Some(false));
    assert!(content(&result).contains("tested"), "{}", content(&result));
}

#[tokio::test]
async fn nonzero_and_zero_exits_report_accurate_results() {
    let root = temp_dir("project-checks-");
    let failed = bash(
        "printf failure >&2; exit 7",
        2.0,
        context(root.path(), CancellationToken::new()),
    )
    .await;
    assert_eq!(failed.is_error, Some(true));
    assert_eq!(detail(&failed, "exitCode"), json!(7));
    assert!(content(&failed).contains("failure"));
    assert_eq!(
        bash("true", 2.0, context(root.path(), CancellationToken::new()))
            .await
            .is_error,
        Some(false)
    );
}

#[tokio::test]
async fn cancellation_before_execution_does_not_spawn_and_in_flight_cancellation_fails() {
    let root = temp_dir("project-checks-");
    let marker = root.path().join("should-not-exist");
    let aborted = CancellationToken::new();
    aborted.cancel();
    let r = bash(
        &format!("touch '{}'", path_str(&marker)),
        2.0,
        context(root.path(), aborted),
    )
    .await;
    assert_eq!(r.is_error, Some(true));
    assert!(!marker.exists());
    let controller = CancellationToken::new();
    let c = controller.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        c.cancel();
    });
    let result = bash("sleep 10", 5.0, context(root.path(), controller)).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(detail(&result, "killedBy"), json!("abort"));
}

#[tokio::test]
async fn timeout_kills_descendants_even_when_the_shell_catches_term_and_exits_zero() {
    let root = temp_dir("project-checks-");
    let marker = root.path().join("descendant-survived");
    let command = format!(
        "trap 'exit 0' TERM; (trap '' TERM; sleep 1; touch '{}') & wait",
        path_str(&marker)
    );
    let result = bash(&command, 0.05, context(root.path(), CancellationToken::new())).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(detail(&result, "killedBy"), json!("timeout"));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert!(!marker.exists());
}

#[tokio::test]
async fn host_only_credentials_are_removed() {
    let root = temp_dir("project-checks-");
    // Set by cargo for this run; hidden from children from here on.
    let key = "CARGO_PKG_NAME";
    let value = std::env::var(key).unwrap();
    assert_eq!(value, "sasacode-bundled");
    hide_from_children([key]);
    let hidden = bash(
        &format!("printf '%s' \"${key}\""),
        2.0,
        context(root.path(), CancellationToken::new()),
    )
    .await;
    assert!(!content(&hidden).contains(&value), "{}", content(&hidden));
}
