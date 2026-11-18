//! `packages/bundled/test/repair.test.ts`. The file the tool-repair run reads is a temp fixture
//! instead of the test's own source.

mod support_core;

use serde_json::{Value, json};

use sasacode_ai::{AssistantContent, Message, StopReason};
use sasacode_bundled::repetition_guard::{RepetitionLimits, detect_repetition};
use sasacode_bundled::tool_repair::{
    FitOptions, TOOL_SYNONYMS, closest_name, fit_to_schema, repair_json, resolve_tool_name,
};
use support_core::*;

fn ok(raw: &str) -> Option<Value> {
    repair_json(raw).map(|r| Value::Object(r.value))
}

#[test]
fn repair_json_the_usual_small_model_mistakes() {
    assert_eq!(ok(r#"{"a": 1,}"#), Some(json!({"a": 1})));
    assert_eq!(ok(r#"{"a": [1, 2,],}"#), Some(json!({"a": [1, 2]})));
    assert_eq!(
        ok("{path: 'a.txt', recursive: True, x: None}"),
        Some(json!({"path": "a.txt", "recursive": true, "x": null}))
    );
    assert_eq!(ok(r#"{"a": {"b": 1"#), Some(json!({"a": {"b": 1}})));
    assert_eq!(
        ok("{\"content\": \"line1\nline2"),
        Some(json!({"content": "line1\nline2"}))
    );
    assert_eq!(ok("```json\n{\"a\": 1}\n```"), Some(json!({"a": 1})));
    assert_eq!(
        ok(r#"Sure! Here are the arguments: {"a": 1} hope that helps"#),
        Some(json!({"a": 1}))
    );
    let double = serde_json::to_string(&serde_json::to_string(&json!({"a": 1})).unwrap()).unwrap();
    assert_eq!(ok(&double), Some(json!({"a": 1})));
    assert_eq!(
        ok(r#"{'text': 'it\'s "quoted"'}"#),
        Some(json!({"text": "it's \"quoted\""}))
    );
    // Text inside strings is never touched.
    assert_eq!(ok(r#"{"a": "x, } True: y",}"#), Some(json!({"a": "x, } True: y"})));
    assert_eq!(repair_json("no json here"), None);
    assert_eq!(repair_json("[1, 2]"), None); // arguments must be an object
    assert_eq!(
        repair_json(r#"{"a": 1,}"#).unwrap().fixes,
        vec!["removed trailing comma"]
    );
}

fn params(name: &str) -> Value {
    sasacode_tools::builtin_tools()
        .into_iter()
        .find(|t| t.name() == name)
        .map(|t| t.parameters())
        .unwrap()
}

fn fit(input: Value, schema: &Value, drop_unknown: bool) -> (Value, Vec<String>) {
    let r = fit_to_schema(&obj(input), schema, FitOptions { drop_unknown });
    (Value::Object(r.value), r.fixes)
}

#[test]
fn fit_to_schema_rename_convert_and_drop_only_when_unambiguous() {
    let (read, edit, write) = (params("read"), params("edit"), params("write"));
    // The TS wrote `offset: 3.0`, which is the number 3 in JS.
    assert_eq!(
        fit(json!({"file": "a.txt", "limit": "20", "offset": 3}), &read, false),
        (
            json!({"path": "a.txt", "limit": 20, "offset": 3}),
            vec![
                r#"argument "file" → "path""#.to_string(),
                r#"limit: "20" → 20"#.to_string()
            ]
        )
    );
    assert_eq!(
        fit(
            json!({"path": "a", "old": "x", "new": "y", "replace_all": "TRUE"}),
            &edit,
            false
        )
        .0,
        json!({"path": "a", "old_string": "x", "new_string": "y", "replace_all": true})
    );
    // Unknown extra argument with no candidate: dropped only when allowed (read-only tools) and reported.
    assert_eq!(
        fit(json!({"path": "a", "encoding": "utf8"}), &read, true),
        (
            json!({"path": "a"}),
            vec![r#"dropped unknown argument "encoding""#.to_string()]
        )
    );
    // On a tool that changes things it may be a constraint the model meant: kept for validation to report.
    assert_eq!(
        fit(json!({"file": "a", "content": "x", "overwrite": false}), &write, false),
        (
            json!({"path": "a", "content": "x", "overwrite": false}),
            vec![r#"argument "file" → "path""#.to_string()]
        )
    );
    // Anthropic's text editor calls the content file_text.
    assert_eq!(
        fit(json!({"path": "a", "file_text": "x"}), &write, false).0,
        json!({"path": "a", "content": "x"})
    );
    // "20abc" is not a number: left for validation to report.
    assert_eq!(
        fit(json!({"path": "a", "limit": "20abc"}), &read, false).0,
        json!({"path": "a", "limit": "20abc"})
    );
    // Nested objects, arrays and enums.
    let schema = json!({
        "type": "object",
        "properties": {
            "todos": {
                "type": "array",
                "items": {"type": "object", "properties": {"content": {"type": "string"}, "status": {"type": "string", "enum": ["pending", "done"]}}},
            },
        },
    });
    assert_eq!(
        fit(
            json!({"todos": r#"[{"content": 1, "status": "Done"}]"#}),
            &schema,
            false
        )
        .0,
        json!({"todos": [{"content": "1", "status": "done"}]})
    );
    assert_eq!(
        fit(json!({"todos": {"content": "x", "status": "pending"}}), &schema, false).0,
        json!({"todos": [{"content": "x", "status": "pending"}]})
    );
}

const NO_SYNONYMS: &[(&str, &[&str])] = &[];

#[test]
fn closest_name_unique_matches_only() {
    let tools = ["read", "write", "edit", "bash", "web_fetch"];
    let c = |n: &str| closest_name(n, &tools, NO_SYNONYMS, true);
    assert_eq!(c("raed").as_deref(), Some("read"));
    assert_eq!(c("Bash").as_deref(), Some("bash"));
    assert_eq!(c("webFetch").as_deref(), Some("web_fetch"));
    assert_eq!(c("rite").as_deref(), Some("write"));
    assert_eq!(c("xyz"), None);
    assert_eq!(closest_name("ab", &["aa", "bb"], NO_SYNONYMS, true), None); // two equally close candidates
}

fn tool_call_of(m: &Message, i: usize) -> Value {
    match m {
        Message::Assistant(a) => serde_json::to_value(&a.content[i]).unwrap(),
        _ => panic!("not an assistant message"),
    }
}

#[tokio::test]
async fn tool_repair_plugin_a_broken_call_from_a_small_model_runs_and_the_model_learns_what_was_fixed() {
    let dir = temp_dir("sasacode-repair-");
    std::fs::write(
        dir.path().join("repair.test.ts"),
        "import { expect, test } from \"bun:test\";\n// more\n",
    )
    .unwrap();
    let s = run(
        vec![
            reply_at(
                vec![raw_call("1", "functions.Read", "{file: 'repair.test.ts', limit: '3',}")],
                1,
            ),
            reply_at(vec![say("done")], 2),
        ],
        "tool-repair",
        &path_str(dir.path()),
    )
    .await;
    let req = &s.requests()[1];
    let (assistant, result) = (&req.messages[1], &req.messages[2]);
    assert_eq!(
        tool_call_of(assistant, 0),
        json!({"type": "tool_call", "id": "1", "name": "read", "input": {"path": "repair.test.ts", "limit": 3}})
    );
    let text = json_of(result);
    assert!(text.contains("harness repaired this call"), "{text}");
    assert!(text.contains(r#"tool \"functions.Read\" → \"read\""#), "{text}");
    assert!(text.contains("import { expect, test }"), "{text}");
    assert!(matches!(result, Message::Tool(t) if !t.is_error));
}

fn limits() -> RepetitionLimits {
    RepetitionLimits {
        min_repeats: 4.0,
        min_span: 200.0,
        min_unit: 10.0,
        short_min_span: 400.0,
    }
}

#[test]
fn detect_repetition_loops_are_found_ordinary_text_and_short_runs_are_not() {
    let l = limits();
    let lp = format!("Intro text. {}", "I will now check the file again. ".repeat(8));
    let r = detect_repetition(&lp, &l).unwrap();
    assert_eq!((r.unit.as_str(), r.count), ("I will now check the file again. ", 8));
    assert_eq!(detect_repetition(&"x".repeat(300), &l), None); // a short unit needs a longer run …
    assert_eq!(detect_repetition(&"x".repeat(500), &l).unwrap().unit, "x"); // … but is caught then
    let normal = format!(
        "{}{}",
        "A normal paragraph that goes on for a while without repeating itself. ",
        "abc".repeat(10)
    );
    assert_eq!(detect_repetition(&normal, &l), None);
    assert_eq!(detect_repetition(&"same line here\n".repeat(3), &l), None); // too few copies
}

fn assistant(m: &Message) -> &sasacode_ai::AssistantMessage {
    m.as_assistant()
        .unwrap_or_else(|| panic!("not an assistant message: {m:?}"))
}

#[tokio::test]
async fn repetition_guard_stops_the_loop_keeps_one_copy_and_tells_the_model() {
    let looping = format!("Let me think. {}", "I should read the config file first. ".repeat(30));
    let s = run(
        vec![
            reply_at(vec![say(&looping)], 111),
            reply_at(vec![say("Reading it now.")], 222),
        ],
        "repetition-guard",
        &path_str(&std::env::temp_dir()),
    )
    .await;
    let messages = s.agent.messages();
    let kept = assistant(&messages[1]);
    assert_eq!(kept.stop_reason, StopReason::Stopped);
    assert_eq!(
        kept.content,
        vec![say("Let me think. I should read the config file first. ")]
    );
    assert!(json_of(&s.requests()[1].messages[2]).contains("kept repeating the same text"));
    assert_eq!(
        assistant(messages.last().unwrap()).content,
        vec![say("Reading it now.")]
    );
}

#[tokio::test]
async fn repetition_guard_a_run_of_newlines_is_cut_where_it_starts_at_the_end_not_at_the_first_paragraph_break() {
    let answer = "## Findings\n\nFirst, src/config.ts parses the port as a string, which is the bug.\n\nSecond, the fix is to wrap it in Number().\n\nThird, a test for it goes in config.test.ts.\n";
    let s = run(
        vec![
            reply_at(vec![say(&format!("{answer}{}", "\n".repeat(900)))], 333),
            reply_at(vec![say("Done.")], 444),
        ],
        "repetition-guard",
        &path_str(&std::env::temp_dir()),
    )
    .await;
    let messages = s.agent.messages();
    let kept = assistant(&messages[1]);
    assert_eq!(kept.stop_reason, StopReason::Stopped);
    assert_eq!(kept.content, vec![say(answer)]);
}

#[test]
fn detect_repetition_short_units_need_a_long_run_code_and_tables_do_not_trip_it() {
    let l = limits();
    let r = detect_repetition(&"なるほど。".repeat(100), &l).unwrap();
    assert_eq!((r.unit.as_str(), r.count), ("なるほど。", 100));
    assert_eq!(
        detect_repetition(&format!("見出し\n{}", "なるほど。".repeat(30)), &l),
        None
    ); // 150 chars: not yet
    let mut rows = vec!["| a | b |".to_string(), "| --- | --- |".to_string()];
    rows.extend((0..40).map(|i| format!("| row {i} | {} |", i * 7)));
    assert_eq!(detect_repetition(&rows.join("\n"), &l), None);
    let code: String = (0..60).map(|i| format!("  const v{i} = values[{i}] ?? 0;\n")).collect();
    assert_eq!(detect_repetition(&code, &l), None);
    let banner = format!("{}\nTitle\n{}", "=".repeat(80), "=".repeat(80));
    assert_eq!(detect_repetition(&banner, &l), None);
}

#[tokio::test]
async fn tool_repair_keeps_a_mutating_tools_unknown_argument_so_the_model_is_asked_again() {
    let dir = temp_dir("sasacode-repair-");
    let s = run(
        vec![
            reply_at(
                vec![call(
                    "1",
                    "write",
                    json!({"path": "never-written.txt", "content": "x", "overwrite": false}),
                )],
                11,
            ),
            reply_at(vec![say("ok")], 12),
        ],
        "tool-repair",
        &path_str(dir.path()),
    )
    .await;
    let result = json_of(last(&s.requests()[1]));
    assert!(
        result.contains("input.overwrite is not an allowed property"),
        "{result}"
    );
    assert!(!dir.path().join("never-written.txt").exists());
}

fn norm(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect()
}

#[test]
fn closest_name_tool_names_other_harnesses_use_only_to_a_tool_that_is_offered() {
    let tools = ["read", "write", "edit", "bash", "web_fetch"];
    let said = [
        ("read_file", "read"),
        ("view_file", "read"),
        ("cat", "read"),
        ("write_file", "write"),
        ("create_file", "write"),
        ("edit_file", "edit"),
        ("str_replace", "edit"),
        ("str_replace_editor", "edit"),
        ("replace_in_file", "edit"),
        ("run_command", "bash"),
        ("execute_command", "bash"),
        ("execute_bash", "bash"),
        ("run_shell_command", "bash"),
        ("shell", "bash"),
        ("terminal", "bash"),
    ];
    for (name, meant) in said {
        assert_eq!(
            (name, closest_name(name, &tools, TOOL_SYNONYMS, true)),
            (name, Some(meant.to_string()))
        );
    }
    assert_eq!(closest_name("read_file", &tools, NO_SYNONYMS, true), None); // too far for a typo: only the table knows it
    assert_eq!(
        closest_name("shell", &["read", "write", "edit"], TOOL_SYNONYMS, true),
        None
    ); // bash is not offered
    assert_eq!(
        closest_name("view", &["read", "view"], TOOL_SYNONYMS, true).as_deref(),
        Some("view")
    ); // a tool of that very name wins
    let two: &[(&str, &[&str])] = &[("a", &["x"]), ("b", &["x"])];
    assert_eq!(closest_name("x", &["a", "b"], two, true), None); // listed under two: no fix
    // Each spelling belongs to one tool and is no tool's own name, or it would never resolve.
    let spellings: Vec<String> = TOOL_SYNONYMS
        .iter()
        .flat_map(|(_, l)| l.iter().map(|s| norm(s)))
        .collect();
    let unique: std::collections::HashSet<&String> = spellings.iter().collect();
    assert_eq!(unique.len(), spellings.len());
    let keys: Vec<&str> = TOOL_SYNONYMS.iter().map(|(k, _)| *k).collect();
    assert!(spellings.iter().all(|s| !keys.contains(&s.as_str())));
}

#[test]
fn resolve_tool_name_an_mcp_tools_own_name_wins_over_a_builtins_and_typos_only_when_allowed() {
    let names = [
        "read",
        "write",
        "bash",
        "mcp__github__get_file_contents",
        "mcp__filesystem__read_file",
    ];
    assert_eq!(
        resolve_tool_name("get_file_contents", &names, true).as_deref(),
        Some("mcp__github__get_file_contents")
    );
    assert_eq!(resolve_tool_name("get_file_contents", &["read", "bash"], true), None); // GitHub's name, not one for read
    assert_eq!(
        resolve_tool_name("read_file", &names, true).as_deref(),
        Some("mcp__filesystem__read_file")
    );
    assert_eq!(resolve_tool_name("read_file", &["read"], true).as_deref(), Some("read"));
    assert_eq!(
        resolve_tool_name("read_file", &["read", "mcp__a__read_file", "mcp__b__read_file"], true),
        None
    ); // two such tools
    assert_eq!(resolve_tool_name("head", &names, true).as_deref(), Some("read"));
    assert_eq!(resolve_tool_name("head", &names, false), None);
    assert_eq!(
        resolve_tool_name("functions.read_file", &["read"], false).as_deref(),
        Some("read")
    );
}

#[tokio::test]
async fn tool_repair_another_harnesss_tool_and_argument_names_run_as_the_builtin_tool() {
    let dir = temp_dir("sasacode-repair-");
    let file = path_str(&dir.path().join("a.ts"));
    std::fs::write(&file, "const a = 1;\nconst b = 2;\n").unwrap();
    let c = |id: i64, name: &str, input: Value| reply_at(vec![call(&id.to_string(), name, input)], id);
    let s = run(
        vec![
            c(1, "read_file", json!({"file_path": file})),
            c(
                2,
                "str_replace",
                json!({"path": file, "old_str": "const a = 1;", "new_str": "const a = 10;"}),
            ),
            c(
                3,
                "str_replace_editor",
                json!({"command": "str_replace", "path": file, "old_str": "const b = 2;", "new_str": "const b = 20;"}),
            ),
            c(4, "str_replace_editor", json!({"command": "view", "path": file})),
            // The command that picks the tool can be inside broken JSON: it is repaired first.
            reply_at(
                vec![raw_call(
                    "5",
                    "str_replace_editor",
                    &format!("{{command: 'view', path: {},}}", serde_json::to_string(&file).unwrap()),
                )],
                5,
            ),
            c(6, "run_shell_command", json!({"command": "echo shell-ran"})),
            reply_at(vec![say("done")], 7),
        ],
        "tool-repair",
        &path_str(dir.path()),
    )
    .await;
    let requests = s.requests();
    let results: Vec<&sasacode_ai::ToolResultMessage> = requests[1..]
        .iter()
        .map(|r| match last(r) {
            Message::Tool(t) => t,
            m => panic!("not a tool result: {m:?}"),
        })
        .collect();
    assert_eq!(results.iter().map(|r| r.is_error).collect::<Vec<_>>(), vec![false; 6]);
    let last_text = |i: usize| {
        results[i]
            .content
            .last()
            .and_then(|c| c.as_text())
            .unwrap_or_default()
            .to_string()
    };
    assert!(last_text(0).contains("const a = 1;"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "const a = 10;\nconst b = 20;\n"
    );
    assert!(
        results[2].content[0]
            .as_text()
            .unwrap()
            .contains(r#"tool "str_replace_editor" (command "str_replace") → "edit""#)
    );
    assert!(last_text(3).contains("const b = 20;"));
    assert!(last_text(4).contains("const b = 20;"));
    assert!(json_of(results[5]).contains("shell-ran"));
    // The history keeps each call as it ran: the model sees the built-in names and arguments.
    let history: Vec<AssistantContent> = requests
        .last()
        .unwrap()
        .messages
        .iter()
        .flat_map(|m| match m {
            Message::Assistant(a) => a.content.clone(),
            _ => Vec::new(),
        })
        .collect();
    let names: Vec<String> = history
        .iter()
        .filter_map(|c| c.as_tool_call().map(|t| t.name.clone()))
        .collect();
    assert_eq!(names, vec!["read", "edit", "edit", "read", "read", "bash"]);
    let input = |i: usize| Value::Object(history[i].as_tool_call().unwrap().input.clone());
    assert_eq!(input(2)["path"], json!(file));
    assert_eq!(input(2)["old_string"], json!("const b = 2;"));
    assert_eq!(input(2)["new_string"], json!("const b = 20;"));
    assert!(input(2).get("command").is_none());
    assert_eq!(input(4), json!({"path": file}));
}
