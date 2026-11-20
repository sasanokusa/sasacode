//! Integration tests for the 9 small-model harness features:
//! search-tools, error-hints, action-nudge, syntax-check, context-slim,
//! thinking-budget, hashline-edit, clean-retry, and slim-profile.

mod support_core;

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions, ApprovalAnswer, ApprovalDecision};
use sasacode_agent::permission::{PermissionCheck, PermissionPolicy};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::SessionFile;
use sasacode_ai::{
    AssistantContent, LocalServer, Message, ModelInfo, ThinkingContent, ToolResultMessage, UserContent, register_api,
};
use sasacode_bundled::clean_retry::looks_like_check_command;
use sasacode_bundled::hashline_edit::line_hash;
use sasacode_bundled::syntax_check::check_rust_syntax;
use sasacode_plugin_api::{Decision, Lang, PermissionMode, PermissionRules, ToolDef, ToolKind, ToolResult, hook};
use sasacode_testing::{ReplayOptions, ReplayProvider};
use support_core::*;

fn think(t: &str) -> AssistantContent {
    AssistantContent::Thinking(ThinkingContent {
        thinking: t.into(),
        signature: None,
        redacted: None,
        reasoning_field: None,
    })
}

fn tool_msg(id: &str, name: &str, text: &str, is_error: bool) -> Message {
    Message::Tool(ToolResultMessage {
        tool_call_id: id.into(),
        tool_name: name.into(),
        content: vec![UserContent::text(text)],
        is_error,
        timestamp: 100,
    })
}

fn settings_map(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
}

fn opts(cwd: &str, pairs: &[(&str, Value)]) -> SetupOpts {
    SetupOpts {
        cwd: Some(cwd.into()),
        settings: settings_map(pairs),
        ..SetupOpts::default()
    }
}

fn set_local_model(s: &Setup) {
    let mut m = s.agent.model();
    m.server = Some(LocalServer::Ollama);
    s.agent.set_model(m).unwrap();
}

fn set_model_with(s: &Setup, id: &str, server: Option<LocalServer>, base_url: Option<&str>) {
    let mut m = s.agent.model();
    m.id = id.into();
    m.server = server;
    m.base_url = base_url.map(str::to_string);
    s.agent.set_model(m).unwrap();
}

#[tokio::test]
async fn search_tools_find_files_and_search_text_respect_gitignore_and_caps() {
    let dir = temp_dir("sasacode-search-tools-");
    let root = dir.path();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    std::fs::create_dir_all(root.join("src/nested")).unwrap();
    std::fs::create_dir_all(root.join("ignored_dir")).unwrap();
    std::fs::write(root.join(".gitignore"), "ignored_dir/\n*.log\n").unwrap();
    std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(root.join("node_modules/pkg/index.js"), "needle_here\n").unwrap();
    std::fs::write(root.join("ignored_dir/secret.ts"), "needle_here\n").unwrap();
    std::fs::write(root.join("debug.log"), "needle_here\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {\n    let needle_here = 1;\n}\n").unwrap();
    std::fs::write(
        root.join("src/nested/util.ts"),
        "export const needle_here = 2;\nexport const OTHER = 3;\n",
    )
    .unwrap();
    std::fs::write(root.join("src/bin.dat"), b"\x00\x01\x02needle_here").unwrap();

    let l = load_plugin("search-tools", bundled("search-tools"), &path_str(root), json!({})).await;

    // find_files skips .git, node_modules, .gitignore entries and matches globs
    let r = l.invoke("find_files", json!({})).await;
    let t = text_of(&r);
    let lines: Vec<&str> = t.lines().collect();
    assert!(
        lines.contains(&"src/main.rs") && lines.contains(&"src/nested/util.ts"),
        "{t}"
    );
    assert!(
        !lines.iter().any(|p| p.contains("node_modules")
            || p.contains("ignored_dir")
            || p.contains("debug.log")
            || p.starts_with(".git/")),
        "{t}"
    );

    let r = l.invoke("find_files", json!({"pattern": "*.ts"})).await;
    assert_eq!(text_of(&r), "src/nested/util.ts");

    let r = l.invoke("find_files", json!({"max_results": 1})).await;
    assert!(text_of(&r).contains("[Showing 1 of "), "{}", text_of(&r));

    // search_text literal, glob filter, binary skip, regex + ignore_case
    let r = l.invoke("search_text", json!({"query": "needle_here"})).await;
    let t = text_of(&r);
    assert!(t.contains("src/main.rs:2: let needle_here = 1;"), "{t}");
    assert!(t.contains("src/nested/util.ts:1: export const needle_here = 2;"), "{t}");
    assert!(
        !t.contains("bin.dat") && !t.contains("node_modules") && !t.contains("ignored_dir"),
        "{t}"
    );

    let r = l
        .invoke(
            "search_text",
            json!({"query": "other", "ignore_case": true, "glob": "*.ts"}),
        )
        .await;
    assert!(
        text_of(&r).contains("src/nested/util.ts:2: export const OTHER = 3;"),
        "{}",
        text_of(&r)
    );

    let r = l
        .invoke("search_text", json!({"query": "let\\s+needle_\\w+", "regex": true}))
        .await;
    assert!(text_of(&r).contains("src/main.rs:2:"), "{}", text_of(&r));

    // bad regex is a clean tool error
    let r = l.invoke("search_text", json!({"query": "[", "regex": true})).await;
    assert!(r.is_error());
}

#[tokio::test]
async fn search_tools_enforce_read_permissions_and_exclude_secret_files() {
    let dir = temp_dir("sasacode-search-perms-");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::create_dir_all(root.join("certs")).unwrap();
    std::fs::create_dir_all(root.join("ssh")).unwrap();
    std::fs::write(root.join(".env"), "SECRET_TOKEN=root_secret\n").unwrap();
    std::fs::write(root.join(".env.local"), "SECRET_TOKEN=local_secret\n").unwrap();
    std::fs::write(root.join(".env.example"), "SECRET_TOKEN=example_placeholder\n").unwrap();
    std::fs::write(root.join(".env.sample"), "SECRET_TOKEN=sample_placeholder\n").unwrap();
    std::fs::write(root.join("sub/.env"), "SECRET_TOKEN=sub_secret\n").unwrap();
    std::fs::write(root.join("sub/.env.production"), "SECRET_TOKEN=prod_secret\n").unwrap();
    std::fs::write(root.join("certs/server.pem"), "SECRET_TOKEN=pem_secret\n").unwrap();
    std::fs::write(root.join("certs/private.key"), "SECRET_TOKEN=key_secret\n").unwrap();
    std::fs::write(root.join("ssh/id_rsa"), "SECRET_TOKEN=rsa_secret\n").unwrap();
    std::fs::write(root.join("ssh/id_rsa.pub"), "SECRET_TOKEN=rsa_pub\n").unwrap();
    std::fs::write(root.join("ssh/id_ed25519"), "SECRET_TOKEN=ed25519_secret\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "const SECRET_TOKEN: &str = \"public\";\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join(".env"), root.join("link_to_env")).unwrap();

    let l = load_plugin("search-tools", bundled("search-tools"), &path_str(root), json!({})).await;
    assert_eq!(l.tool("find_files").permissions_as().as_deref(), Some("read"));
    assert_eq!(l.tool("search_text").permissions_as().as_deref(), Some("read"));

    // User `read(**/.env)` deny rule blocks single-file search_text and find_files in edits mode.
    let deny_env = Arc::new(
        PermissionPolicy::new(
            PermissionMode::Edits,
            &PermissionRules {
                deny: vec!["read(**/.env)".into()],
                ..Default::default()
            },
        )
        .unwrap(),
    );
    l.agent.set_permissions(deny_env);

    for (tool, args) in [
        ("search_text", json!({"path": ".env", "query": "."})),
        ("search_text", json!({"path": "sub/.env", "query": "."})),
        ("find_files", json!({"path": ".env"})),
    ] {
        let r = l.invoke(tool, args.clone()).await;
        assert!(r.is_error(), "{tool}({args}) should be denied: {}", text_of(&r));
        assert!(text_of(&r).contains("Permission denied"), "{}", text_of(&r));
    }
    #[cfg(unix)]
    {
        let r = l
            .invoke("search_text", json!({"path": "link_to_env", "query": "."}))
            .await;
        assert!(r.is_error(), "symlink to .env should be denied: {}", text_of(&r));
    }

    // Directory walks and search output exclude secret-looking files, while keeping .env.example/.env.sample.
    let r = l.invoke("find_files", json!({})).await;
    let listed_text = text_of(&r);
    let listed: Vec<&str> = listed_text.lines().collect();
    assert!(listed.contains(&".env.example"), "{listed:?}");
    assert!(listed.contains(&".env.sample"), "{listed:?}");
    assert!(listed.contains(&"src/main.rs"), "{listed:?}");
    for forbidden in [
        ".env",
        ".env.local",
        "sub/.env",
        "sub/.env.production",
        "certs/server.pem",
        "certs/private.key",
        "ssh/id_rsa",
        "ssh/id_rsa.pub",
        "ssh/id_ed25519",
    ] {
        assert!(!listed.contains(&forbidden), "{forbidden} leaked in {listed:?}");
    }

    let r = l.invoke("search_text", json!({"query": "SECRET_TOKEN"})).await;
    let t = text_of(&r);
    assert!(t.contains(".env.example:1:"), "{t}");
    assert!(t.contains(".env.sample:1:"), "{t}");
    assert!(t.contains("src/main.rs:1:"), "{t}");
    for leaked in [
        "root_secret",
        "local_secret",
        "sub_secret",
        "prod_secret",
        "pem_secret",
        "key_secret",
        "rsa_secret",
        "rsa_pub",
        "ed25519_secret",
    ] {
        assert!(!t.contains(leaked), "{leaked} leaked in {t}");
    }

    // In ask mode, search_text prompts (and fails in non-interactive runs).
    l.agent.set_permissions(policy(PermissionMode::Ask));
    let r = l
        .invoke("search_text", json!({"path": "src/main.rs", "query": "SECRET_TOKEN"}))
        .await;
    assert!(r.is_error(), "ask mode should require approval: {}", text_of(&r));
}

#[tokio::test]
async fn search_tools_supports_gitignore_negation_nested_gitignore_and_skips_symlinks() {
    // 1. Allow-list style .gitignore (`*` + `!src/` + `!src/**`)
    let dir1 = temp_dir("sasacode-gi-allowlist-");
    let r1 = dir1.path();
    std::fs::create_dir_all(r1.join("src/nested")).unwrap();
    std::fs::create_dir_all(r1.join("other")).unwrap();
    std::fs::write(r1.join(".gitignore"), "*\n!src/\n!src/**\n").unwrap();
    std::fs::write(r1.join("root.txt"), "x\n").unwrap();
    std::fs::write(r1.join("other/skip.rs"), "x\n").unwrap();
    std::fs::write(r1.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(r1.join("src/nested/util.ts"), "export const x = 1;\n").unwrap();

    let l1 = load_plugin("search-tools", bundled("search-tools"), &path_str(r1), json!({})).await;
    let res = text_of(&l1.invoke("find_files", json!({})).await);
    let lines: Vec<&str> = res.lines().collect();
    assert_eq!(lines, vec!["src/main.rs", "src/nested/util.ts"]);

    // 2. Allow-list style (`*` + `!*/` + `!*.rs`) keeps .rs files in subdirectories while skipping .txt/.ts
    std::fs::write(r1.join(".gitignore"), "*\n!*/\n!*.rs\n").unwrap();
    let res = text_of(&l1.invoke("find_files", json!({})).await);
    assert_eq!(res, "other/skip.rs\nsrc/main.rs");

    // 3. Nested .gitignore, negation overrides, re-ignore, dir-only trailing slash, and symlinks
    let dir2 = temp_dir("sasacode-gi-nested-");
    let r2 = dir2.path();
    std::fs::create_dir_all(r2.join("sub/deep")).unwrap();
    std::fs::write(
        r2.join(".gitignore"),
        "*.log\n!root_keep.log\n!reignored.log\nreignored.log\ndir_only/\n",
    )
    .unwrap();
    std::fs::write(r2.join("sub/.gitignore"), "!keep.log\n*.tmp\n/local_only.txt\n").unwrap();
    std::fs::write(r2.join("a.log"), "1\n").unwrap();
    std::fs::write(r2.join("root_keep.log"), "1\n").unwrap();
    std::fs::write(r2.join("reignored.log"), "1\n").unwrap();
    std::fs::write(r2.join("dir_only"), "regular file named dir_only\n").unwrap();
    std::fs::write(r2.join("root.tmp"), "1\n").unwrap();
    std::fs::write(r2.join("local_only.txt"), "1\n").unwrap();
    std::fs::write(r2.join("sub/drop.log"), "1\n").unwrap();
    std::fs::write(r2.join("sub/keep.log"), "1\n").unwrap();
    std::fs::write(r2.join("sub/drop.tmp"), "1\n").unwrap();
    std::fs::write(r2.join("sub/deep/drop2.tmp"), "1\n").unwrap();
    std::fs::write(r2.join("sub/local_only.txt"), "1\n").unwrap();
    std::fs::write(r2.join("sub/deep/local_only.txt"), "1\n").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(r2.join("root_keep.log"), r2.join("link.log")).unwrap();
        std::os::unix::fs::symlink(r2, r2.join("loop_dir")).unwrap();
    }

    let l2 = load_plugin("search-tools", bundled("search-tools"), &path_str(r2), json!({})).await;
    let res = text_of(&l2.invoke("find_files", json!({})).await);
    let lines: Vec<&str> = res.lines().collect();
    assert_eq!(
        lines,
        vec![
            ".gitignore",
            "dir_only",
            "local_only.txt",
            "root.tmp",
            "root_keep.log",
            "sub/.gitignore",
            "sub/deep/local_only.txt",
            "sub/keep.log",
        ]
    );

    let search_res = text_of(&l2.invoke("search_text", json!({"query": "1"})).await);
    assert!(search_res.contains("sub/keep.log:1:"), "{search_res}");
    assert!(
        !search_res.contains("a.log")
            && !search_res.contains("drop.log")
            && !search_res.contains("reignored.log")
            && !search_res.contains("drop.tmp")
            && !search_res.contains("link.log"),
        "{search_res}"
    );

    // Searching directly inside `sub` still respects root `.gitignore` (`*.log`) and `sub/.gitignore` (`!keep.log`).
    let sub_res = text_of(&l2.invoke("find_files", json!({"path": "sub"})).await);
    let sub_lines: Vec<&str> = sub_res.lines().collect();
    assert_eq!(
        sub_lines,
        vec!["sub/.gitignore", "sub/deep/local_only.txt", "sub/keep.log"]
    );
}

#[tokio::test]
async fn error_hints_suggests_paths_skills_tools_and_sut_files() {
    let dir = temp_dir("sasacode-error-hints-");
    let root = dir.path();
    std::fs::create_dir_all(root.join("packages/tools/src")).unwrap();
    std::fs::create_dir_all(root.join("packages/tools/test")).unwrap();
    std::fs::create_dir_all(root.join(".sasacode/skills/harness-improve")).unwrap();
    std::fs::write(
        root.join("packages/tools/src/edit.ts"),
        "export function fixCount() { return 1; }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("packages/tools/test/edit.test.ts"),
        "import { fixCount } from '../src/edit';\nexpect(fixCount()).toBe(2);\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".sasacode/skills/harness-improve/SKILL.md"),
        "---\nname: harness-improve\ndescription: Improve the harness.\n---\nSteps...\n",
    )
    .unwrap();

    let s = setup(
        vec![
            reply(vec![call("1", "read", json!({"path": "packages/tools/edit.ts"}))]),
            reply(vec![call("2", "harness-improve", json!({}))]),
            reply(vec![call("3", "raed", json!({}))]),
            reply(vec![call(
                "4",
                "bash",
                json!({"command": "echo 'FAIL packages/tools/test/edit.test.ts' >&2; exit 1"}),
            )]),
            reply(vec![say("done")]),
        ],
        &["error-hints"],
        opts(&path_str(root), &[]),
    )
    .await;
    s.agent.set_permissions(policy(PermissionMode::Auto));
    s.agent.prompt_text("go").await.unwrap();

    let reqs = s.requests();
    let r1 = last(&reqs[1]).text();
    assert!(r1.contains("Did you mean: packages/tools/src/edit.ts"), "{r1}");

    let r2 = last(&reqs[2]).text();
    assert!(r2.contains("\"harness-improve\" is an Agent Skill, not a tool"), "{r2}");
    assert!(r2.contains("SKILL.md"), "{r2}");

    let r3 = last(&reqs[3]).text();
    assert!(r3.contains("Similar tools:") && r3.contains("read"), "{r3}");

    let r4 = last(&reqs[4]).text();
    assert!(
        r4.contains("Likely source file(s) under test: packages/tools/src/edit.ts"),
        "{r4}"
    );
}

#[tokio::test]
async fn action_nudge_nudges_intent_without_calls_but_ignores_questions_completions_and_long_prose() {
    let dir = temp_dir("sasacode-action-nudge-");
    let f = path_str(&dir.path().join("a.txt"));
    std::fs::write(&f, "hello\n").unwrap();

    // 1. Intent without tool call -> nudged -> calls tool -> completes -> NOT nudged
    let s = setup(
        vec![
            reply(vec![say("まずはファイルの内容を確認します。")]),
            reply(vec![call("1", "read", json!({"path": f}))]),
            reply(vec![say("確認して修正しました。テストはすべて通っています。")]),
        ],
        &["action-nudge"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    set_local_model(&s);
    s.agent.prompt_text("check a.txt").await.unwrap();
    let reqs = s.requests();
    assert_eq!(reqs.len(), 3, "expected nudge + tool turn + completion");
    assert!(json_of(last(&reqs[1])).contains("[action-nudge]"));

    // 2. Question, long prose, and maxNudges cap are respected
    let s2 = setup(
        vec![
            reply(vec![say("Let me check the logs first.")]),
            reply(vec![say("Which file should I inspect?")]),
        ],
        &["action-nudge"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    set_local_model(&s2);
    s2.agent.prompt_text("help").await.unwrap();
    assert_eq!(s2.requests().len(), 2);

    // 3. Caps at maxNudges = 2 even if model keeps declaring intent
    let s3 = setup(
        vec![
            reply(vec![say("Let me inspect the code.")]),
            reply(vec![say("I will check the code now.")]),
            reply(vec![say("I'll examine it right away.")]),
        ],
        &["action-nudge"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    set_local_model(&s3);
    s3.agent.prompt_text("go").await.unwrap();
    assert_eq!(s3.requests().len(), 3);
}

#[tokio::test]
async fn action_nudge_ignores_cloud_models_false_positive_summaries_and_conditionals() {
    let dir = temp_dir("sasacode-action-nudge-fp-");

    // 1. Cloud models (including Ollama -cloud models) are never nudged.
    for (id, server, base_url, msg) in [
        ("m", None, None, "まずはファイルの内容を確認します。"),
        (
            "gpt-oss:120b-cloud",
            Some(LocalServer::Ollama),
            Some("http://localhost:11434/v1"),
            "Let me check the logs first.",
        ),
    ] {
        let s = setup(
            vec![reply(vec![say(msg)])],
            &["action-nudge"],
            opts(&path_str(dir.path()), &[]),
        )
        .await;
        set_model_with(&s, id, server, base_url);
        s.agent.prompt_text("go").await.unwrap();
        assert_eq!(s.requests().len(), 1, "cloud model {id} should not be nudged");
    }

    // 2. Confirmed false positives and conditional offers on local models are not nudged.
    let no_nudge_replies = [
        "1. まず設定ファイルを確認します。…コードは変更していません。",
        "必要であれば修正します。",
        "First, I'll check the config loader.\nThen I'll update the tests.\nNo files were changed.",
        "必要なら修正します。",
        "ご希望なら確認します。",
        "よろしければ編集します。",
        "If you want, I'll check the logs.",
        "If you'd like, I'll update the tests.",
        "Once you confirm, I'll edit the file.",
        "Let me know if I should check the config.",
    ];
    for text in no_nudge_replies {
        let s = setup(
            vec![reply(vec![say(text)])],
            &["action-nudge"],
            opts(&path_str(dir.path()), &[]),
        )
        .await;
        set_local_model(&s);
        s.agent.prompt_text("go").await.unwrap();
        assert_eq!(s.requests().len(), 1, "unexpected nudge for: {text}");
    }
}

#[tokio::test]
async fn syntax_check_flags_broken_json_and_rust_without_false_positives_on_lifetimes() {
    // Unit checks on the Rust lexer
    assert_eq!(
        check_rust_syntax(r##"fn ok<'a>(s: &'static str) -> &'a str { let x = r#"({"#; s }"##),
        None
    );
    assert!(check_rust_syntax("fn broken() {\n  let x = (1 + 2;\n}\n").is_some());
    assert!(check_rust_syntax("fn broken() { /* nested /* ok */ unclosed }").is_some());

    let dir = temp_dir("sasacode-syntax-check-");
    let json_path = path_str(&dir.path().join("cfg.json"));
    let rs_path = path_str(&dir.path().join("lib.rs"));
    let ts_path = path_str(&dir.path().join("app.ts"));
    let py_path = path_str(&dir.path().join("mod.py"));

    let mut script = vec![
        reply(vec![call(
            "1",
            "write",
            json!({"path": json_path, "content": "{\n  \"a\": 1,\n}\n"}),
        )]),
        reply(vec![call(
            "2",
            "write",
            json!({"path": rs_path, "content": "pub fn f<'a>(s: &'a str) -> &'a str {\n    let x = (1 + 2;\n    s\n}\n"}),
        )]),
        reply(vec![call(
            "3",
            "write",
            json!({"path": rs_path, "content": "pub fn f<'a>(s: &'a str) -> &'a str {\n    let _ = r#\"({[\"#;\n    s\n}\n"}),
        )]),
    ];
    let has_bun = std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let has_py = std::process::Command::new("python3")
        .args(["-B", "-c", "import ast"])
        .output()
        .is_ok_and(|o| o.status.success());
    if has_bun {
        script.push(reply(vec![call(
            "4",
            "write",
            json!({"path": ts_path, "content": "export function broken(x: number {\n  return x + 1;\n}\n"}),
        )]));
        script.push(reply(vec![call(
            "5",
            "write",
            json!({"path": ts_path, "content": "export function ok(x: number): number {\n  return x + 1;\n}\n"}),
        )]));
    }
    if has_py {
        script.push(reply(vec![call(
            "6",
            "write",
            json!({"path": py_path, "content": "def f():\n    return (1 + 2\n"}),
        )]));
    }
    script.push(reply(vec![say("fixed")]));

    let s = setup(script, &["syntax-check"], opts(&path_str(dir.path()), &[])).await;
    s.agent.prompt_text("write files").await.unwrap();
    let reqs = s.requests();
    let r1 = last(&reqs[1]);
    assert!(
        json_of(r1).contains("[syntax-check] Syntax error in cfg.json"),
        "{r1:?}"
    );
    let r2 = last(&reqs[2]);
    assert!(json_of(r2).contains("[syntax-check] Syntax error in lib.rs"), "{r2:?}");
    let r3 = last(&reqs[3]);
    assert!(!json_of(r3).contains("[syntax-check]"), "{r3:?}");
    let mut idx = 4usize;
    if has_bun {
        let r_bad_ts = last(&reqs[idx]);
        assert!(
            json_of(r_bad_ts).contains("[syntax-check] Syntax error in app.ts"),
            "{r_bad_ts:?}"
        );
        let r_ok_ts = last(&reqs[idx + 1]);
        assert!(!json_of(r_ok_ts).contains("[syntax-check]"), "{r_ok_ts:?}");
        idx += 2;
    }
    if has_py {
        let r_bad_py = last(&reqs[idx]);
        assert!(
            json_of(r_bad_py).contains("[syntax-check] Syntax error in mod.py"),
            "{r_bad_py:?}"
        );
    }
}

/// What each call of a syntax-check script should get back: `Some(file)` a note naming the file,
/// `None` no note at all. Call ids are "1", "2", ... in order.
async fn syntax_check_run(cwd: &std::path::Path, calls: Vec<(&str, Value, Option<&str>)>) {
    let mut script: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(i, (tool, input, _))| reply(vec![call(&(i + 1).to_string(), tool, input.clone())]))
        .collect();
    script.push(reply(vec![say("done")]));
    // A generous timeout: the external checkers start slowly when every test runs at once.
    let settings = [("syntax-check", json!({"timeoutMs": 10000}))];
    let s = setup(script, &["syntax-check"], opts(&path_str(cwd), &settings)).await;
    s.agent.prompt_text("go").await.unwrap();
    let reqs = s.requests();
    assert_eq!(reqs.len(), calls.len() + 1);
    for (i, (tool, input, expected)) in calls.iter().enumerate() {
        let result = json_of(last(&reqs[i + 1]));
        let what = format!("call {} ({tool} {}): {result}", i + 1, input["path"]);
        assert!(!result.contains("\"isError\":true"), "the call failed: {what}");
        match expected {
            Some(file) => assert!(
                result.contains(&format!("[syntax-check] Syntax error in {file}")),
                "no note: {what}"
            ),
            None => assert!(!result.contains("[syntax-check]"), "a note: {what}"),
        }
    }
}

#[tokio::test]
async fn syntax_check_reports_only_what_the_edit_broke() {
    let dir = temp_dir("sasacode-syntax-baseline-");
    let root = dir.path();
    let at = |name: &str| path_str(&root.join(name));
    // Broken before the model touched them: a file the checker cannot read is not the model's to fix.
    std::fs::write(root.join("old.rs"), "pub fn f() {\n    let x = (1 + 2;\n}\n").unwrap();
    std::fs::write(root.join("old.json"), "{\n  // a comment\n  \"a\": 1\n}\n").unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/old.json"), "{\n  // a comment\n  \"a\": 1\n}\n").unwrap();

    let broken = "{\n  \"a\": 1,\n}\n";
    let jsonc = "{\n  // comments and trailing commas are this format's own\n  \"compilerOptions\": { \"strict\": true, },\n}\n";
    syntax_check_run(
        root,
        vec![
            // Already failing, still failing: silent, whether edited or rewritten.
            (
                "edit",
                json!({"path": at("old.rs"), "old_string": "let x", "new_string": "let y"}),
                None,
            ),
            (
                "write",
                json!({"path": at("old.json"), "content": "{\n  // a comment\n  \"a\": 2\n}\n"}),
                None,
            ),
            // The same by a path relative to the working directory, as models mostly give it.
            (
                "edit",
                json!({"path": "sub/old.json", "old_string": "\"a\": 1", "new_string": "\"a\": 3"}),
                None,
            ),
            (
                "write",
                json!({"path": "./sub/fresh.json", "content": broken}),
                Some("sub/fresh.json"),
            ),
            (
                "edit",
                json!({"path": "sub/fresh.json", "old_string": "\"a\": 1", "new_string": "\"a\": 4"}),
                Some("sub/fresh.json"),
            ),
            // A new file has no earlier state: the error is the write's.
            (
                "write",
                json!({"path": at("new.json"), "content": broken}),
                Some("new.json"),
            ),
            // A failed fix of a reported error is reported again, not taken for "it was like that".
            (
                "write",
                json!({"path": at("new.json"), "content": "{\n  \"a\": 2,\n}\n"}),
                Some("new.json"),
            ),
            (
                "write",
                json!({"path": at("new.json"), "content": "{\n  \"a\": 2\n}\n"}),
                None,
            ),
            (
                "edit",
                json!({"path": at("new.json"), "old_string": "\"a\": 2", "new_string": "\"a\": 2,"}),
                Some("new.json"),
            ),
            // Once it parses, the next break is the edit's doing, in the old file as well.
            (
                "write",
                json!({"path": at("old.rs"), "content": "pub fn f() {\n    let y = (1 + 2);\n}\n"}),
                None,
            ),
            (
                "edit",
                json!({"path": at("old.rs"), "old_string": "(1 + 2);", "new_string": "(1 + 2;"}),
                Some("old.rs"),
            ),
            // JSON its tools read with comments: never put to the strict parser, new file or not.
            ("write", json!({"path": at("tsconfig.json"), "content": jsonc}), None),
            (
                "write",
                json!({"path": at("tsconfig.build.json"), "content": jsonc}),
                None,
            ),
            ("write", json!({"path": at("jsconfig.json"), "content": jsonc}), None),
            (
                "write",
                json!({"path": at(".vscode/settings.json"), "content": jsonc}),
                None,
            ),
            // The same text under any other name is an error.
            (
                "write",
                json!({"path": at("data.json"), "content": jsonc}),
                Some("data.json"),
            ),
        ],
    )
    .await;
    assert_eq!(
        std::fs::read_to_string(root.join("old.rs")).unwrap(),
        "pub fn f() {\n    let y = (1 + 2;\n}\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".vscode/settings.json")).unwrap(),
        jsonc
    );
}

/// The external checkers run without approval: whatever the project holds, none of it may run.
#[tokio::test]
async fn syntax_check_runs_nothing_of_the_project() {
    let dir = temp_dir("sasacode-syntax-hostile-");
    let root = dir.path();
    let at = |name: &str| path_str(&root.join(name));
    let marker = |name: &str| root.join(name);
    let py_mark = |name: &str| format!("open({:?}, 'w').write('x')\n", path_str(&marker(name)));

    // A module named like the one the checker imports, and a preload for bun.
    std::fs::write(
        root.join("ast.py"),
        format!("{}def parse(*a, **k): pass\n", py_mark("MARK_AST")),
    )
    .unwrap();
    std::fs::write(
        root.join("evil.js"),
        format!(
            "require('fs').writeFileSync({:?}, 'x');\n",
            path_str(&marker("MARK_PRELOAD"))
        ),
    )
    .unwrap();
    std::fs::write(root.join("bunfig.toml"), "preload = [\"./evil.js\"]\n").unwrap();
    std::fs::write(root.join("legacy.py"), "def f(:\n    pass\n").unwrap();

    let has_bun = std::process::Command::new("bun")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    let has_py = std::process::Command::new("python3")
        .args(["-I", "-S", "-B", "-c", "import ast"])
        .output()
        .is_ok_and(|o| o.status.success());

    // A venv whose site-packages run code at interpreter start: started through the venv, even an
    // isolated Python runs it (checked here, so the fixture is known to be live).
    let venv_python = root.join(".venv").join("bin").join("python");
    let made_venv = has_py
        && cfg!(unix)
        && std::process::Command::new("python3")
            .args(["-m", "venv", "--without-pip"])
            .arg(root.join(".venv"))
            .output()
            .is_ok_and(|o| o.status.success());
    if made_venv {
        let lib = root.join(".venv").join("lib");
        for entry in std::fs::read_dir(&lib).unwrap().flatten() {
            let site = entry.path().join("site-packages");
            if site.is_dir() {
                std::fs::write(site.join("evil.pth"), format!("import os; {}", py_mark("MARK_PTH"))).unwrap();
            }
        }
        let _ = std::process::Command::new(&venv_python)
            .args(["-I", "-c", "pass"])
            .output();
        if marker("MARK_PTH").exists() {
            std::fs::remove_file(marker("MARK_PTH")).unwrap();
        } else {
            eprintln!("this venv does not run its .pth: that part of the test shows nothing");
        }
    }

    let mut calls = vec![
        (
            "write",
            json!({"path": at("mod.py"), "content": "x = 1\ndef f():\n    return (1 + 2\n"}),
            has_py.then_some("mod.py"),
        ),
        (
            "write",
            json!({"path": at("mod.py"), "content": "def f():\n    return 1\n"}),
            None,
        ),
        // Broken before the edit: silent.
        (
            "edit",
            json!({"path": at("legacy.py"), "old_string": "pass", "new_string": "return 1"}),
            None,
        ),
        // JSX in plain .js is no error, with bun or with node.
        (
            "write",
            json!({"path": at("view.js"), "content": "export const View = () => <div className=\"a\">hi</div>;\n"}),
            None,
        ),
    ];
    if has_bun {
        calls.push((
            "write",
            json!({"path": at("app.ts"), "content": "export function broken(x: number {\n  return x + 1;\n}\n"}),
            Some("app.ts"),
        ));
        calls.push((
            "write",
            json!({"path": at("app.ts"), "content": "const n = <number>(1 as unknown);\nexport const id = <T>(x: T) => x ?? n;\n"}),
            None,
        ));
    }
    syntax_check_run(root, calls).await;

    for name in ["MARK_AST", "MARK_PRELOAD", "MARK_PTH"] {
        assert!(!marker(name).exists(), "the project's code ran: {name}");
    }
}

#[tokio::test]
async fn context_slim_masks_old_tool_outputs_in_step_batches_preserving_prefix() {
    let dir = temp_dir("sasacode-context-slim-");
    // context_window = 2400, triggerRatio = 0.50 (1200 tokens), keepRecent = 2, maskStep = 2, minChars = 200
    let big = "line of file content\n".repeat(45); // ~945 chars
    let big_err = format!("HEAD_MARKER {} TAIL_MARKER", "x".repeat(700));
    let s = setup(
        vec![
            reply(vec![call(
                "c6",
                "read",
                json!({"path": path_str(&dir.path().join("c.txt"))}),
            )]),
            reply(vec![call(
                "c7",
                "read",
                json!({"path": path_str(&dir.path().join("big.txt"))}),
            )]),
            reply(vec![say("done")]),
        ],
        &["context-slim"],
        SetupOpts {
            cwd: Some(path_str(dir.path())),
            context_window: Some(2400),
            settings: settings_map(&[(
                "context-slim",
                json!({"triggerRatio": 0.50, "keepRecent": 2, "maskStep": 2, "minChars": 200}),
            )]),
            ..SetupOpts::default()
        },
    )
    .await;
    std::fs::write(dir.path().join("c.txt"), "tiny").unwrap();
    std::fs::write(dir.path().join("big.txt"), "more big output line\n".repeat(80)).unwrap();
    // Initial tokens ~1320 > 1200; masking batch 1 (t1, t2) drops it ~900 <= 1200 so turn 2 stays inside batch boundary
    let a1 = Message::Assistant(reply(vec![call("c1", "read", json!({"path": "src/a.rs"}))]));
    let t1 = tool_msg("c1", "read", &big, false);
    let a2 = Message::Assistant(reply(vec![call("c2", "bash", json!({"command": "cargo test"}))]));
    let t2 = tool_msg("c2", "bash", &big_err, true);
    let a3 = Message::Assistant(reply(vec![call("c3", "read", json!({"path": "src/b.rs"}))]));
    let t3 = tool_msg("c3", "read", &big, false);
    let a4 = Message::Assistant(reply(vec![call("c4", "read", json!({"path": "src/c.rs"}))]));
    let t4 = tool_msg("c4", "read", &big, false);
    let a5 = Message::Assistant(reply(vec![call("c5", "read", json!({"path": "src/d.rs"}))]));
    let t5 = tool_msg("c5", "read", &big, false);
    s.agent.reset(vec![
        Message::user(vec![UserContent::text("go")]),
        a1,
        t1,
        a2,
        t2,
        a3,
        t3,
        a4,
        t4,
        a5,
        t5,
    ]);
    set_local_model(&s);
    s.agent.prompt_text("next").await.unwrap();
    let reqs = s.requests();
    let sent = &reqs[0].messages;
    // t1 and t2 should be masked (batch 1 of size 2), t5 kept recent (and original messages in agent stay intact!)
    let t1_sent = sent[2].text();
    let t2_sent = sent[4].text();
    assert!(
        t1_sent.contains("[context-slim: omitted") && t1_sent.contains("read(src/a.rs)"),
        "{t1_sent}"
    );
    assert!(
        t2_sent.contains("HEAD_MARKER")
            && t2_sent.contains("[context-slim: omitted")
            && t2_sent.contains("TAIL_MARKER"),
        "{t2_sent}"
    );
    assert_eq!(sent[10].text(), big);
    // Underlying stored session history is not mutated
    assert_eq!(s.agent.messages()[2].text(), big);
    // Prefix up through t5 stays byte-identical on the next turn when below threshold (KV cache preservation)
    assert_eq!(reqs[1].messages[..11], reqs[0].messages[..11]);
    // Once big.txt pushes tokens over threshold again, batch 2 (t3, t4) gets masked
    assert!(
        reqs[2].messages[6].text().contains("[context-slim: omitted"),
        "{}",
        reqs[2].messages[6].text()
    );
    assert!(
        reqs[2].messages[8].text().contains("[context-slim: omitted"),
        "{}",
        reqs[2].messages[8].text()
    );
}

#[tokio::test]
async fn context_slim_ignores_cloud_models_and_never_masks_subagent_or_skill_reads() {
    let dir = temp_dir("sasacode-context-slim-scope-");
    let big = "line of file content\n".repeat(45); // ~945 chars
    let slim_opts = || SetupOpts {
        cwd: Some(path_str(dir.path())),
        context_window: Some(2400),
        settings: settings_map(&[(
            "context-slim",
            json!({"triggerRatio": 0.50, "keepRecent": 2, "maskStep": 4, "minChars": 200}),
        )]),
        ..SetupOpts::default()
    };

    // 1. Cloud models (including Ollama -cloud models) leave requests completely untouched.
    for (id, server, base_url) in [
        ("m", None, None),
        (
            "gpt-oss:120b-cloud",
            Some(LocalServer::Ollama),
            Some("http://localhost:11434/v1"),
        ),
    ] {
        let s = setup(vec![reply(vec![say("done")])], &["context-slim"], slim_opts()).await;
        set_model_with(&s, id, server, base_url);
        let history = vec![
            Message::user(vec![UserContent::text("go")]),
            Message::Assistant(reply(vec![call("c1", "read", json!({"path": "src/a.rs"}))])),
            tool_msg("c1", "read", &big, false),
            Message::Assistant(reply(vec![call("c2", "bash", json!({"command": "cargo test"}))])),
            tool_msg("c2", "bash", &big, false),
            Message::Assistant(reply(vec![call("c3", "read", json!({"path": "src/b.rs"}))])),
            tool_msg("c3", "read", &big, false),
            Message::Assistant(reply(vec![call("c4", "read", json!({"path": "src/c.rs"}))])),
            tool_msg("c4", "read", &big, false),
        ];
        s.agent.reset(history);
        s.agent.prompt_text("next").await.unwrap();
        let sent = &s.requests()[0].messages;
        assert_eq!(sent[2].text(), big, "cloud model {id} must not mask read");
        assert_eq!(sent[4].text(), big, "cloud model {id} must not mask bash");
        assert!(!json_of(sent).contains("[context-slim:"));
    }

    // 2. Local model past threshold: task (subagent), read of SKILL.md, and non-search/exec tools
    //    are never masked; read / find_files / search_text are masked.
    let s = setup(vec![reply(vec![say("done")])], &["context-slim"], slim_opts()).await;
    set_local_model(&s);
    s.agent.reset(vec![
        Message::user(vec![UserContent::text("go")]),
        Message::Assistant(reply(vec![call(
            "t1",
            "task",
            json!({"description": "sub", "prompt": "check"}),
        )])),
        tool_msg("t1", "task", &big, false),
        Message::Assistant(reply(vec![call(
            "t2",
            "read",
            json!({"path": ".sasacode/skills/deploy/SKILL.md"}),
        )])),
        tool_msg("t2", "read", &big, false),
        Message::Assistant(reply(vec![call(
            "t3",
            "web_fetch",
            json!({"url": "https://example.com"}),
        )])),
        tool_msg("t3", "web_fetch", &big, false),
        Message::Assistant(reply(vec![call("t4", "read", json!({"path": "src/lib.rs"}))])),
        tool_msg("t4", "read", &big, false),
        Message::Assistant(reply(vec![call("t5", "find_files", json!({"pattern": "*.rs"}))])),
        tool_msg("t5", "find_files", &big, false),
        Message::Assistant(reply(vec![call("t6", "search_text", json!({"query": "main"}))])),
        tool_msg("t6", "search_text", &big, false),
        Message::Assistant(reply(vec![call("t7", "read", json!({"path": "src/recent.rs"}))])),
        tool_msg("t7", "read", &big, false),
    ]);
    s.agent.prompt_text("next").await.unwrap();
    let sent = &s.requests()[0].messages;
    assert_eq!(sent[2].text(), big, "subagent report must never be masked");
    assert_eq!(sent[4].text(), big, "SKILL.md read must never be masked");
    assert_eq!(sent[6].text(), big, "non-allowlisted tool must never be masked");
    assert!(
        sent[8].text().contains("[context-slim: omitted") && sent[8].text().contains("read(src/lib.rs)"),
        "{}",
        sent[8].text()
    );
    assert!(
        sent[10].text().contains("[context-slim: omitted") && sent[10].text().contains("find_files"),
        "{}",
        sent[10].text()
    );
    assert!(
        sent[12].text().contains("[context-slim: omitted") && sent[12].text().contains("search_text"),
        "{}",
        sent[12].text()
    );
    assert_eq!(sent[14].text(), big, "recent read stays unmasked");
}

#[tokio::test]
async fn thinking_budget_stops_local_model_runaway_thinking_but_not_cloud_default() {
    let dir = temp_dir("sasacode-thinking-budget-");
    let run_with = |id: &'static str, local: bool| {
        let cwd = path_str(dir.path());
        async move {
            let api = api_name();
            let provider = Arc::new(ReplayProvider::new(
                vec![
                    reply_at(vec![think(&"thinking step ".repeat(30))], 11),
                    reply_at(vec![say("concise answer")], 22),
                ],
                ReplayOptions::api(&api),
            ));
            register_api(&api, provider.clone());
            let mut m: ModelInfo = serde_json::from_value(json!({
                "id": id, "provider": "t", "api": api, "contextWindow": 32_000, "maxOutput": 1000
            }))
            .unwrap();
            if local {
                m.server = Some(LocalServer::Ollama);
            }
            let agent = Agent::new(AgentOptions::new(m, cwd.clone(), "base"));
            let host = PluginHost::new(PluginHostOptions {
                agent: agent.clone(),
                cwd,
                settings: Some(Arc::new(|_| json!({"localMaxChars": 100}))),
                register_provider: None,
                disabled_tools: Vec::new(),
                lang: Lang::En,
            });
            assert!(host.load("thinking-budget", bundled("thinking-budget")).await);
            agent.prompt_text("go").await.unwrap();
            (agent.messages(), provider.requests())
        }
    };

    let (local_msgs, local_reqs) = run_with("m", true).await;
    assert_eq!(local_reqs.len(), 2);
    assert!(json_of(&local_reqs[1].messages).contains("[thinking-budget]"));
    assert_eq!(local_msgs.last().unwrap().text(), "concise answer");

    let (cloud_msgs, cloud_reqs) = run_with("m", false).await;
    assert_eq!(cloud_reqs.len(), 1);
    assert_eq!(cloud_msgs.len(), 2);

    let (ollama_cloud_msgs, ollama_cloud_reqs) = run_with("gpt-oss:120b-cloud", true).await;
    assert_eq!(ollama_cloud_reqs.len(), 1);
    assert_eq!(ollama_cloud_msgs.len(), 2);
}

#[tokio::test]
async fn hashline_edit_annotates_read_edits_by_anchor_rejects_stale_hash_and_strips_write_prefixes() {
    let dir = temp_dir("sasacode-hashline-");
    let file = dir.path().join("code.ts");
    std::fs::write(&file, "const a = 1;\n  const b = 2;\nconst c = 3;\n").unwrap();
    let h1 = line_hash("const a = 1;");
    let h2 = line_hash("  const b = 2;");
    let h3 = line_hash("const c = 3;");
    let f = path_str(&file);

    let s = setup(
        vec![
            reply(vec![call("1", "read", json!({"path": f}))]),
            // stale hash rejected cleanly with updated window
            reply(vec![call(
                "2",
                "hash_edit",
                json!({"path": f, "start": "2:ff", "content": "  const b = 20;"}),
            )]),
            // valid replace
            reply(vec![call(
                "3",
                "hash_edit",
                json!({"path": f, "start": format!("2:{h2}"), "content": "  const b = 20;"}),
            )]),
            // insert_after line 3
            reply(vec![call(
                "4",
                "hash_edit",
                json!({"path": f, "start": format!("3:{h3}"), "mode": "insert_after", "content": "const d = 4;"}),
            )]),
            // write with accidental N:hh| prefixes gets stripped before writing
            reply(vec![call(
                "5",
                "write",
                json!({"path": f, "content": format!("1:{h1}|const a = 1;\n2:{h2}|  const b = 99;\n")}),
            )]),
            reply(vec![say("done")]),
        ],
        &["hashline-edit", "write-guard"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    s.agent.prompt_text("edit file").await.unwrap();
    let reqs = s.requests();
    let read_out = last(&reqs[1]).text();
    assert!(
        read_out.contains(&format!("1:{h1}|const a = 1;\n2:{h2}|  const b = 2;")),
        "{read_out}"
    );

    let stale_out = last(&reqs[2]).text();
    assert!(
        stale_out.contains("Hash mismatch at") && stale_out.contains(&format!("2:{h2}|  const b = 2;")),
        "{stale_out}"
    );

    assert!(last(&reqs[3]).text().contains("Edited"), "{}", last(&reqs[3]).text());
    assert!(last(&reqs[4]).text().contains("Edited"), "{}", last(&reqs[4]).text());

    // Final write should have been stripped of 1:xx| prefixes
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "const a = 1;\n  const b = 99;\n"
    );
}

#[tokio::test]
async fn clean_retry_rolls_back_files_and_resets_history_after_repeated_failures() {
    let dir = temp_dir("sasacode-clean-retry-");
    let target = dir.path().join("app.txt");
    let extra = dir.path().join("junk.txt");
    std::fs::write(&target, "original\n").unwrap();
    let target_s = path_str(&target);
    let extra_s = path_str(&extra);
    let sess_dir = temp_dir("sasacode-clean-retry-sess-");
    let session = SessionFile::create(sess_dir.path(), &path_str(dir.path()), Vec::new()).unwrap();

    let s = setup(
        vec![
            reply(vec![call(
                "1",
                "write",
                json!({"path": target_s, "content": "broken\n"}),
            )]),
            reply(vec![call("2", "write", json!({"path": extra_s, "content": "temp\n"}))]),
            reply(vec![call(
                "3",
                "bash",
                json!({"command": "cargo test --manifest-path missing-1/Cargo.toml"}),
            )]),
            reply(vec![call(
                "4",
                "bash",
                json!({"command": "cargo test --manifest-path missing-2/Cargo.toml"}),
            )]),
            // Clean retry triggers at TurnEnd after 2nd test failure!
            reply(vec![call("5", "read", json!({"path": target_s}))]),
            reply(vec![say("recovered")]),
        ],
        &["clean-retry"],
        SetupOpts {
            cwd: Some(path_str(dir.path())),
            session: Some(session),
            settings: settings_map(&[("clean-retry", json!({"triggerFailures": 2, "maxRetries": 1}))]),
            ..SetupOpts::default()
        },
    )
    .await;
    s.agent.set_permissions(policy(PermissionMode::Auto));
    s.agent.prompt_text("fix app").await.unwrap();

    // Target restored, newly created junk deleted
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original\n");
    assert!(!extra.exists());

    // Request 4 (index 4, after clean-retry) starts fresh with clean-retry prompt + original prompt
    let reqs = s.requests();
    assert_eq!(reqs.len(), 6);
    assert_eq!(reqs[4].messages.len(), 1);
    let retry_msg = reqs[4].messages[0].text();
    assert!(
        retry_msg.contains("fix app") && retry_msg.contains("[clean-retry]"),
        "{retry_msg}"
    );
    assert_eq!(last(&reqs[5]).text().trim(), "1\toriginal");
}

#[tokio::test]
async fn clean_retry_ignores_denied_tool_calls_and_grep_nonzero_exits() {
    for cmd in "cargo test\ncargo check\ncargo build\ncargo clippy -- -D warnings\ncargo nextest run\n\
RUST_BACKTRACE=1 cargo test\ncd crates/foo && cargo test 2>&1 | tail -n 20\nenv CI=1 pytest -q\n\
python -m pytest\npython3 -m mypy src/\npython -m unittest\nnpm test\nnpm run test\n\
pnpm run lint\nyarn run typecheck\nbun test\nbun run build\ngo test ./...\ngo vet ./...\n\
go build ./...\ndeno test\ndeno check main.ts\nmake\ntsc --noEmit\njest\nvitest\nruff check .\neslint src/"
        .lines()
    {
        assert!(looks_like_check_command(cmd), "expected check: {cmd}");
    }
    for cmd in "grep foo tests/\ngrep -rn \"cargo test\" src/\nrg test crates/\n\
echo test fail 1 >&2; exit 1\ncd tests && ls -la\ncargo run -- test\nnpm install jest"
        .lines()
    {
        assert!(!looks_like_check_command(cmd), "expected non-check: {cmd}");
    }

    let dir = temp_dir("sasacode-clean-retry-5-");
    let target = dir.path().join("app.txt");
    let tests_dir = dir.path().join("tests");
    std::fs::create_dir_all(&tests_dir).unwrap();
    std::fs::write(&target, "v0\n").unwrap();
    std::fs::write(tests_dir.join("sample.txt"), "hello\n").unwrap();
    let target_s = path_str(&target);

    // (a) 4 user denials on edit/write must NOT count as failures or trigger rollback
    let calls_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_seen_cb = calls_seen.clone();
    let s1 = setup(
        vec![
            reply(vec![call("1", "write", json!({"path": target_s, "content": "v1\n"}))]),
            reply(vec![call(
                "2",
                "edit",
                json!({"path": target_s, "old_string": "v1", "new_string": "d1"}),
            )]),
            reply(vec![call(
                "3",
                "edit",
                json!({"path": target_s, "old_string": "v1", "new_string": "d2"}),
            )]),
            reply(vec![call(
                "4",
                "edit",
                json!({"path": target_s, "old_string": "v1", "new_string": "d3"}),
            )]),
            reply(vec![call(
                "5",
                "edit",
                json!({"path": target_s, "old_string": "v1", "new_string": "d4"}),
            )]),
            reply(vec![say("stopped after denials")]),
        ],
        &["clean-retry"],
        opts(
            &path_str(dir.path()),
            &[("clean-retry", json!({"triggerEditErrors": 4, "maxRetries": 1}))],
        ),
    )
    .await;
    s1.agent.set_permissions(policy(PermissionMode::Ask));
    s1.agent.set_approve(Some(Arc::new(move |_| {
        let n = calls_seen_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            Ok(ApprovalAnswer::new(if n == 0 {
                ApprovalDecision::Allow
            } else {
                ApprovalDecision::Deny
            }))
        })
    })));
    s1.agent.prompt_text("update app").await.unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1\n");
    assert_eq!(s1.requests().len(), 6);
    assert!(
        !s1.requests()
            .iter()
            .any(|r| json_of(&r.messages).contains("[clean-retry]"))
    );

    // (b) `grep foo tests/` exiting nonzero 3 times must NOT trigger a retry
    std::fs::write(&target, "v0\n").unwrap();
    let s2 = setup(
        vec![
            reply(vec![call("1", "write", json!({"path": target_s, "content": "v1\n"}))]),
            reply(vec![call(
                "2",
                "bash",
                json!({"command": "grep definitely_missing_token tests/"}),
            )]),
            reply(vec![call(
                "3",
                "bash",
                json!({"command": "grep another_missing_token tests/"}),
            )]),
            reply(vec![call(
                "4",
                "bash",
                json!({"command": "grep third_missing_token tests/"}),
            )]),
            reply(vec![say("finished search")]),
        ],
        &["clean-retry"],
        opts(
            &path_str(dir.path()),
            &[("clean-retry", json!({"triggerFailures": 2, "maxRetries": 1}))],
        ),
    )
    .await;
    s2.agent.set_permissions(policy(PermissionMode::Auto));
    s2.agent.prompt_text("search tests").await.unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1\n");
    assert_eq!(s2.requests().len(), 5);
    assert!(
        !s2.requests()
            .iter()
            .any(|r| json_of(&r.messages).contains("[clean-retry]"))
    );
}

#[tokio::test]
async fn clean_retry_preserves_snapshots_and_base_len_across_mid_run_user_prompt() {
    let dir = temp_dir("sasacode-clean-retry-6a-");
    let target = dir.path().join("app.txt");
    std::fs::write(&target, "original\n").unwrap();
    let target_s = path_str(&target);

    let entered_pause = Arc::new(tokio::sync::Notify::new());
    let resume_pause = Arc::new(tokio::sync::Notify::new());
    let prompt_seen = Arc::new(tokio::sync::Notify::new());

    let s = setup(
        vec![
            // Prior completed turn so base_len is non-zero (2 messages).
            reply(vec![say("prior answer")]),
            // Run 2: edit app.txt, fail check 1, pause for mid-run UserPrompt, fail check 2 -> triggers retry!
            reply(vec![call(
                "1",
                "write",
                json!({"path": target_s, "content": "broken\n"}),
            )]),
            reply(vec![call(
                "2",
                "bash",
                json!({"command": "cargo test --manifest-path missing-1/Cargo.toml"}),
            )]),
            reply(vec![call("3", "pause_point", json!({}))]),
            reply(vec![call(
                "4",
                "bash",
                json!({"command": "cargo test --manifest-path missing-2/Cargo.toml"}),
            )]),
            reply(vec![say("recovered")]),
        ],
        &["clean-retry"],
        opts(
            &path_str(dir.path()),
            &[("clean-retry", json!({"triggerFailures": 2, "maxRetries": 1}))],
        ),
    )
    .await;
    s.agent.set_permissions(policy(PermissionMode::Auto));

    let entered_cb = entered_pause.clone();
    let resume_cb = resume_pause.clone();
    s.agent.set_tool(
        ToolDef::new(
            "pause_point",
            "test pause hook",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            ToolKind::Read,
            move |_args, _ctx| {
                let entered = entered_cb.clone();
                let resume = resume_cb.clone();
                async move {
                    entered.notify_one();
                    resume.notified().await;
                    Ok(ToolResult::text("resumed"))
                }
            },
        )
        .into_ref(),
    );
    let seen_cb = prompt_seen.clone();
    s.agent
        .hooks()
        .on::<hook::UserPrompt, _, _>("test-observer", move |ev, _| {
            let seen = seen_cb.clone();
            let text: String = ev
                .content
                .iter()
                .filter_map(UserContent::as_text)
                .collect::<Vec<_>>()
                .join("");
            async move {
                if text.contains("mid-run follow-up") {
                    seen.notify_one();
                }
                Ok(None)
            }
        });

    // Prior conversation before the failing run.
    s.agent.prompt_text("prior question").await.unwrap();

    let agent1 = s.agent.clone();
    let h1 = tokio::spawn(async move { agent1.prompt_text("fix app").await });

    entered_pause.notified().await;
    let agent2 = s.agent.clone();
    let h2 = tokio::spawn(async move { agent2.prompt_text("mid-run follow-up").await });
    prompt_seen.notified().await;
    resume_pause.notify_one();

    h1.await.unwrap().unwrap();
    h2.await.unwrap().unwrap();

    // Snapshot from before the mid-run UserPrompt MUST still restore app.txt.
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "original\n");

    let reqs = s.requests();
    assert_eq!(reqs.len(), 6);
    // Request 5 (after clean-retry trigger) keeps the 2 prior messages + clean-retry prompt (base_len = 2, NOT mid-run).
    assert_eq!(reqs[5].messages.len(), 3);
    assert_eq!(reqs[5].messages[0].text(), "prior question");
    assert_eq!(reqs[5].messages[1].text(), "prior answer");
    let retry_msg = reqs[5].messages[2].text();
    assert!(
        retry_msg.contains("fix app") && retry_msg.contains("[clean-retry]"),
        "{retry_msg}"
    );
    assert!(!retry_msg.contains("mid-run follow-up"), "{retry_msg}");
}

#[tokio::test]
async fn clean_retry_lists_unrestorable_files_and_protects_mid_run_user_edits() {
    // Part 1: large file (> maxFileKB) + file changed via bash + snapshotted file + created file
    let dir1 = temp_dir("sasacode-clean-retry-6b-unrestorable-");
    let small = dir1.path().join("small.txt");
    let big = dir1.path().join("big.txt");
    let bash_file = dir1.path().join("bash_mod.txt");
    let created = dir1.path().join("created.txt");
    std::fs::write(&small, "small-v0\n").unwrap();
    std::fs::write(&big, "x".repeat(2048)).unwrap();
    std::fs::write(&bash_file, "bash-v0\n").unwrap();
    let small_s = path_str(&small);
    let big_s = path_str(&big);
    let created_s = path_str(&created);

    let s1 = setup(
        vec![
            reply(vec![call(
                "1",
                "write",
                json!({"path": small_s, "content": "small-bad\n"}),
            )]),
            reply(vec![call(
                "2",
                "write",
                json!({"path": created_s, "content": "temp\n"}),
            )]),
            reply(vec![call("3", "write", json!({"path": big_s, "content": "big-bad\n"}))]),
            reply(vec![call(
                "4",
                "bash",
                json!({"command": "echo bash-bad > bash_mod.txt"}),
            )]),
            reply(vec![call(
                "5",
                "bash",
                json!({"command": "cargo test --manifest-path missing-1/Cargo.toml"}),
            )]),
            reply(vec![call(
                "6",
                "bash",
                json!({"command": "cargo test --manifest-path missing-2/Cargo.toml"}),
            )]),
            reply(vec![say("recovered")]),
        ],
        &["clean-retry"],
        opts(
            &path_str(dir1.path()),
            &[(
                "clean-retry",
                json!({"triggerFailures": 2, "maxRetries": 1, "maxFileKB": 1}),
            )],
        ),
    )
    .await;
    s1.agent.set_permissions(policy(PermissionMode::Auto));
    s1.agent.prompt_text("fix project").await.unwrap();

    // Snapshotted changes rolled back; unrestorable changes remain and are listed honestly.
    assert_eq!(std::fs::read_to_string(&small).unwrap(), "small-v0\n");
    assert!(!created.exists());
    assert_eq!(std::fs::read_to_string(&big).unwrap(), "big-bad\n");
    assert_eq!(std::fs::read_to_string(&bash_file).unwrap(), "bash-bad\n");

    let reqs1 = s1.requests();
    assert_eq!(reqs1.len(), 7);
    let retry_msg1 = reqs1[6].messages[0].text();
    assert!(
        !retry_msg1.contains("All file changes were rolled back"),
        "{retry_msg1}"
    );
    assert!(retry_msg1.contains(&big_s), "{retry_msg1}");
    assert!(
        retry_msg1.contains("Changes made through bash were not rolled back."),
        "{retry_msg1}"
    );

    // Part 2: manual user edit mid-run must NOT be overwritten, and pre-prompt uncommitted changes stay preserved when untouched mid-run
    let dir2 = temp_dir("sasacode-clean-retry-6b-user-edit-");
    let app = dir2.path().join("app.txt");
    let uncommitted = dir2.path().join("uncommitted.txt");
    std::fs::write(&app, "pre-prompt\n").unwrap();
    std::fs::write(&uncommitted, "user-uncommitted-v0\n").unwrap();
    let app_s = path_str(&app);
    let uncommitted_s = path_str(&uncommitted);

    let s2 = setup(
        vec![
            reply(vec![call(
                "1",
                "write",
                json!({"path": uncommitted_s, "content": "agent-v1\n"}),
            )]),
            reply(vec![call(
                "2",
                "write",
                json!({"path": app_s, "content": "agent-edit\n"}),
            )]),
            reply(vec![call("3", "simulate_user_edit", json!({}))]),
            reply(vec![call(
                "4",
                "bash",
                json!({"command": "cargo test --manifest-path missing-1/Cargo.toml"}),
            )]),
            reply(vec![call(
                "5",
                "bash",
                json!({"command": "cargo test --manifest-path missing-2/Cargo.toml"}),
            )]),
            reply(vec![say("done")]),
        ],
        &["clean-retry"],
        opts(
            &path_str(dir2.path()),
            &[("clean-retry", json!({"triggerFailures": 2, "maxRetries": 1}))],
        ),
    )
    .await;
    s2.agent.set_permissions(policy(PermissionMode::Auto));
    let app_for_tool = app.clone();
    s2.agent.set_tool(
        ToolDef::new(
            "simulate_user_edit",
            "simulates a user editing app.txt mid-run",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            ToolKind::Read,
            move |_args, _ctx| {
                let p = app_for_tool.clone();
                async move {
                    std::fs::write(&p, "user-manual-mid-run\n").unwrap();
                    Ok(ToolResult::text("user edited"))
                }
            },
        )
        .into_ref(),
    );
    s2.agent.prompt_text("fix both").await.unwrap();

    // Uncommitted pre-prompt file restored; manually mid-run edited file preserved!
    assert_eq!(std::fs::read_to_string(&uncommitted).unwrap(), "user-uncommitted-v0\n");
    assert_eq!(std::fs::read_to_string(&app).unwrap(), "user-manual-mid-run\n");

    let reqs2 = s2.requests();
    assert_eq!(reqs2.len(), 6);
    let retry_msg2 = reqs2[5].messages[0].text();
    assert!(
        !retry_msg2.contains("All file changes were rolled back"),
        "{retry_msg2}"
    );
    assert!(retry_msg2.contains(&app_s), "{retry_msg2}");
}

#[tokio::test]
async fn slim_profile_adds_concise_rules_and_caps_oversized_read_and_bash_output() {
    let dir = temp_dir("sasacode-slim-profile-");
    let big_file = dir.path().join("big.txt");
    let big_content = (0..120)
        .map(|i| format!("line-{i:03} padding text here"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&big_file, &big_content).unwrap();

    let s = setup(
        vec![
            reply(vec![call("1", "read", json!({"path": path_str(&big_file)}))]),
            reply(vec![call("2", "bash", json!({"command": "printf '%02500d' 0"}))]),
            reply(vec![say("ok")]),
        ],
        &["slim-profile"],
        opts(
            &path_str(dir.path()),
            &[("slim-profile", json!({"readMaxChars": 800, "bashMaxChars": 700}))],
        ),
    )
    .await;
    s.agent.set_permissions(policy(PermissionMode::Auto));
    // Note: in setup(), hooks fire after session_start; trigger prompt_text to observe SystemPrompt & ToolResult
    s.agent.prompt_text("inspect").await.unwrap();
    let reqs = s.requests();
    assert!(reqs[0].system.contains("Small-model rules:"), "{}", reqs[0].system);
    assert!(
        last(&reqs[1]).text().contains("[slim-profile: truncated"),
        "{}",
        last(&reqs[1]).text()
    );
    assert!(
        last(&reqs[2]).text().contains("[slim-profile: truncated"),
        "{}",
        last(&reqs[2]).text()
    );
}

#[tokio::test]
async fn hash_edit_reports_permissions_as_edit_and_enforces_edit_rules_without_leaking_unread_files() {
    let dir = temp_dir("sasacode-hashline-perm-");
    let env_file = dir.path().join(".env");
    let ok_file = dir.path().join("ok.txt");
    std::fs::write(&env_file, "SECRET_TOKEN=super-secret-value\nOTHER=1\n").unwrap();
    std::fs::write(&ok_file, "alpha\nbeta\n").unwrap();

    let l = load_plugin(
        "hashline-edit",
        bundled("hashline-edit"),
        &path_str(dir.path()),
        json!({}),
    )
    .await;
    let tool = l.tool("hash_edit");
    assert_eq!(tool.permissions_as().as_deref(), Some("edit"));
    assert_eq!(
        tool.paths(&obj(json!({"path": ".env"})), dir.path()),
        Some(vec![path_str(&env_file)])
    );

    let rules = PermissionRules {
        deny: vec!["edit(**/.env)".into()],
        ..Default::default()
    };
    let perm = Arc::new(PermissionPolicy::new(PermissionMode::Edits, &rules).unwrap());
    let env_verdict = perm.check(&PermissionCheck {
        tool: tool.clone(),
        args: obj(json!({"path": ".env", "start": "1:00", "content": "x"})),
        cwd: path_str(dir.path()),
    });
    assert_eq!(env_verdict.decision, Decision::Deny);
    let ok_verdict = perm.check(&PermissionCheck {
        tool,
        args: obj(json!({"path": "ok.txt", "start": "1:00", "content": "x"})),
        cwd: path_str(dir.path()),
    });
    assert_eq!(ok_verdict.decision, Decision::Allow);

    // Even if only `read(**/.env)` is denied (so `hash_edit` reaches execution), a mismatch on an
    // unread file must not print file lines.
    let s = setup(
        vec![
            reply(vec![call(
                "1",
                "hash_edit",
                json!({"path": path_str(&env_file), "start": "1:00", "content": "x"}),
            )]),
            reply(vec![say("done")]),
        ],
        &["hashline-edit"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    let read_deny_rules = PermissionRules {
        deny: vec!["read(**/.env)".into()],
        ..Default::default()
    };
    s.agent.set_permissions(Arc::new(
        PermissionPolicy::new(PermissionMode::Edits, &read_deny_rules).unwrap(),
    ));
    s.agent.prompt_text("probe").await.unwrap();
    let reqs = s.requests();
    let out = last(&reqs[1]).text();
    assert!(out.contains("Hash mismatch at"), "{out}");
    assert!(!out.contains("super-secret-value"), "leaked unread content: {out}");
}

#[tokio::test]
async fn hash_edit_returns_fresh_anchors_and_rejects_stale_brace_anchor_after_insert() {
    let dir = temp_dir("sasacode-hashline-stale-");
    let file = dir.path().join("mod.rs");
    let original = [
        "fn first() {",
        "    let a = 1;",
        "    let b = 2;",
        "}",
        "fn middle() {",
        "    let m1 = 1;",
        "    let m2 = 2;",
        "    let m3 = 3;",
        "}",
        "fn second() {",
        "}",
        "fn third() {",
        "    done();",
        "}",
    ]
    .join("\n")
        + "\n";
    std::fs::write(&file, &original).unwrap();
    let f = path_str(&file);
    let h2 = line_hash("    let a = 1;");
    let h_brace = line_hash("}");
    let h_inserted = line_hash("    let a3 = 4;");

    // 1. Read -> insert 2 lines at line 2 -> reuse stale `11:<h_brace>` (old line 9 `}` shifted
    //    to line 11!) -> must be rejected, then edit line 4 using fresh anchor from result, then
    //    re-read and edit line 13 `}` using fresh anchor from read.
    let s = setup(
        vec![
            reply(vec![call("1", "read", json!({"path": f}))]),
            reply(vec![call(
                "2",
                "hash_edit",
                json!({
                    "path": f,
                    "start": format!("2:{h2}"),
                    "mode": "insert_after",
                    "content": "    let a2 = 3;\n    let a3 = 4;"
                }),
            )]),
            // Old line 11 (`fn second`'s `}`) is stale after +2 shift; line 11 now holds `fn middle`'s `}`!
            reply(vec![call(
                "3",
                "hash_edit",
                json!({"path": f, "start": format!("11:{h_brace}"), "content": "} // second"}),
            )]),
            // Fresh anchor `4:<h_inserted>` returned by call 2 must work without re-reading.
            reply(vec![call(
                "4",
                "hash_edit",
                json!({"path": f, "start": format!("4:{h_inserted}"), "content": "    let a3 = 40;"}),
            )]),
            // Re-read, then edit line 13 (`fn second`'s shifted `}`).
            reply(vec![call("5", "read", json!({"path": f}))]),
            reply(vec![call(
                "6",
                "hash_edit",
                json!({"path": f, "start": format!("13:{h_brace}"), "content": "} // second"}),
            )]),
            reply(vec![say("done")]),
        ],
        &["hashline-edit"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    s.agent.prompt_text("edit safely").await.unwrap();
    let reqs = s.requests();

    // Call 2 returns fresh anchors for the edited region.
    let insert_out = last(&reqs[2]).text();
    assert!(
        insert_out.contains(&format!("4:{h_inserted}|    let a3 = 4;")),
        "{insert_out}"
    );

    // Call 3 was rejected instead of silently corrupting `fn middle`'s `}` at line 11.
    let stale_out = last(&reqs[3]).text();
    assert!(
        stale_out.contains("Stale anchor") && stale_out.contains("Re-read"),
        "{stale_out}"
    );

    // Call 4 (using fresh anchor from call 2) and Call 6 (after re-read) both succeeded.
    assert!(last(&reqs[4]).text().contains("Edited"), "{}", last(&reqs[4]).text());
    assert!(last(&reqs[6]).text().contains("Edited"), "{}", last(&reqs[6]).text());

    let disk = std::fs::read_to_string(&file).unwrap();
    let lines: Vec<&str> = disk.lines().collect();
    assert_eq!(lines[3], "    let a3 = 40;");
    assert_eq!(lines[10], "}", "middle()'s closing brace at line 11 must be untouched");
    assert_eq!(
        lines[12], "} // second",
        "second()'s closing brace at line 13 should be edited"
    );

    // 2. Same-turn batch: insert 2 lines + reuse stale `}` within the same turn must also reject.
    let file2 = dir.path().join("batch.rs");
    std::fs::write(&file2, "fn a() {\n    x();\n}\nfn b() {\n}\n").unwrap();
    let f2 = path_str(&file2);
    let hx = line_hash("    x();");
    let s2 = setup(
        vec![
            reply(vec![call("1", "read", json!({"path": f2}))]),
            reply(vec![
                call(
                    "2",
                    "hash_edit",
                    json!({
                        "path": f2,
                        "start": format!("2:{hx}"),
                        "mode": "insert_after",
                        "content": "    y();\n    z();"
                    }),
                ),
                call(
                    "3",
                    "hash_edit",
                    json!({"path": f2, "start": format!("5:{h_brace}"), "content": "} // b"}),
                ),
            ]),
            reply(vec![say("done")]),
        ],
        &["hashline-edit"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    s2.agent.prompt_text("batch").await.unwrap();
    let batch_reqs = s2.requests();
    let second_call_out = json_of(&batch_reqs[2].messages);
    assert!(second_call_out.contains("Stale anchor"), "{second_call_out}");
    assert_eq!(
        std::fs::read_to_string(&file2).unwrap(),
        "fn a() {\n    x();\n    y();\n    z();\n}\nfn b() {\n}\n"
    );

    // 3. External edit on disk between `read` and `hash_edit` must reject stale `}` anchor.
    let file3 = dir.path().join("ext.rs");
    std::fs::write(&file3, &original).unwrap();
    let f3 = path_str(&file3);
    let s3 = setup(
        vec![
            reply(vec![call("1", "read", json!({"path": f3}))]),
            reply(vec![call(
                "2",
                "edit",
                json!({
                    "path": f3,
                    "old_string": "    let a = 1;",
                    "new_string": "    let a = 1;\n    let x1 = 1;\n    let x2 = 2;"
                }),
            )]),
            reply(vec![call(
                "3",
                "hash_edit",
                json!({"path": f3, "start": format!("11:{h_brace}"), "content": "} // ext"}),
            )]),
            reply(vec![say("done")]),
        ],
        &["hashline-edit"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    s3.agent.prompt_text("external edit").await.unwrap();
    let ext_reqs = s3.requests();
    let ext_out = last(&ext_reqs[3]).text();
    assert!(ext_out.contains("changed since your last read"), "{ext_out}");
}

#[tokio::test]
async fn hashline_edit_strips_prefixes_from_hash_edit_content_and_edit_args() {
    let dir = temp_dir("sasacode-hashline-strip-");
    let file = dir.path().join("app.ts");
    std::fs::write(&file, "const a = 1;\nconst b = 2;\nconst c = 3;\n").unwrap();
    let f = path_str(&file);
    let h1 = line_hash("const a = 1;");
    let h2 = line_hash("const b = 2;");
    let h3 = line_hash("const c = 3;");

    let s = setup(
        vec![
            reply(vec![call("1", "read", json!({"path": f}))]),
            // hash_edit content with N:hh| on every non-empty line gets stripped
            reply(vec![call(
                "2",
                "hash_edit",
                json!({
                    "path": f,
                    "start": format!("1:{h1}"),
                    "end": format!("2:{h2}"),
                    "content": format!("1:{h1}|const a = 10;\n2:{h2}|const b = 20;")
                }),
            )]),
            // edit old_string & new_string with N:hh| on every non-empty line get stripped via ToolCall hook
            reply(vec![call(
                "3",
                "edit",
                json!({
                    "path": f,
                    "old_string": format!("2:{h2}|const b = 20;\n3:{h3}|const c = 3;"),
                    "new_string": format!("2:{h2}|const b = 200;\n3:{h3}|const c = 300;")
                }),
            )]),
            // Mixed prefixed + non-empty unprefixed lines must NOT be stripped
            reply(vec![call(
                "4",
                "edit",
                json!({
                    "path": f,
                    "old_string": format!("1:{h1}|const a = 10;\nconst b = 200;"),
                    "new_string": "const a = 99;\nconst b = 200;"
                }),
            )]),
            reply(vec![say("done")]),
        ],
        &["hashline-edit"],
        opts(&path_str(dir.path()), &[]),
    )
    .await;
    s.agent.prompt_text("strip prefixes").await.unwrap();
    let reqs = s.requests();
    assert!(last(&reqs[2]).text().contains("Edited"), "{}", last(&reqs[2]).text());
    assert!(last(&reqs[3]).text().contains("Edited"), "{}", last(&reqs[3]).text());
    assert!(
        last(&reqs[4]).text().contains("old_string not found"),
        "{}",
        last(&reqs[4]).text()
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "const a = 10;\nconst b = 200;\nconst c = 300;\n"
    );
}

#[tokio::test]
async fn slim_profile_cuts_hashline_and_japanese_read_output_at_line_boundaries() {
    let dir = temp_dir("sasacode-slim-hashline-");
    let jp_file = dir.path().join("jp.txt");
    let content = (1..=120)
        .map(|i| format!("日本語の行-{i:03} パディング文字列です"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&jp_file, &content).unwrap();

    let s = setup(
        vec![
            reply(vec![call("1", "read", json!({"path": path_str(&jp_file)}))]),
            reply(vec![say("ok")]),
        ],
        &["hashline-edit", "slim-profile"],
        opts(&path_str(dir.path()), &[("slim-profile", json!({"readMaxChars": 600}))]),
    )
    .await;
    s.agent.set_permissions(policy(PermissionMode::Auto));
    s.agent.prompt_text("read jp").await.unwrap();
    let reqs = s.requests();
    let out = last(&reqs[1]).text();
    assert!(out.contains("[slim-profile: truncated"), "{out}");

    let parts: Vec<&str> = out.split('\n').collect();
    let marker = parts
        .iter()
        .position(|l| l.contains("[slim-profile: truncated"))
        .expect("marker");
    // Tail starts cleanly on a complete line with full `N:hh|` prefix, never mid-prefix (e.g. `3:ab|`).
    let first_tail = parts[marker + 1];
    let (prefix, body) = first_tail.split_once('|').expect("hashline pipe in tail");
    let (num, hash) = prefix.split_once(':').expect("colon in tail");
    let n: usize = num.parse().expect("line number");
    assert!(n > 90 && hash.len() == 2, "broken tail prefix: {first_tail}");
    assert!(body.starts_with("日本語の行-"), "broken tail body: {first_tail}");
}
