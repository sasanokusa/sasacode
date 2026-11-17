//! packages/cli/test/control.test.ts, a real run with a scripted model driven over the channel: the
//! sasacode binary with `--control stdio`, against a fake model server. The protocol tests are lib
//! tests in src/control_tests.rs.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

use sasacode_testing::{FakeServer, Reply, ToolCallSpec};

use crate::support::{Bin, fake_config};

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

/// `sasacode -p go --output jsonl --control stdio` in ask mode; `react` sees every stdout line as
/// it comes (each one JSON object: one write, one line) and may answer on stdin.
fn drive(script: Vec<Reply>, mut react: impl FnMut(&Value, &mut Channel)) -> Driven {
    let server = FakeServer::start(script).expect("the fake server");
    let bin = Bin::new(&fake_config(
        server.url(),
        json!({ "permissions": { "mode": "ask" }, "plugins": { "disabled": ["openai-codex"] } }),
    ));
    let mut child = bin
        .command(["-p", "go", "--no-session", "--output", "jsonl", "--control", "stdio"])
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
