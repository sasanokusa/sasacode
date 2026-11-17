//! packages/bundled/test/write-guard.test.ts: write-guard: Gemma4の計測（#68）で write がファイルを
//! 壊した2つの形を、実際の呼び出しの形で再現する。Each test has its own temp root.

mod support_core;

use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_ai::{AssistantMessage, Message, register_api};
use sasacode_bundled::write_guard::strip_line_numbers;
use sasacode_plugin_api::{Lang, PermissionMode, Tool, ToolContext};
use sasacode_testing::{ReplayOptions, ReplayProvider};
use sasacode_tools::ReadTool;
use support_core::*;

fn write(id: &str, path: &str, content: &str) -> AssistantMessage {
    reply_at(vec![call(id, "write", json!({ "path": path, "content": content }))], 0)
}

struct Result {
    text: String,
    is_error: bool,
}

/// The writes run one per response through the real tools, tool-repair and write-guard; the results the model got.
async fn run(root: &Path, writes: Vec<AssistantMessage>, settings: Value) -> Vec<Result> {
    let mut script = writes;
    script.push(reply_at(vec![say("done")], 0));
    let api = api_name();
    register_api(&api, Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api))));
    let cwd = path_str(root);
    let mut o = AgentOptions::new(model(&api, 100_000, 1000), cwd.clone(), "");
    o.permissions = Some(policy(PermissionMode::Auto));
    let agent = Agent::new(o);
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd,
        settings: Some(Arc::new(move |n: &str| {
            if n == "write-guard" {
                settings.clone()
            } else {
                json!({})
            }
        })),
        register_provider: None,
        disabled_tools: Vec::new(),
        lang: Lang::Ja,
    });
    host.set_ui(Some(Arc::new(NoticeUi::default())));
    assert!(host.load("builtin-tools", builtin_tools).await);
    assert!(host.load("tool-repair", bundled("tool-repair")).await);
    assert!(host.load("write-guard", bundled("write-guard")).await);
    let _ = agent.prompt_text("go").await;
    agent
        .messages()
        .iter()
        .filter_map(|m| match m {
            Message::Tool(t) => Some(Result {
                text: t
                    .content
                    .iter()
                    .map(|c| c.as_text().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("\n"),
                is_error: t.is_error,
            }),
            _ => None,
        })
        .collect()
}

async fn numbered(root: &Path, path: &str) -> String {
    let r = ReadTool
        .execute(obj(json!({ "path": path })), ToolContext::new(root))
        .await
        .unwrap();
    r.content[0].as_text().unwrap().to_string()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

#[tokio::test]
async fn content_copied_from_read_with_its_line_numbers_written_without_them_and_the_model_is_told() {
    let root = temp_dir("sasacode-write-guard-");
    let file = root.path().join("permission.test.ts");
    let lines: Vec<String> = (0..108)
        .map(|i| {
            if i % 9 == 4 {
                String::new()
            } else {
                format!("line {}\tafter a tab", i + 1)
            }
        })
        .collect();
    let text = format!("{}\n", lines.join("\n"));
    std::fs::write(&file, text.replacen("line 2\t", "old 2\t", 1)).unwrap();
    // As Gemma4 e2b sent it: read's output, the last line's padding one space too wide.
    let content = numbered(root.path(), &path_str(&file))
        .await
        .replacen("old 2", "line 2", 1)
        .replacen("   108\t", "    108\t", 1);
    let results = run(root.path(), vec![write("1", "permission.test.ts", &content)], json!({})).await;
    assert!(
        results[0]
            .text
            .contains("removed read's line numbers from content (108 lines)"),
        "{}",
        results[0].text
    );
    assert!(!results[0].is_error);
    // read does not show the final newline, so the content had none.
    assert_eq!(read(&file), text[..text.len() - 1]);
}

#[tokio::test]
async fn a_fragment_over_a_long_file_is_refused_the_same_write_again_goes_through() {
    let root = temp_dir("sasacode-write-guard-");
    let file = root.path().join("edit.ts");
    // The TS edit tool's source, the file Gemma4 was editing when it sent this write.
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/edit.ts");
    let text = read(&source);
    std::fs::write(&file, &text).unwrap();
    // Gemma4 e4b's write: 5 lines of edit.ts's 81, one of them changed.
    let fragment = "    const diff = lineDiff(before, after);\n    return {\n      content: [text(`Edited ${path} (1 replacement).`)],\n      details: { path, changed: true, diff },\n    };";
    let results = run(
        root.path(),
        vec![write("1", "edit.ts", fragment), write("2", "edit.ts", fragment)],
        json!({}),
    )
    .await;
    let (first, again) = (&results[0], &results[1]);
    assert!(first.is_error);
    let lines = text.split('\n').count() - 1;
    assert!(
        first.text.contains(&format!(
            "write-guard: edit.ts has {lines} lines and this content has 5"
        )),
        "{}",
        first.text
    );
    assert!(first.text.contains("use edit"), "{}", first.text);
    assert!(again.text.contains("Overwrote"), "{}", again.text);
    assert_eq!(read(&file), fragment);
}

#[tokio::test]
async fn ordinary_writes_pass_short_files_moderate_shrinking_new_files_a_tsv_that_counts_in_its_first_column() {
    let root = temp_dir("sasacode-write-guard-");
    let lines = |n: usize| (0..n).map(|i| format!("row {i}")).collect::<Vec<_>>().join("\n");
    std::fs::write(root.path().join("short.txt"), lines(19)).unwrap();
    std::fs::write(root.path().join("long.txt"), lines(40)).unwrap();
    let tsv = "1\tAlice\n2\tBob\n3\tCarol\n";
    let results = run(
        root.path(),
        vec![
            write("1", "short.txt", "x"),
            write("2", "long.txt", &lines(20)),
            write("3", "data.tsv", tsv),
        ],
        json!({}),
    )
    .await;
    assert_eq!(
        results.iter().map(|r| r.is_error).collect::<Vec<_>>(),
        vec![false, false, false]
    );
    assert!(
        !results
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join(",")
            .contains("harness repaired")
    );
    assert_eq!(read(&root.path().join("data.tsv")), tsv);
}

#[tokio::test]
async fn line_numbers_that_do_not_count_up_a_line_dropped_or_added_among_them_are_refused_not_written() {
    let root = temp_dir("sasacode-write-guard-");
    let content = "     1\tone\n     2\ttwo\n     4\tfour\nadded\n     5\tfive";
    let results = run(
        root.path(),
        vec![write("1", "gap.txt", content), write("2", "gap.txt", content)],
        json!({}),
    )
    .await;
    assert!(
        results[0]
            .text
            .contains("write-guard: 4 of content's 5 lines start with a line number as read shows it"),
        "{}",
        results[0].text
    );
    assert!(!results[1].is_error);
    assert_eq!(read(&root.path().join("gap.txt")), content);
}

#[tokio::test]
async fn each_guard_can_be_turned_off() {
    let root = temp_dir("sasacode-write-guard-");
    std::fs::write(root.path().join("off.txt"), vec!["x"; 30].join("\n")).unwrap();
    let results = run(
        root.path(),
        vec![
            write("1", "off.txt", "y"),
            write("2", "nums.txt", "     1\ta\n     2\tb"),
        ],
        json!({ "shrink": false, "lineNumbers": false }),
    )
    .await;
    assert_eq!((results[0].is_error, results[1].is_error), (false, false));
    assert_eq!(read(&root.path().join("nums.txt")), "     1\ta\n     2\tb");
}

#[test]
fn strip_line_numbers_blank_lines_with_or_without_their_tab_crlf_a_final_newline_anything_else_is_left_alone() {
    assert_eq!(
        strip_line_numbers("     7\ta\r\n     8\t\r\n     9\n    10\tb\n").as_deref(),
        Some("a\r\n\r\n\nb\n")
    );
    assert_eq!(strip_line_numbers("     1\ta\nb"), None);
    assert_eq!(strip_line_numbers("1\ta\n2\tb"), None);
    assert_eq!(strip_line_numbers("     2\ta\n     1\tb"), None);
    assert_eq!(strip_line_numbers(""), None);
}

#[test]
fn strip_line_numbers_indented_numbers_with_no_tab_anywhere_are_the_files_text_92() {
    for s in [
        " 1\n 2\n 3\n",
        "  10\n  11\n  12\n",
        "    1\n    2\n",
        " 7\n",
        "       7\n",
    ] {
        assert_eq!((s, strip_line_numbers(s)), (s, None));
    }
    assert_eq!(strip_line_numbers("  1\tfoo\n").as_deref(), Some("foo\n"));
    assert_eq!(
        strip_line_numbers("     1\ta\n     2\n     3\tb\n").as_deref(),
        Some("a\n\nb\n")
    ); // a blank line among numbered ones
}
