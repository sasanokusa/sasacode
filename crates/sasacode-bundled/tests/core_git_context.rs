//! plugins/git-context/index.test.ts. Each test makes its own temporary repositories; the set-up git
//! commands read neither the system nor the user's global git config.

mod support_core;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use sasacode_agent::permission::PermissionCheck;
use sasacode_bundled::git_context::{self, collect_context};
use sasacode_plugin_api::{Decision, ToolContext, ToolResult};
use support_core::*;

struct Fixture {
    cwd: PathBuf,
    ctx: ToolContext,
}

impl Fixture {
    fn git(&self, args: &[&str]) {
        git(&self.cwd, args);
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.cwd.join(name), text).unwrap();
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture(root: &Path, commit: bool) -> Fixture {
    let cwd = tempfile::Builder::new()
        .prefix("repo-")
        .tempdir_in(root)
        .unwrap()
        .keep();
    git(&cwd, &["init", "-b", "main"]);
    git(&cwd, &["config", "user.name", "Test"]);
    git(&cwd, &["config", "user.email", "test@example.invalid"]);
    std::fs::write(cwd.join("file.txt"), "original\n").unwrap();
    git(&cwd, &["add", "."]);
    if commit {
        git(&cwd, &["-c", "commit.gpgsign=false", "commit", "-m", "initial"]);
    }
    let ctx = ToolContext::new(cwd.clone());
    Fixture { cwd, ctx }
}

/// `JSON.stringify(r.content)`.
fn body(r: &ToolResult) -> String {
    serde_json::to_string(&r.content).unwrap()
}

async fn context(args: Value, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
    collect_context(&obj(args), ctx).await
}

#[tokio::test]
async fn working_combines_staged_and_unstaged_staged_stays_at_the_index() {
    let root = temp_dir("git-context-");
    let f = fixture(root.path(), true);
    f.write("file.txt", "staged\n");
    f.git(&["add", "."]);
    f.write("file.txt", "unstaged\n");
    f.write("untracked.txt", "UNTRACKED_SECRET_CONTENT\n");
    let working = body(&context(json!({}), &f.ctx).await.unwrap());
    assert!(working.contains("+unstaged"), "{working}");
    assert!(working.contains("untracked.txt"));
    assert!(!working.contains("UNTRACKED_SECRET_CONTENT"));
    assert!(body(&context(json!({ "scope": "staged" }), &f.ctx).await.unwrap()).contains("+staged"));
    assert!(!body(&context(json!({ "patch": false }), &f.ctx).await.unwrap()).contains("+unstaged"));
}

#[tokio::test]
async fn branch_uses_merge_base_and_excludes_uncommitted_edits() {
    let root = temp_dir("git-context-");
    let f = fixture(root.path(), true);
    f.git(&["checkout", "-b", "feature"]);
    f.write("file.txt", "feature\n");
    f.git(&["add", "."]);
    f.git(&["-c", "commit.gpgsign=false", "commit", "-m", "feature"]);
    f.write("file.txt", "local-only\n");
    let output = body(
        &context(json!({ "scope": "branch", "base": "main" }), &f.ctx)
            .await
            .unwrap(),
    );
    assert!(output.contains("+feature"), "{output}");
    assert!(!output.contains("+local-only"));
    assert!(
        context(json!({ "scope": "branch", "base": "--output=evil" }), &f.ctx)
            .await
            .is_err()
    );
    assert!(context(json!({ "scope": "branch" }), &f.ctx).await.is_err());
}

#[tokio::test]
async fn works_before_first_commit_and_limits_scope_to_cwd_descendants() {
    let root = temp_dir("git-context-");
    let first = fixture(root.path(), false);
    assert!(body(&context(json!({}), &first.ctx).await.unwrap()).contains("+original"));
    let f = fixture(root.path(), true);
    std::fs::create_dir(f.cwd.join("sub")).unwrap();
    f.write("sub/inside.txt", "old\n");
    f.git(&["add", "."]);
    f.git(&["-c", "commit.gpgsign=false", "commit", "-m", "sub"]);
    f.write("file.txt", "OUTSIDE_CONTENT\n");
    f.write("sub/inside.txt", "INSIDE_CONTENT\n");
    let output = body(&context(json!({}), &ToolContext::new(f.cwd.join("sub"))).await.unwrap());
    assert!(output.contains("INSIDE_CONTENT"), "{output}");
    assert!(!output.contains("OUTSIDE_CONTENT"));
}

#[tokio::test]
async fn truncates_explicitly_disables_external_diff_and_respects_cancellation() {
    let root = temp_dir("git-context-");
    let f = fixture(root.path(), true);
    f.git(&["config", "diff.external", "false"]);
    f.write("file.txt", &"long change\n".repeat(1000));
    let result = context(json!({ "maxChars": 1000 }), &f.ctx).await.unwrap();
    assert_eq!(result.details.as_ref().unwrap()["truncated"], true);
    let text = result.content[0].as_text().unwrap();
    assert!(text.encode_utf16().count() <= 1000);
    assert!(body(&result).contains("Truncated"));
    let mut aborted = ToolContext::new(f.cwd.clone());
    aborted.signal = CancellationToken::new();
    aborted.signal.cancel();
    assert!(context(json!({}), &aborted).await.is_err());
}

#[tokio::test]
async fn loads_through_the_host_as_a_read_tool_with_cwd_permission_scope() {
    let root = temp_dir("git-context-");
    let f = fixture(root.path(), true);
    let cwd = path_str(&f.cwd);
    let h = load_plugin("git-context", git_context::plugin, &cwd, json!({})).await;
    let tool = h.tool("git_context");
    let verdict = h.agent.permissions().check(&PermissionCheck {
        tool: Arc::clone(&tool),
        args: obj(json!({})),
        cwd: cwd.clone(),
    });
    assert_eq!(verdict.decision, Decision::Allow);
    let r = tool
        .execute(obj(json!({})), ToolContext::new(f.cwd.clone()))
        .await
        .unwrap();
    assert!(body(&r).contains("Git context"));
}
