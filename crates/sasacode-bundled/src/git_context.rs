//! git-context (`plugins/git-context/index.ts`): a bounded Git review context (status, diff
//! statistics and an optional patch) for working, staged and branch diffs. Off until
//! `plugins.enabled` names it.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use sasacode_ai::JsonObject;
use sasacode_plugin_api::{
    PluginApi, ToolContext, ToolDef, ToolKind, ToolResult, apply_child_env, validate::is_integer,
};

use crate::repair::js::{js_space, utf16_len, utf16_prefix};

const MAX_BUFFER: usize = 8 * 1024 * 1024;

/// Git receives argv, never shell text. External diff/textconv helpers are disabled.
async fn git(cwd: &Path, args: &[&str], signal: &CancellationToken) -> anyhow::Result<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["--no-pager", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(cwd);
    let extra: Vec<(String, String)> = [
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_NO_LAZY_FETCH", "1"),
        ("LC_ALL", "C"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    apply_child_env(&mut cmd, &extra);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let command = format!("git --no-pager -c core.fsmonitor=false {}", args.join(" "));
    let mut child = cmd.spawn()?;
    let (Some(mut out), Some(mut err)) = (child.stdout.take(), child.stderr.take()) else {
        anyhow::bail!("Command failed: {command}");
    };
    let run = async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let (mut out, mut err) = (
            (&mut out).take(MAX_BUFFER as u64 + 1),
            (&mut err).take(MAX_BUFFER as u64 + 1),
        );
        let (a, b) = tokio::join!(out.read_to_end(&mut stdout), err.read_to_end(&mut stderr));
        a?;
        b?;
        if stdout.len() > MAX_BUFFER || stderr.len() > MAX_BUFFER {
            anyhow::bail!("stdout maxBuffer length exceeded");
        }
        let status = child.wait().await?;
        let stderr = String::from_utf8_lossy(&stderr).into_owned();
        if !status.success() {
            anyhow::bail!("Command failed: {command}\n{stderr}");
        }
        Ok(String::from_utf8_lossy(&stdout).trim_end_matches(js_space).to_string())
    };
    tokio::select! {
        r = tokio::time::timeout(Duration::from_secs(15), run) => match r {
            Ok(r) => r,
            Err(_) => anyhow::bail!("Command failed: {command}\n(timed out)"),
        },
        () = signal.cancelled() => anyhow::bail!("The operation was aborted"),
    }
}

pub async fn collect_context(args: &JsonObject, ctx: &ToolContext) -> anyhow::Result<ToolResult> {
    let signal = &ctx.signal;
    if signal.is_cancelled() {
        anyhow::bail!("This operation was aborted");
    }
    let scope = match args.get("scope") {
        None | Some(Value::Null) => "working",
        Some(Value::String(s)) if ["working", "staged", "branch"].contains(&s.as_str()) => s.as_str(),
        Some(_) => anyhow::bail!("Unknown scope"),
    };
    let base = args.get("base").filter(|v| !v.is_null());
    if base.is_some() && scope != "branch" {
        anyhow::bail!("base requires scope=branch");
    }
    let limit = match args.get("maxChars").filter(|v| !v.is_null()) {
        None => 16_000.0,
        Some(v) if is_integer(v) => v.as_f64().unwrap_or(0.0),
        Some(_) => f64::NAN,
    };
    if !(1000.0..=60_000.0).contains(&limit) {
        anyhow::bail!("maxChars must be 1000..60000");
    }
    let limit = limit as usize;
    let cwd = ctx.cwd.as_path();
    let head = match git(cwd, &["rev-parse", "--verify", "HEAD"], signal).await {
        Ok(h) => Some(h),
        Err(e) if signal.is_cancelled() => return Err(e),
        Err(_) => None,
    };
    // Also verifies that this is a working tree, including repositories before their first commit.
    let status = git(
        cwd,
        &["status", "--short", "--branch", "--untracked-files=normal", "--", "."],
        signal,
    )
    .await?;
    let range: Vec<String>;
    let comparison: String;
    if scope == "branch" {
        let base = base.and_then(Value::as_str).unwrap_or("@{upstream}");
        // Resolve first so refs beginning with '-' cannot become options.
        let base_id = git(
            cwd,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{base}^{{commit}}"),
            ],
            signal,
        )
        .await?;
        let Some(head) = &head else {
            anyhow::bail!("Branch comparison requires a commit");
        };
        let merge_base = git(cwd, &["merge-base", head, &base_id], signal).await?;
        comparison = format!("merge-base({base}, HEAD) {merge_base} -> HEAD {head}; excludes uncommitted changes");
        range = vec![merge_base, head.clone()];
    } else if scope == "staged" {
        range = vec!["--cached".into()];
        comparison = "HEAD -> index (staged changes)".into();
    } else {
        // HEAD vs worktree includes staged + unstaged changes; without HEAD, all tracked files are staged.
        range = vec![head.clone().unwrap_or_else(|| "--cached".into())];
        comparison = "HEAD -> working tree (combined staged + unstaged changes)".into();
    }
    let diff = |options: &'static str| {
        let mut a: Vec<String> = [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--ignore-submodules=all",
            options,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        a.extend(range.iter().cloned());
        a.extend(["--".to_string(), ".".to_string()]);
        async move {
            let refs: Vec<&str> = a.iter().map(String::as_str).collect();
            git(cwd, &refs, signal).await
        }
    };
    let or = |s: String, empty: &str| if s.is_empty() { empty.to_string() } else { s };
    let mut sections = vec![
        format!(
            "Git context ({scope})\nComparison: {comparison}\nScope: current directory and descendants.\nUntracked contents and submodule changes are not included. This is a live view, not an atomic snapshot."
        ),
        format!("Status\n{}", or(status, "(clean)")),
        format!("Diff statistics\n{}", or(diff("--stat").await?, "(no changes)")),
    ];
    if args.get("patch") != Some(&Value::Bool(false)) {
        sections.push(format!("Patch\n{}", or(diff("--unified=3").await?, "(no changes)")));
    }
    let full = sections.join("\n\n");
    let truncated = utf16_len(&full) > limit;
    let note = "\n[Truncated. Request patch=false for an overview, raise maxChars, or inspect individual files.]";
    let text = if truncated {
        format!("{}{note}", utf16_prefix(&full, limit - utf16_len(note)))
    } else {
        full
    };
    let mut details = JsonObject::new();
    details.insert("scope".into(), json!(scope));
    details.insert("head".into(), json!(head));
    details.insert("truncated".into(), json!(truncated));
    Ok(ToolResult::text(text).with_details(details))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        api.register_tool(
            ToolDef::new(
                "git_context",
                "Read a bounded Git review context: status, diff statistics and optional patch. working combines staged and unstaged changes; staged shows the index; branch compares merge-base(base, HEAD) to HEAD, defaulting to upstream. Never fetches or changes Git state. Untracked contents and submodules are excluded.",
                json!({
                    "type": "object", "properties": {
                        "scope": { "type": "string", "enum": ["working", "staged", "branch"] },
                        "base": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "patch": { "type": "boolean" },
                        "maxChars": { "type": "integer", "minimum": 1000, "maximum": 60_000 },
                    }, "additionalProperties": false,
                }),
                ToolKind::Read,
                |args, ctx| async move { collect_context(&args, &ctx).await },
            )
            .concurrent(true)
            .paths(|_args, cwd| vec![cwd.to_string_lossy().into_owned()])
            .into_ref(),
        );
        Ok(())
    })
}
