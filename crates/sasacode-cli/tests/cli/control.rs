//! packages/cli/test/control.test.ts, a real run with a scripted model driven over the channel: the
//! sasacode binary with `--control stdio`, against a fake model server. The protocol tests are lib
//! tests in src/control_tests.rs.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use sasacode_protocol::Event;
use sasacode_protocol::event::UiAsk;
use sasacode_testing::{FakeServer, Reply, ToolCallSpec};

use crate::support::{Bin, fake_config, merged};

/// The parent's end of `--control stdio`: what it writes on sasacode's stdin.
struct Channel {
    stdin: Option<ChildStdin>,
}

impl Channel {
    /// One line: `message` as it is when it is a string, else as JSON (the TS `ch.send`).
    fn send(&mut self, message: Value) {
        let line = match message {
            Value::String(s) => s,
            other => other.to_string(),
        };
        if let Some(w) = self.stdin.as_mut() {
            // A process that stopped reading is not an error here: its output and exit code tell.
            let _ = w.write_all(format!("{line}\n").as_bytes()).and_then(|_| w.flush());
        }
    }

    /// Close sasacode's stdin.
    fn end(&mut self) {
        self.stdin = None;
    }
}

fn bash(command: &str) -> Reply {
    Reply::tool_calls(vec![ToolCallSpec::new("bash", json!({ "command": command }))])
}

/// What a run printed, the server it ran against, and its sandbox.
struct Driven {
    code: i32,
    events: Vec<Value>,
    server: FakeServer,
    bin: Bin,
}

impl Driven {
    fn types(&self) -> Vec<&str> {
        self.events.iter().map(|e| e["type"].as_str().unwrap_or("")).collect()
    }

    fn index(&self, ty: &str) -> usize {
        self.types()
            .iter()
            .position(|t| *t == ty)
            .unwrap_or_else(|| panic!("no {ty} in {:?}", self.types()))
    }

    fn of(&self, ty: &str) -> Vec<&Value> {
        self.events.iter().filter(|e| e["type"] == ty).collect()
    }

    /// `JSON.stringify(requests[n].messages.at(-1))`.
    fn last_message(&self, n: usize) -> String {
        self.server.requests()[n]["messages"]
            .as_array()
            .and_then(|m| m.last())
            .map(Value::to_string)
            .unwrap_or_default()
    }
}

/// How a run starts: its prompt, the config's top-level keys over the ask-mode default, and
/// flags added.
struct Run {
    prompt: &'static str,
    config: Value,
    args: Vec<String>,
}

impl Default for Run {
    fn default() -> Self {
        Run {
            prompt: "go",
            config: json!({}),
            args: Vec::new(),
        }
    }
}

/// `sasacode -p go --output jsonl --control stdio` in ask mode; `react` sees every stdout line as
/// it comes (each one JSON object: one write, one line) and may answer on stdin.
fn drive(script: Vec<Reply>, react: impl FnMut(&Value, &mut Channel)) -> Driven {
    drive_with(script, Run::default(), react)
}

fn drive_with(script: Vec<Reply>, run: Run, mut react: impl FnMut(&Value, &mut Channel)) -> Driven {
    let server = FakeServer::start(script).expect("the fake server");
    let config = merged(
        json!({ "permissions": { "mode": "ask" }, "plugins": { "disabled": ["openai-codex"] } }),
        run.config,
    );
    let bin = Bin::new(&fake_config(server.url(), config));
    let mut args: Vec<String> = [
        "-p",
        run.prompt,
        "--no-session",
        "--output",
        "jsonl",
        "--control",
        "stdio",
    ]
    .map(String::from)
    .to_vec();
    args.extend(run.args);
    let mut child = bin
        .command(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sasacode");
    let mut ch = Channel {
        stdin: child.stdin.take(),
    };
    let (tx, lines) = mpsc::channel::<String>();
    let stdout = BufReader::new(child.stdout.take().expect("its stdout"));
    std::thread::spawn(move || {
        for line in stdout.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let mut stderr = child.stderr.take().expect("its stderr");
    let stderr = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut events = Vec::new();
    loop {
        match lines.recv_timeout(Duration::from_secs(60)) {
            Ok(line) => {
                let e: Value =
                    serde_json::from_str(&line).unwrap_or_else(|err| panic!("not one JSON line ({err}): {line}"));
                react(&e, &mut ch);
                events.push(e);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                panic!("no line from sasacode in 60 s");
            }
        }
    }
    let code = child.wait().expect("its exit").code().unwrap_or(-1);
    let stderr = stderr.join().unwrap_or_default();
    assert!(!events.is_empty(), "{stderr}");
    Driven {
        code,
        events,
        server,
        bin,
    }
}

#[test]
fn allow_the_call_runs_a_bad_line_in_between_is_reported_and_does_not_stop_the_run() {
    let d = drive(vec![bash("touch allowed"), Reply::text("done")], |e, ch| {
        if e["type"] != "approval_request" {
            return;
        }
        ch.send(json!("this is not json"));
        ch.send(json!({ "type": "approval", "id": e["id"], "decision": "allow" }));
    });
    assert_eq!(d.code, 0);
    let asked = d.of("approval_request");
    assert_eq!(asked.len(), 1);
    // The call the model made: its id is the server's.
    let call = d
        .events
        .iter()
        .find(|e| e["type"] == "message_end" && e["message"]["role"] == "assistant")
        .map(|e| e["message"]["content"][0].clone())
        .expect("the model's call");
    assert_eq!(call["type"], "tool_call");
    assert!(asked[0]["reason"].is_string());
    assert_eq!(
        asked[0],
        &json!({
            "type": "approval_request",
            "id": asked[0]["id"],
            "callId": call["id"],
            "tool": "bash",
            "args": { "command": "touch allowed" },
            "reason": asked[0]["reason"],
            "cwd": d.bin.cwd().to_string_lossy(),
            "rules": ["bash(touch allowed)"],
        })
    );
    assert_eq!(
        d.of("control_error"),
        [&json!({ "type": "control_error", "error": "not valid JSON" })]
    );
    assert!(d.bin.sb.project.join("allowed").exists());
    assert!(d.index("approval_request") < d.index("tool_start"));
    assert_eq!(d.events.last(), Some(&json!({ "type": "agent_end", "cause": "done" })));
}

#[test]
fn deny_with_feedback_the_call_does_not_run_and_the_model_is_told_why() {
    let d = drive(
        vec![bash("touch denied"), Reply::text("ok, not doing that")],
        |e, ch| {
            if e["type"] == "approval_request" {
                ch.send(json!({ "type": "approval", "id": e["id"], "decision": "deny", "feedback": "not now" }));
            }
        },
    );
    assert_eq!(d.code, 0);
    assert!(!d.bin.sb.project.join("denied").exists());
    assert!(
        d.last_message(1)
            .contains("The user denied this tool call. User feedback: not now"),
        "{}",
        d.last_message(1)
    );
    assert!(d.of("tool_start").is_empty());
}

#[test]
fn always_allows_the_same_call_again_without_asking() {
    let d = drive(
        vec![bash("touch allowed"), bash("touch allowed"), Reply::text("done")],
        |e, ch| {
            if e["type"] == "approval_request" {
                ch.send(json!({ "type": "approval", "id": e["id"], "decision": "always" }));
            }
        },
    );
    assert_eq!(d.code, 0);
    assert_eq!(d.of("approval_request").len(), 1);
    assert_eq!(d.of("tool_start").len(), 2);
}

#[test]
fn abort_while_a_request_is_open_approval_cancelled_the_call_does_not_run_the_run_ends_as_aborted() {
    let mut id = Value::Null;
    let d = drive(vec![bash("touch cancelled"), Reply::text("unreachable")], |e, ch| {
        if e["type"] != "approval_request" {
            return;
        }
        id = e["id"].clone();
        ch.send(json!({ "type": "abort" }));
    });
    assert_eq!(d.code, 1);
    assert_eq!(
        d.of("approval_cancelled"),
        [&json!({ "type": "approval_cancelled", "id": id })]
    );
    assert!(d.index("approval_cancelled") < d.index("agent_end"));
    assert_eq!(
        d.events.last(),
        Some(&json!({ "type": "agent_end", "cause": "aborted" }))
    );
    assert!(!d.bin.sb.project.join("cancelled").exists());
}

#[test]
fn stdin_closing_mid_run_denies_the_open_request_and_the_next_one_the_run_still_finishes() {
    let d = drive(
        vec![bash("touch never"), bash("touch never"), Reply::text("gave up")],
        |e, ch| {
            if e["type"] == "approval_request" {
                ch.end();
            }
        },
    );
    assert_eq!(d.code, 0); // not aborted: the model got the denials and answered
    assert_eq!(d.of("approval_request").len(), 1);
    assert!(!d.bin.sb.project.join("never").exists());
    for n in [1, 2] {
        assert!(
            d.last_message(n)
                .contains("User feedback: the controlling process closed stdin"),
            "{}",
            d.last_message(n)
        );
    }
    assert_eq!(d.events.last(), Some(&json!({ "type": "agent_end", "cause": "done" })));
}

#[test]
fn a_message_sent_mid_run_reaches_the_model_at_its_next_turn() {
    let mut sent = false;
    let d = drive(vec![bash("touch allowed"), Reply::text("done")], |e, ch| {
        if e["type"] != "approval_request" {
            return;
        }
        ch.send(json!({ "type": "message", "text": "also say hello" }));
        ch.send(json!({ "type": "approval", "id": e["id"], "decision": "allow" }));
        sent = true;
    });
    assert!(sent);
    assert_eq!(d.code, 0);
    let second = d.server.requests()[1]["messages"].to_string();
    assert!(second.contains("also say hello"), "{second}");
    // At its next turn: not in the first request.
    assert!(
        !d.server.requests()[0]["messages"]
            .to_string()
            .contains("also say hello")
    );
}

// ── what the desktop GUI needs beyond approvals: edited arguments, stopping one call, rules that
// outlast a run (--allow), the plugins' questions, notices, status and commands, slash commands
// from -p (--slash), and the agent mode's judge (packages/cli/test/control-run.test.ts) ──

/// The line as the GUI reads it: each new kind of line is checked against the protocol crate.
fn read(e: &Value) -> Event {
    serde_json::from_value(e.clone()).unwrap_or_else(|err| panic!("{err}: {e}"))
}

fn plugins(enabled: &[&str]) -> Value {
    json!({ "disabled": ["openai-codex"], "enabled": enabled })
}

#[test]
fn edited_args_the_call_runs_with_them_the_model_is_told_and_the_approval_request_named_the_rule_always_adds() {
    let d = drive(vec![bash("touch original"), Reply::text("done")], |e, ch| {
        if e["type"] == "approval_request" {
            let answer = json!({ "type": "approval", "id": e["id"], "decision": "allow", "args": { "command": "touch edited" } });
            ch.send(answer);
        }
    });
    assert_eq!(d.code, 0);
    let Event::ApprovalRequest { rules, .. } = read(d.of("approval_request")[0]) else {
        panic!("not an approval_request")
    };
    assert_eq!(rules, ["bash(touch original)"]);
    assert!(d.bin.sb.project.join("edited").exists());
    assert!(!d.bin.sb.project.join("original").exists());
    let told = r#"[the user edited the arguments; the call ran with {\"command\":\"touch edited\"}]"#;
    assert!(d.last_message(1).contains(told), "{}", d.last_message(1));
}

#[test]
fn args_sent_back_unchanged_run_the_call_as_asked_the_loop_guard_does_not_take_it_for_a_repeat() {
    let d = drive(vec![bash("touch original"), Reply::text("done")], |e, ch| {
        if e["type"] == "approval_request" {
            ch.send(json!({ "type": "approval", "id": e["id"], "decision": "allow", "args": e["args"] }));
        }
    });
    assert_eq!(d.code, 0);
    assert!(d.bin.sb.project.join("original").exists());
    assert!(!d.last_message(1).contains("the user edited the arguments"));
}

#[test]
fn edited_args_that_do_not_fit_the_tool_or_that_a_deny_rule_matches_are_a_denial_with_the_reason() {
    let mut answers = vec![json!({ "command": "rm -rf original" }), json!({ "command": 5 })];
    let run = Run {
        config: json!({ "permissions": { "mode": "ask", "deny": ["bash(rm *)"] } }),
        ..Run::default()
    };
    let script = vec![bash("touch original"), bash("touch original"), Reply::text("ok")];
    let d = drive_with(script, run, |e, ch| {
        if e["type"] == "approval_request" {
            ch.send(json!({ "type": "approval", "id": e["id"], "decision": "allow", "args": answers.pop() }));
        }
    });
    assert_eq!(d.code, 0);
    assert!(d.of("tool_start").is_empty());
    assert!(
        d.last_message(1).contains("The user's edited arguments were refused ("),
        "{}",
        d.last_message(1)
    );
    assert!(d.last_message(1).contains("command"));
    assert!(
        d.last_message(2).contains("rule deny: bash(rm *)"),
        "{}",
        d.last_message(2)
    );
}

#[test]
fn always_with_edited_args_allows_the_edited_call_from_then_on() {
    let script = vec![bash("touch original"), bash("touch edited"), Reply::text("done")];
    let d = drive(script, |e, ch| {
        if e["type"] == "approval_request" {
            let answer = json!({ "type": "approval", "id": e["id"], "decision": "always", "args": { "command": "touch edited" } });
            ch.send(answer);
        }
    });
    assert_eq!(d.of("approval_request").len(), 1);
    assert_eq!(d.of("tool_start").len(), 2);
    assert!(!d.bin.sb.project.join("original").exists());
}

#[test]
fn allow_with_an_approval_requests_rules_the_next_run_allows_the_same_call_without_asking() {
    let mut rules: Vec<String> = Vec::new();
    drive(vec![bash("touch allowed"), Reply::text("done")], |e, ch| {
        if let Event::ApprovalRequest { rules: r, .. } = read(e) {
            rules = r;
            ch.send(json!({ "type": "approval", "id": e["id"], "decision": "always" }));
        }
    });
    assert_eq!(rules, ["bash(touch allowed)"]);
    let run = Run {
        args: rules.iter().flat_map(|r| ["--allow".to_string(), r.clone()]).collect(),
        ..Run::default()
    };
    let d = drive_with(vec![bash("touch allowed"), Reply::text("done")], run, |_, _| {});
    assert_eq!(d.code, 0);
    assert!(!d.types().contains(&"approval_request"));
    assert!(d.bin.sb.project.join("allowed").exists());
}

#[test]
fn abort_with_a_call_id_stops_that_call_alone_its_result_is_an_error_and_the_run_goes_on() {
    let started = std::time::Instant::now();
    let run = Run {
        config: json!({ "permissions": { "mode": "auto" } }),
        ..Run::default()
    };
    let d = drive_with(vec![bash("sleep 20"), Reply::text("carried on")], run, |e, ch| {
        if e["type"] == "tool_start" {
            ch.send(json!({ "type": "abort", "callId": e["call"]["id"] }));
        }
    });
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    assert_eq!(d.code, 0);
    assert_eq!(d.of("tool_end")[0]["result"]["isError"], true);
    let told = "[The user stopped this tool call; the rest of the run goes on.]";
    assert!(d.last_message(1).contains(told), "{}", d.last_message(1));
    assert_eq!(d.events.last(), Some(&json!({ "type": "agent_end", "cause": "done" })));
}

fn ask_color() -> Reply {
    let question = json!({ "question": "Color?", "options": [{ "label": "Red" }, { "label": "Blue" }] });
    Reply::tool_calls(vec![ToolCallSpec::new("ask_user", json!({ "questions": [question] }))])
}

#[test]
fn a_plugins_question_reaches_the_parent_and_its_answer_the_tool_the_plugins_commands_come_first() {
    let run = Run {
        config: json!({ "permissions": { "mode": "auto" }, "plugins": plugins(&["ask-user"]) }),
        ..Run::default()
    };
    let d = drive_with(vec![ask_color(), Reply::text("done")], run, |e, ch| {
        if e["type"] == "ui_request" {
            ch.send(json!({ "type": "ui_response", "id": e["id"], "value": "1" }));
        }
    });
    assert_eq!(d.code, 0);
    let Event::UiRequest { id, ask, title } = read(d.of("ui_request")[0]) else {
        panic!("not a ui_request")
    };
    assert_eq!((id.as_str(), title.as_str()), ("ui-1", "Color?"));
    let UiAsk::Select { options } = ask else {
        panic!("not a select: {ask:?}")
    };
    let offered: Vec<(&str, &str)> = options.iter().map(|o| (o.value.as_str(), o.label.as_str())).collect();
    assert_eq!(offered[..2], [("0", "Red"), ("1", "Blue")]);
    assert!(d.last_message(1).contains("Blue"), "{}", d.last_message(1));
    let Event::Commands { commands } = read(d.of("commands")[0]) else {
        panic!("not commands")
    };
    let undo = commands.iter().find(|c| c.name == "undo").expect("/undo is listed");
    assert_eq!(undo.owner, "checkpoints");
    assert!(d.index("commands") < d.index("agent_start"));
}

#[test]
fn abort_while_a_plugins_question_is_open_ui_cancelled_the_question_is_dismissed_and_the_run_ends_as_aborted() {
    let run = Run {
        config: json!({ "permissions": { "mode": "auto" }, "plugins": plugins(&["ask-user"]) }),
        ..Run::default()
    };
    let d = drive_with(vec![ask_color(), Reply::text("unreachable")], run, |e, ch| {
        if e["type"] == "ui_request" {
            ch.send(json!({ "type": "abort" }));
        }
    });
    assert_eq!(d.code, 1);
    assert_eq!(d.of("ui_cancelled"), [&json!({ "type": "ui_cancelled", "id": "ui-1" })]);
    assert!(d.index("ui_cancelled") < d.index("agent_end"));
    assert_eq!(
        d.events.last(),
        Some(&json!({ "type": "agent_end", "cause": "aborted" }))
    );
}

fn slash(prompt: &'static str) -> Run {
    Run {
        prompt,
        args: vec!["--slash".into()],
        ..Run::default()
    }
}

#[test]
fn slash_runs_a_plugin_command_what_it_says_is_a_line_and_no_model_is_asked() {
    let d = drive_with(Vec::new(), slash("/undo list"), |_, _| {});
    assert_eq!(d.code, 0);
    let said = json!({ "type": "plugin_notify", "level": "info", "text": "[undo] Nothing to undo" });
    assert!(d.events.contains(&said), "{:?}", d.events);
    assert!(matches!(read(&said), Event::PluginNotify { .. }));
    assert!(!d.types().contains(&"agent_start"));
    assert!(d.server.requests().is_empty());
}

#[test]
fn slash_a_run_the_command_starts_runs_to_its_end_an_unknown_command_is_an_error_line_and_exit_1() {
    let started = drive_with(vec![Reply::text("on it")], slash("/goal write the tests"), |_, _| {});
    assert_eq!(started.code, 0);
    assert_eq!(
        started.events.last(),
        Some(&json!({ "type": "agent_end", "cause": "done" }))
    );
    assert!(
        started.server.requests()[0]["messages"]
            .to_string()
            .contains("write the tests")
    );
    let status = started.of("plugin_status");
    assert!(
        status
            .iter()
            .any(|e| matches!(read(e), Event::PluginStatus { key, text: Some(_) } if key == "goal:goal")),
        "{status:?}"
    );
    let unknown = drive_with(Vec::new(), slash("/nope x"), |_, _| {});
    assert_eq!(unknown.code, 1);
    assert!(
        unknown
            .events
            .contains(&json!({ "type": "error", "error": "unknown command: /nope" }))
    );
    // Without --slash the same prompt goes to the model as it always did.
    let plain = drive_with(
        vec![Reply::text("a reply")],
        Run {
            prompt: "/undo list",
            ..Run::default()
        },
        |_, _| {},
    );
    assert_eq!(plain.code, 0);
    assert!(
        plain.server.requests()[0]["messages"]
            .to_string()
            .contains("/undo list")
    );
}

#[test]
fn slash_in_text_mode_the_commands_notices_go_to_stderr() {
    let server = FakeServer::start(Vec::new()).expect("the fake server");
    let bin = Bin::new(&fake_config(
        server.url(),
        json!({ "plugins": { "disabled": ["openai-codex"] } }),
    ));
    let r = bin.run(["-p", "/undo list", "--slash", "--no-session"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stderr.contains("[undo] Nothing to undo"), "{}", r.stderr);
    assert!(server.requests().is_empty());
}

#[test]
fn agent_mode_the_judges_verdict_is_a_judged_line() {
    let run = Run {
        config: json!({ "permissions": { "mode": "agent" } }),
        ..Run::default()
    };
    let verdict = Reply::text(r#"{"safe": true, "reason": "creates one file here"}"#);
    let d = drive_with(vec![bash("touch judged"), verdict, Reply::text("done")], run, |_, _| {});
    assert_eq!(d.code, 0);
    let call = d.of("tool_start")[0]["call"]["id"].clone();
    let judged = json!({
        "type": "judged", "callId": call, "tool": "bash", "verdict": "allow",
        "reason": "agent judged safe: creates one file here",
    });
    assert_eq!(d.of("judged"), [&judged]);
    assert!(matches!(read(&judged), Event::Judged { .. }));
    assert!(d.of("approval_request").is_empty());
    assert!(d.bin.sb.project.join("judged").exists());
}

#[test]
fn allow_must_be_a_permission_rule_and_slash_needs_p() {
    let bin = Bin::new(&json!({}));
    let bad = bin.run(["-p", "x", "--allow", "bash(ls)", "--allow", "(oops)"]);
    assert_eq!(bad.code, 1);
    assert!(
        bad.stderr.contains(r#"--allow "(oops)" is not a permission rule"#),
        "{}",
        bad.stderr
    );
    let slash = bin.run(["--slash"]);
    assert_eq!(slash.code, 1);
    assert!(slash.stderr.contains("--slash needs -p"), "{}", slash.stderr);
}
