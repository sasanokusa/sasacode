//! End to end (`packages/e2e/test/cli.e2e.test.ts`): a sasacode binary, as a child process, against
//! a fake OpenAI-compatible server. Each test is a thing a user does, checked by what they would see
//! (output, files, exit code) and by what the server was actually sent.
//!
//! The binary is the one `SASACODE_E2E_BIN` names (a path, absolute or from the workspace root, or a
//! command on the PATH); without it every test is skipped with a message:
//!
//!   SASACODE_E2E_BIN=target/debug/sasacode cargo test -p sasacode-testing --test cli_e2e
//!
//! Dropped from the TS suite: "a plugin installed from a local folder loads, and the model can call
//! its tool" — `sasacode plugin install` and external plugins are removed (rust/DECISIONS.md ADR-002).

use std::fs;
use std::path::Path;

use sasacode_testing::harness::{RunOptions, Sandbox, answer, e2e_bin, fake_config, parse_lines, roles};
use sasacode_testing::{FakeServer, Reply, ToolCallShape, ToolCallSpec};
use serde_json::{Value, json};

/// Skips the test (with a message) when no binary is named.
macro_rules! require_bin {
    () => {
        if e2e_bin().is_none() {
            eprintln!("skipped: SASACODE_E2E_BIN is not set");
            return;
        }
    };
}

fn setup(script: Vec<Reply>, config: Value) -> (FakeServer, Sandbox) {
    let server = FakeServer::start(script).expect("the fake server");
    let sandbox = Sandbox::new(&fake_config(server.url(), config));
    (server, sandbox)
}

fn call(name: &str, args: Value) -> ToolCallSpec {
    ToolCallSpec::new(name, args)
}

/// `JSON.stringify(request.messages.at(-1))`.
fn last_message(request: &Value) -> String {
    request["messages"]
        .as_array()
        .and_then(|m| m.last())
        .map(Value::to_string)
        .unwrap_or_default()
}

/// `/sasacode -r (\S+)/.exec(text)?.[1]`.
fn resume_id(text: &str) -> Option<String> {
    text.match_indices("sasacode -r ").find_map(|(i, m)| {
        let id: String = text[i + m.len()..].chars().take_while(|c| !c.is_whitespace()).collect();
        (!id.is_empty()).then_some(id)
    })
}

/// `text.indexOf(pattern)`: -1 when absent, so comparisons read as in the TS.
fn index_of(text: &str, pattern: &str) -> i64 {
    text.find(pattern).map_or(-1, |i| i as i64)
}

fn path_arg(p: &Path) -> String {
    p.to_str().expect("a UTF-8 temporary path").to_string()
}

const CONTROL: [&str; 4] = ["--output", "jsonl", "--control", "stdio"];

fn with_control<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter().copied().chain(CONTROL).collect()
}

fn ask() -> Value {
    json!({ "permissions": { "mode": "ask" } })
}

fn bash_then(text: &str) -> Vec<Reply> {
    vec![
        Reply::tool_calls(vec![call("bash", json!({ "command": "touch made-by-bash" }))]),
        Reply::text(text),
    ]
}

#[test]
fn a_task_that_writes_a_file_and_checks_it_runs_the_real_tools_and_r_continues_the_conversation() {
    require_bin!();
    let (server, sb) = setup(
        vec![
            Reply {
                reasoning: Some("I will write it.".into()),
                ..Reply::tool_calls(vec![call(
                    "write",
                    json!({ "path": "hello.txt", "content": "hi there\n" }),
                )])
            },
            Reply::tool_calls(vec![call("read", json!({ "path": "hello.txt" }))]),
            Reply::text("Wrote hello.txt."),
            Reply::text("It says hi there."),
        ],
        json!({ "permissions": { "mode": "edits" } }),
    );
    let first = sb.run(["-p", "make hello.txt"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    assert!(first.stdout.contains("Wrote hello.txt."), "{}", first.stdout);
    assert_eq!(fs::read_to_string(sb.project.join("hello.txt")).unwrap(), "hi there\n");
    // The model got each tool result back, in order.
    let requests = server.requests();
    assert_eq!(roles(&requests[1]), ["system", "user", "assistant", "tool"]);
    assert!(last_message(&requests[2]).contains("hi there"));

    let id = resume_id(&first.stderr);
    assert!(id.is_some(), "no `sasacode -r <id>` in: {}", first.stderr);
    let second = sb.run(["-r", &id.unwrap(), "-p", "what does it say?"]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(second.stdout.contains("It says hi there."), "{}", second.stdout);
    // The resumed run sent the whole earlier conversation.
    assert!(server.requests()[3]["messages"].to_string().contains("make hello.txt"));
}

#[test]
fn tool_calls_streamed_without_an_index_or_with_the_name_after_the_id_still_run_62() {
    require_bin!();
    let (server, sb) = setup(
        vec![
            Reply::tool_calls(vec![
                call("write", json!({ "path": "a.txt", "content": "first\n" })),
                call("write", json!({ "path": "b.txt", "content": "second\n" })),
            ])
            .with_shape(ToolCallShape::NoIndex),
            Reply::tool_calls(vec![call("read", json!({ "path": "b.txt" }))]).with_shape(ToolCallShape::NameLater),
            Reply::text("Both written."),
        ],
        json!({ "permissions": { "mode": "edits" } }),
    );
    let r = sb.run(["-p", "write a.txt and b.txt"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("Both written."), "{}", r.stdout);
    assert_eq!(fs::read_to_string(sb.project.join("a.txt")).unwrap(), "first\n");
    assert_eq!(fs::read_to_string(sb.project.join("b.txt")).unwrap(), "second\n");
    let requests = server.requests();
    assert_eq!(roles(&requests[1]), ["system", "user", "assistant", "tool", "tool"]);
    assert!(last_message(&requests[2]).contains("second"));
}

#[test]
fn a_reply_cut_off_mid_stream_is_asked_for_again_instead_of_ending_the_turn_in_silence() {
    require_bin!();
    let (server, sb) = setup(
        vec![
            Reply::reasoning("Let me think about the fi").with_cut(),
            Reply::text("The answer is 42."),
        ],
        json!({}),
    );
    let r = sb.run(["-p", "question"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("The answer is 42."), "{}", r.stdout);
    assert!(
        r.stderr.contains("the connection dropped mid-reply; asking again"),
        "{}",
        r.stderr
    );
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn a_server_that_keeps_cutting_replies_ends_the_run_with_an_error_not_a_silent_success() {
    require_bin!();
    let cut = Reply::reasoning("hm").with_cut();
    let (_server, sb) = setup(vec![cut.clone(), cut.clone(), cut.clone(), cut], json!({}));
    let r = sb.run(["-p", "question"]);
    assert_eq!(r.code, 1, "{}", r.stderr);
    assert!(
        r.stderr.contains("the stream ended before the reply was complete"),
        "{}",
        r.stderr
    );
}

#[test]
fn an_effort_the_server_refuses_inside_a_200_stream_vllm_is_dropped_and_the_request_retried() {
    require_bin!();
    let (server, sb) = setup(
        vec![
            Reply::stream_error("reasoning_effort 'none' is not supported for this model"),
            Reply::text("ok"),
        ],
        json!({}),
    );
    let r = sb.run(["-p", "hi", "--thinking", "off"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("ok"), "{}", r.stdout);
    let requests = server.requests();
    assert_eq!(requests[0]["reasoning_effort"], "none");
    assert_ne!(requests.last().unwrap()["reasoning_effort"], "none");
}

#[test]
fn a_headless_run_never_executes_a_call_that_needs_approval_and_says_so() {
    require_bin!();
    let (server, sb) = setup(bash_then("done"), ask());
    let r = sb.run(["-p", "run it"]);
    assert!(!sb.project.join("made-by-bash").exists());
    assert!(last_message(&server.requests()[1]).contains("needs user approval"));
    assert_eq!(r.code, 0, "{}", r.stderr);
}

#[test]
fn a_server_error_surfaces_as_an_error_and_exit_code_1() {
    require_bin!();
    let (_server, sb) = setup(vec![Reply::status(500)], json!({}));
    let r = sb.run(["-p", "hi"]);
    assert_eq!(r.code, 1, "{}", r.stderr);
    assert!(r.stderr.to_lowercase().contains("error"), "{}", r.stderr);
}

#[test]
fn jsonl_output_carries_the_whole_run_and_a_saved_session_is_a_file_that_c_finds_again() {
    require_bin!();
    let (_server, sb) = setup(vec![Reply::text("first"), Reply::text("second")], json!({}));
    let r = sb.jsonl("one", &[]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.events[0]["type"], "session");
    assert_eq!(answer(&r.events), "first");
    assert_eq!(r.events.last(), Some(&json!({ "type": "agent_end", "cause": "done" })));
    let sessions = fs::read_dir(sb.home.join("sessions")).unwrap().count();
    assert_eq!(sessions, 1);
    let again = sb.run(["-c", "-p", "two"]);
    assert!(again.stdout.contains("second"), "{}\n{}", again.stdout, again.stderr);
}

#[test]
fn control_stdio_the_parent_answers_an_approval_request_over_stdin_and_the_call_runs() {
    require_bin!();
    let (server, sb) = setup(bash_then("done"), ask());
    let mut run = sb.start(with_control(&["-p", "run it"]));
    let before = run.until("approval_request");
    assert_eq!(before[0]["type"], "session");
    let request = before.last().unwrap().clone();
    // toEqual: exactly these fields (`id` is compared with itself, so it may be absent).
    let mut keys: Vec<&str> = request
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "id")
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["args", "callId", "cwd", "reason", "tool", "type"], "{request}");
    assert_eq!(request["type"], "approval_request");
    assert!(
        request["callId"].is_string() && request["reason"].is_string() && request["cwd"].is_string(),
        "{request}"
    );
    assert_eq!(request["tool"], "bash");
    assert_eq!(request["args"], json!({ "command": "touch made-by-bash" }));
    assert!(!sb.project.join("made-by-bash").exists()); // nothing ran before the answer
    run.send(&json!({ "type": "approval", "id": request["id"], "decision": "allow" }));
    let rest = run.until("agent_end");
    assert_eq!(rest.last(), Some(&json!({ "type": "agent_end", "cause": "done" })));
    run.end();
    assert_eq!(run.exited(), 0, "{}", run.stderr());
    assert!(sb.project.join("made-by-bash").exists());
    assert!(!last_message(&server.requests()[1]).contains("denied"));
}

#[test]
fn control_stdio_a_deny_reaches_the_model_with_its_feedback_a_bad_line_is_reported_and_the_run_goes_on() {
    require_bin!();
    let (server, sb) = setup(bash_then("ok"), ask());
    let mut run = sb.start(with_control(&["-p", "run it"]));
    let id = run.until("approval_request").last().unwrap()["id"].clone();
    run.send(&json!("not a control message"));
    run.send(&json!({ "type": "approval", "id": id, "decision": "deny", "feedback": "use the editor instead" }));
    let rest = run.until("agent_end");
    assert_eq!(
        rest.iter().filter(|e| e["type"] == "control_error").count(),
        1,
        "{rest:?}"
    );
    assert_eq!(run.exited(), 0, "{}", run.stderr());
    assert!(!sb.project.join("made-by-bash").exists());
    assert!(last_message(&server.requests()[1]).contains("User feedback: use the editor instead"));
}

#[test]
fn control_stdio_abort_cancels_the_open_request_and_ends_the_run_as_aborted() {
    require_bin!();
    let (_server, sb) = setup(bash_then("unreachable"), ask());
    let mut run = sb.start(with_control(&["-p", "run it"]));
    let id = run.until("approval_request").last().unwrap()["id"].clone();
    run.send(&json!({ "type": "abort" }));
    let rest = run.until("agent_end");
    assert_eq!(
        rest.iter().find(|e| e["type"] == "approval_cancelled"),
        Some(&json!({ "type": "approval_cancelled", "id": id })),
        "{rest:?}"
    );
    assert_eq!(rest.last(), Some(&json!({ "type": "agent_end", "cause": "aborted" })));
    assert_eq!(run.exited(), 1, "{}", run.stderr());
    assert!(!sb.project.join("made-by-bash").exists());
}

#[test]
fn control_stdio_a_closed_stdin_denies_approvals_and_what_is_piped_is_not_added_to_the_prompt() {
    require_bin!();
    let (server, sb) = setup(bash_then("ok"), ask());
    let r = sb.run_with(
        with_control(&["-p", "run it"]),
        RunOptions {
            stdin: Some("SECRET-LOG-LINE\n".into()),
            ..RunOptions::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let events = parse_lines(&r.stdout);
    let errors: Vec<&Value> = events.iter().filter(|e| e["type"] == "control_error").collect();
    assert_eq!(errors, [&json!({ "type": "control_error", "error": "not valid JSON" })]);
    let requests = server.requests();
    assert!(!requests[0]["messages"].to_string().contains("SECRET-LOG-LINE"));
    assert!(!sb.project.join("made-by-bash").exists());
    assert!(last_message(&requests[1]).contains("the controlling process closed stdin"));
}

#[test]
fn control_needs_stdio_p_and_output_jsonl_and_says_so_before_anything_runs() {
    require_bin!();
    let (server, sb) = setup(vec![Reply::text("never asked")], json!({}));
    let cases: [(&[&str], &str); 4] = [
        (
            &["-p", "hi", "--control", "stdio"],
            "--control stdio needs -p and --output jsonl",
        ),
        (
            &["-p", "hi", "--output", "text", "--control", "stdio"],
            "--control stdio needs -p and --output jsonl",
        ),
        (
            &["--output", "jsonl", "--control", "stdio"],
            "--control stdio needs -p and --output jsonl",
        ),
        (
            &["-p", "hi", "--output", "jsonl", "--control", "tcp"],
            "--control must be stdio",
        ),
    ];
    for (args, message) in cases {
        let r = sb.run(args);
        assert_eq!(r.code, 1, "{args:?}: {}", r.stderr);
        assert!(r.stderr.contains(message), "{args:?}: {}", r.stderr);
        assert_eq!(r.stdout, "", "{args:?}");
    }
    assert!(server.requests().is_empty());
}

#[test]
fn append_system_prompt_file_adds_the_files_text_after_the_configs_instructions() {
    require_bin!();
    let (server, sb) = setup(
        vec![Reply::text("ok"), Reply::text("ok")],
        json!({ "instructions": "FROM-CONFIG" }),
    );
    let file = sb.root().join("extra.txt");
    fs::write(&file, "FROM-FILE persona: ひとこと\n").unwrap();
    let r = sb.run(["-p", "hi", "--append-system-prompt-file", &path_arg(&file)]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let system = &server.requests()[0]["messages"][0];
    assert_eq!(system["role"], "system");
    let text = match &system["content"] {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    assert!(text.contains("FROM-CONFIG"), "{text}");
    assert!(
        index_of(&text, "FROM-CONFIG") < index_of(&text, "FROM-FILE persona: ひとこと"),
        "{text}"
    );
    assert!(index_of(&text, "FROM-FILE") < index_of(&text, "Environment:"), "{text}");
    // An empty file adds nothing.
    let empty = sb.root().join("empty.txt");
    fs::write(&empty, "").unwrap();
    let r = sb.run(["-p", "hi", "--append-system-prompt-file", &path_arg(&empty)]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(!server.requests()[1]["messages"][0].to_string().contains("FROM-FILE"));
}

#[test]
fn append_system_prompt_file_with_a_file_that_cannot_be_read_fails_before_the_run() {
    require_bin!();
    let (server, sb) = setup(vec![Reply::text("never asked")], json!({}));
    for path in [path_arg(&sb.project.join("missing.txt")), path_arg(&sb.project)] {
        let r = sb.run(["-p", "hi", "--append-system-prompt-file", &path]);
        assert_eq!(r.code, 1, "{path}: {}", r.stderr);
        assert!(r.stderr.contains(&format!("failed to read {path}")), "{}", r.stderr);
    }
    assert!(server.requests().is_empty());
}

#[test]
fn m_with_a_model_the_provider_does_not_list_fails_before_anything_runs_naming_close_ones() {
    require_bin!();
    let server = FakeServer::with_models(
        vec![Reply::text("never asked"), Reply::text("ok")],
        vec![json!({ "id": "m" }), json!({ "id": "big-model" })],
    )
    .expect("the fake server");
    let sb = Sandbox::new(&fake_config(server.url(), json!({})));
    let typo = sb.run(["-m", "fake/big-modle", "-p", "hi"]);
    assert_eq!(typo.code, 1, "{}", typo.stderr);
    assert!(
        typo.stderr
            .contains(r#"fake has no model "fake/big-modle". Did you mean: fake/big-model"#),
        "{}",
        typo.stderr
    );
    let none = sb.run(["-m", "fake/nope", "-p", "hi"]);
    assert_eq!(none.code, 1, "{}", none.stderr);
    assert!(
        none.stderr.contains(r#"fake has no model "fake/nope""#),
        "{}",
        none.stderr
    );
    assert!(server.requests().is_empty());
    let sessions = sb.home.join("sessions");
    let saved: Vec<String> = if sessions.exists() {
        fs::read_dir(&sessions)
            .unwrap()
            .flat_map(|d| fs::read_dir(d.unwrap().path()).unwrap())
            .map(|f| f.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    } else {
        Vec::new()
    };
    assert_eq!(saved, Vec::<String>::new());
    // A listed model starts as before.
    let r = sb.run(["-m", "fake/big-model", "-p", "hi"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
}

#[test]
fn m_is_not_checked_when_the_provider_lists_nothing() {
    require_bin!();
    let server = FakeServer::with_models(vec![Reply::text("ok")], Vec::new()).expect("the fake server");
    let sb = Sandbox::new(&fake_config(server.url(), json!({})));
    let r = sb.run(["-m", "fake/anything", "-p", "hi"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("ok"), "{}", r.stdout);
}
