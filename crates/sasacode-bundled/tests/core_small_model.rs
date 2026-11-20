//! Integration tests for the 9 small-model harness features:
//! search-tools, error-hints, action-nudge, syntax-check, context-slim,
//! thinking-budget, hashline-edit, clean-retry, and slim-profile.

mod support_core;

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::permission::PermissionPolicy;
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::SessionFile;
use sasacode_ai::{
    AssistantContent, LocalServer, Message, ModelInfo, ThinkingContent, ToolResultMessage, UserContent, register_api,
};
use sasacode_bundled::hashline_edit::line_hash;
use sasacode_bundled::syntax_check::check_rust_syntax;
use sasacode_plugin_api::{Lang, PermissionMode, PermissionRules};
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
    s3.agent.prompt_text("go").await.unwrap();
    assert_eq!(s3.requests().len(), 3);
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
async fn thinking_budget_stops_local_model_runaway_thinking_but_not_cloud_default() {
    let dir = temp_dir("sasacode-thinking-budget-");
    let run_with = |local: bool| {
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
                "id": "m", "provider": "t", "api": api, "contextWindow": 32_000, "maxOutput": 1000
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

    let (local_msgs, local_reqs) = run_with(true).await;
    assert_eq!(local_reqs.len(), 2);
    assert!(json_of(&local_reqs[1].messages).contains("[thinking-budget]"));
    assert_eq!(local_msgs.last().unwrap().text(), "concise answer");

    let (cloud_msgs, cloud_reqs) = run_with(false).await;
    assert_eq!(cloud_reqs.len(), 1);
    assert_eq!(cloud_msgs.len(), 2);
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
                json!({"command": "echo test fail 1 >&2; exit 1"}),
            )]),
            reply(vec![call(
                "4",
                "bash",
                json!({"command": "echo test fail 2 >&2; exit 1"}),
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
