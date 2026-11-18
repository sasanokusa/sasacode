use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sasacode_protocol::control::CONTROL_TYPES;
use sasacode_protocol::event::EVENT_TYPES;
use sasacode_protocol::session::{legacy_name, session_dir_of_real_path};
use sasacode_protocol::*;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn repo(path: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(path);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        // A Windows checkout may have CRLF line ends; the markers below look for LF.
        .replace("\r\n", "\n")
}

fn run_events() -> Vec<Event> {
    std::fs::read_to_string(fixture("run.jsonl"))
        .unwrap()
        .lines()
        .map(|l| parse_event(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect()
}

/// The `x` of each `<pattern>x"` in a source, between `after` and `until`.
fn source_types(source: &str, pattern: &str, after: &str, until: &str) -> BTreeSet<String> {
    let start = source.find(after).unwrap_or_else(|| panic!("{after} not found"));
    let rest = &source[start..];
    let rest = &rest[..rest.find(until).unwrap_or(rest.len())];
    let mut out = BTreeSet::new();
    for piece in rest.split(pattern).skip(1) {
        if let Some(end) = piece.find('"') {
            out.insert(piece[..end].to_owned());
        }
    }
    out
}

#[test]
fn the_session_line_says_the_context_window_when_the_cli_knows_it() {
    let line = r#"{"type":"session","id":"a","path":"/p","contextWindow":200000}"#;
    assert!(matches!(
        sasacode_protocol::parse_event(line).unwrap(),
        Event::Session {
            context_window: Some(200_000),
            ..
        }
    ));
    // An older CLI's line has none.
    let line = r#"{"type":"session","id":null,"path":null}"#;
    assert!(matches!(
        sasacode_protocol::parse_event(line).unwrap(),
        Event::Session {
            context_window: None,
            ..
        }
    ));
}

#[test]
fn every_line_of_a_real_run_parses_as_a_known_event() {
    let events = run_events();
    assert!(events.iter().all(|e| !matches!(e, Event::Unknown(_))), "{events:#?}");
    assert!(matches!(
        &events[0],
        Event::Session {
            id: Some(_),
            path: Some(_),
            ..
        }
    ));
    assert!(matches!(
        events.last(),
        Some(Event::AgentEnd {
            cause: StopCause::Done,
            ..
        })
    ));
}

#[test]
fn a_real_run_carries_the_prompt_the_stream_the_approval_and_the_tool() {
    let events = run_events();
    let prompt = events.iter().find_map(|e| match e {
        Event::MessageEnd {
            message: Message::User { content, .. },
        } => Some(text_of(content)),
        _ => None,
    });
    assert_eq!(prompt.as_deref(), Some("ファイルを作って"));
    let streamed: String = events
        .iter()
        .filter_map(|e| match e {
            Event::MessageUpdate {
                event: StreamDelta::TextDelta { delta, .. },
            } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        streamed,
        "ファイルを作ります。**完了**しました。`made.txt` を作成しています。"
    );
    let approval = events.iter().find_map(|e| match e {
        Event::ApprovalRequest { id, tool, args, .. } => Some((id.clone(), tool.clone(), args.clone())),
        _ => None,
    });
    let (id, tool, args) = approval.expect("an approval_request");
    assert_eq!((id.as_str(), tool.as_str()), ("approval-1", "bash"));
    assert_eq!(args["command"], "echo hi > made.txt");
    assert!(events.iter().any(|e| matches!(e, Event::ControlError { .. })));
    let ended = events.iter().find_map(|e| match e {
        Event::ToolEnd { call, result, .. } => Some((call.name.clone(), result.is_error)),
        _ => None,
    });
    assert_eq!(ended, Some(("bash".to_owned(), false)));
    let thinking = events.iter().any(|e| {
        matches!(e, Event::MessageEnd { message: Message::Assistant(a) }
            if a.content.iter().any(|c| matches!(c, Content::Thinking { thinking, .. } if thinking == "まずファイルを作る。")))
    });
    assert!(thinking);
}

#[test]
fn an_unknown_type_is_kept_and_a_known_type_with_a_bad_shape_is_an_error() {
    assert!(matches!(parse_event(r#"{"type":"from_the_future","x":1}"#), Ok(Event::Unknown(v)) if v["x"] == 1));
    assert!(parse_event(r#"{"type":"turn_start"}"#).is_err());
    assert!(parse_event("not json").is_err());
}

#[test]
fn the_session_log_of_that_run_restores_its_conversation() {
    let entries = read_entries(&fixture("session.jsonl")).unwrap();
    let SessionEntry::Session { version, .. } = &entries[0] else {
        panic!("no header")
    };
    assert_eq!(*version, session::SESSION_VERSION);
    let restored = restore(&entries);
    assert_eq!(restored.model.as_deref(), Some("fake/m"));
    let roles: Vec<&str> = restored
        .messages
        .iter()
        .map(|m| match m {
            Message::User { .. } => "user",
            Message::Assistant(_) => "assistant",
            Message::Tool(_) => "tool",
            Message::Other => "other",
        })
        .collect();
    assert_eq!(roles, ["user", "assistant", "tool", "assistant"]);
    let summary = session::summarize(&fixture("session.jsonl")).unwrap();
    assert_eq!(summary.first_prompt, "ファイルを作って");
    assert_eq!(summary.message_count, 4);
}

#[test]
fn the_session_folder_is_named_as_the_typescript_side_names_it() {
    let events = run_events();
    let Event::Session { path: Some(log), .. } = &events[0] else {
        panic!()
    };
    let cwd = events.iter().find_map(|e| match e {
        Event::ApprovalRequest { cwd, .. } => Some(cwd.clone()),
        _ => None,
    });
    let log = Path::new(log);
    let home = log.parent().unwrap().parent().unwrap().parent().unwrap();
    assert_eq!(session_dir_of_real_path(home, &cwd.unwrap()), log.parent().unwrap());
    assert_eq!(legacy_name("/a/b"), "a-b");
    assert_eq!(legacy_name(r"C:\Users\x"), "C-Users-x");
}

#[test]
fn control_messages_are_one_line_each_in_the_shape_the_cli_reads() {
    let allow = ControlMessage::Approval {
        id: "approval-1".into(),
        decision: Decision::Allow,
        feedback: None,
        args: None,
    };
    assert_eq!(
        allow.to_line(),
        "{\"type\":\"approval\",\"id\":\"approval-1\",\"decision\":\"allow\"}\n"
    );
    let deny = ControlMessage::Approval {
        id: "a".into(),
        decision: Decision::Deny,
        feedback: Some("理由".into()),
        args: None,
    };
    assert_eq!(
        deny.to_line(),
        "{\"type\":\"approval\",\"id\":\"a\",\"decision\":\"deny\",\"feedback\":\"理由\"}\n"
    );
    assert_eq!(
        ControlMessage::Message {
            text: "テストも見て".into()
        }
        .to_line(),
        "{\"type\":\"message\",\"text\":\"テストも見て\"}\n"
    );
    assert_eq!(
        ControlMessage::Abort { call_id: None }.to_line(),
        "{\"type\":\"abort\"}\n"
    );
}

// The CLI's sources are the source of truth: a type added there must be added here.

/// The variants of `pub enum AgentEvent` as serde names them (`tag = "type"`, snake_case).
fn event_variants(source: &str) -> BTreeSet<String> {
    let start = source
        .find("pub enum AgentEvent {")
        .expect("pub enum AgentEvent not found");
    let body = &source[start..];
    let body = &body[..body.find("\n}\n").expect("end of AgentEvent not found")];
    let mut out = BTreeSet::new();
    for line in body.lines().skip(1) {
        let Some(rest) = line
            .strip_prefix("    ")
            .filter(|l| l.starts_with(|c: char| c.is_ascii_uppercase()))
        else {
            continue;
        };
        let mut name = String::new();
        for c in rest.chars().take_while(char::is_ascii_alphanumeric) {
            if c.is_ascii_uppercase() && !name.is_empty() {
                name.push('_');
            }
            name.push(c.to_ascii_lowercase());
        }
        out.insert(name);
    }
    out
}

#[test]
fn every_agent_event_and_control_line_the_cli_writes_is_known() {
    let mut written = event_variants(&repo("crates/sasacode-agent/src/events.rs"));
    assert!(
        written.contains("tool_start") && written.contains("agent_end"),
        "{written:?}"
    );
    // `json!({ "type": "…" })` in the files that write lines of their own (the doc comments write
    // `{"type":…}` without the space and are not counted).
    for file in [
        "crates/sasacode-cli/src/control.rs",
        "crates/sasacode-cli/src/headless.rs",
    ] {
        written.extend(source_types(&repo(file), "\"type\": \"", "", "\u{0}"));
    }
    assert!(
        written.contains("approval_request") && written.contains("session"),
        "{written:?}"
    );
    let known: BTreeSet<String> = EVENT_TYPES.iter().map(|s| s.to_string()).collect();
    let missing: Vec<_> = written.difference(&known).collect();
    assert!(
        missing.is_empty(),
        "event types the CLI writes but this crate does not know: {missing:?}"
    );
}

#[test]
fn every_control_message_the_cli_reads_is_known() {
    let source = repo("crates/sasacode-cli/src/control.rs");
    let mut read = BTreeSet::new();
    for piece in source
        .split("kind.and_then(Value::as_str) == Some(\"")
        .skip(1)
        .chain(source.split("kind.and_then(Value::as_str) != Some(\"").skip(1))
    {
        read.insert(piece[..piece.find('"').unwrap()].to_owned());
    }
    let known: BTreeSet<String> = CONTROL_TYPES.iter().map(|s| s.to_string()).collect();
    assert_eq!(read, known);
}

#[test]
fn the_session_format_version_matches() {
    let source = repo("crates/sasacode-agent/src/session.rs");
    assert!(source.contains(&format!("SESSION_VERSION: u32 = {};", session::SESSION_VERSION)));
}

#[test]
fn every_line_of_every_recorded_run_parses_as_a_known_event() {
    for name in ["work", "stop", "cancel", "error", "dropped", "turns", "loop"] {
        let raw = std::fs::read_to_string(fixture(&format!("{name}.jsonl"))).unwrap();
        for line in raw.lines() {
            let event = parse_event(line).unwrap_or_else(|e| panic!("{name}: {line}: {e}"));
            assert!(!matches!(event, Event::Unknown(_)), "{name}: {line}");
        }
    }
}

#[test]
fn a_session_log_keeps_the_repairs_and_the_usage_outside_the_conversation() {
    let entries = read_entries(&fixture("work-session.jsonl")).unwrap();
    let repairs: Vec<&str> = entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::ToolRepair { note, .. } => Some(note.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(repairs, [r#"tool "read_file" → "read"; argument "file" → "path""#]);
    let outside: Vec<(&str, u64)> = entries
        .iter()
        .filter_map(|e| match e {
            SessionEntry::Usage { usage, source, .. } => Some((source.as_str(), usage.input)),
            _ => None,
        })
        .collect();
    assert_eq!(outside, [("subagent", 100), ("subagent", 100)]);
}
