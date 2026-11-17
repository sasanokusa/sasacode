//! packages/bundled/test/text-tool-calls.test.ts. Each test has its own temp root holding `a.ts`;
//! the replay api name is unique per run.

mod support_core;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use sasacode_agent::agent::{Agent, AgentOptions};
use sasacode_agent::plugins::{PluginHost, PluginHostOptions};
use sasacode_agent::session::{SessionEntry, SessionFile};
use sasacode_agent::tool_exec::{ApprovalAnswer, ApprovalDecision, ApprovalRequest, ApproveFn};
use sasacode_ai::{
    AssistantContent, AssistantMessage, Message, Request, StopReason, ToolResultMessage, now_ms, register_api,
};
use sasacode_bundled::text_tool_calls::{TextCall, TextCalls, extract_text_calls};
use sasacode_plugin_api::{Lang, PermissionMode};
use sasacode_testing::{ReplayOptions, ReplayProvider};
use support_core::*;

/// The test's root with `a.ts` in it; `at` is the file's path as a JSON string.
struct Fixture {
    root: tempfile::TempDir,
    file: String,
    at: String,
}

fn fixture() -> Fixture {
    let root = temp_dir("sasacode-text-calls-");
    let file = path_str(&root.path().join("a.ts"));
    std::fs::write(&file, "export const answer = 42;\n").unwrap();
    let at = serde_json::to_string(&file).unwrap();
    Fixture { root, file, at }
}

impl Fixture {
    fn cwd(&self) -> String {
        path_str(self.root.path())
    }
}

/// text-tool-calls.test.ts `reply`: empty usage, the time now.
fn msg(content: Vec<AssistantContent>) -> AssistantMessage {
    reply_at(content, now_ms())
}

fn say_msg(text: &str) -> AssistantMessage {
    msg(vec![say(text)])
}

fn block(v: Value) -> AssistantContent {
    serde_json::from_value(v).unwrap()
}

struct RunOptions {
    /// Where the model is served: a server on this machine unless given; `Some(None)`: no baseUrl.
    base_url: Option<Option<String>>,
    mode: PermissionMode,
    settings: Value,
    approve: Option<ApproveFn>,
    /// Replies after the first, before "done" (a judge's answer).
    then: Vec<AssistantMessage>,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            base_url: None,
            mode: PermissionMode::Auto,
            settings: json!({}),
            approve: None,
            then: Vec::new(),
        }
    }
}

struct Ran {
    requests: Vec<Request>,
    stored: AssistantMessage,
    results: Vec<ToolResultMessage>,
    entries: Vec<SessionEntry>,
}

const DEFAULT: &[&str] = &["text-tool-calls"];

async fn run(fx: &Fixture, first: AssistantMessage, plugins: &[&str], opts: RunOptions) -> Ran {
    let mut script = vec![first];
    script.extend(opts.then);
    script.push(say_msg("done"));
    let api = api_name();
    let provider = Arc::new(ReplayProvider::new(script, ReplayOptions::api(&api)));
    register_api(&api, provider.clone());
    let cwd = fx.cwd();
    let session = SessionFile::create(&fx.root.path().join("sessions"), &cwd, Vec::new()).unwrap();
    let mut m = model(&api, 100_000, 1000);
    m.base_url = match opts.base_url {
        None => Some("http://127.0.0.1:8080/v1".into()),
        Some(b) => b,
    };
    let mut o = AgentOptions::new(m, cwd.clone(), "");
    o.permissions = Some(policy(opts.mode));
    o.session = Some(session.clone());
    o.approve = opts.approve;
    let agent = Agent::new(o);
    let settings = opts.settings;
    let host = PluginHost::new(PluginHostOptions {
        agent: agent.clone(),
        cwd,
        settings: Some(Arc::new(move |p: &str| {
            if p == "text-tool-calls" {
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
    for p in plugins {
        assert!(host.load(p, bundled(p)).await, "{p}");
    }
    let _ = agent.prompt_text("go").await;
    let messages = agent.messages();
    let stored = match &messages[1] {
        Message::Assistant(a) => a.clone(),
        other => panic!("not an assistant message: {other:?}"),
    };
    let results = messages
        .iter()
        .filter_map(|m| match m {
            Message::Tool(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    let (_, entries) = SessionFile::open(session.path(), Vec::new()).unwrap();
    Ran {
        requests: provider.requests(),
        stored,
        results,
        entries,
    }
}

fn tc(name: &str, args: &str) -> TextCall {
    TextCall {
        name: name.into(),
        args: args.into(),
    }
}

fn calls(text: &str) -> Option<Vec<TextCall>> {
    extract_text_calls(text).map(|f| f.calls)
}

#[test]
fn extract_text_calls_the_three_formats_and_only_calls_that_end_the_reply() {
    assert_eq!(
        extract_text_calls(
            "Let me look.\n<tool_call>\n{\"name\": \"read\", \"arguments\": {\"path\": \"a.ts\"}}\n</tool_call>"
        ),
        Some(TextCalls {
            prose: "Let me look.".into(),
            calls: vec![tc("read", r#"{"path":"a.ts"}"#)],
            skipped: 0
        })
    );
    // Several in a row; Hermes puts the arguments first; a missing closing tag at the very end.
    assert_eq!(
        calls(
            "<tool_call>{\"arguments\": {\"path\": \"a\"}, \"name\": \"read\"}</tool_call>\n<tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}"
        ),
        Some(vec![tc("read", r#"{"path":"a"}"#), tc("bash", r#"{"command":"ls"}"#)])
    );
    assert_eq!(
        extract_text_calls(r#"<function=read>{"path": "a.ts"}</function>"#),
        Some(TextCalls {
            prose: String::new(),
            calls: vec![tc("read", r#"{"path": "a.ts"}"#)],
            skipped: 0
        })
    );
    assert_eq!(
        calls("Reading it.\n```json\n{\"name\": \"read\", \"parameters\": {\"path\": \"a.ts\"}}\n```\n"),
        Some(vec![tc("read", r#"{"path":"a.ts"}"#)])
    );
    // Broken JSON: the arguments are cut out as written, for tool-repair.
    assert_eq!(
        calls("<tool_call>{\"name\": \"read\", \"arguments\": {path: 'a.ts',}}</tool_call>"),
        Some(vec![tc("read", "{path: 'a.ts',}")])
    );
    assert_eq!(
        calls(r#"<tool_call>{"arguments": {"path": "a",}, "name": "read"}</tool_call>"#),
        Some(vec![tc("read", r#"{"path": "a",}"#)])
    );
    // A Python-style envelope (single quotes around the name too) is repaired as a whole.
    assert_eq!(
        calls("<tool_call>{'name': 'read', 'arguments': {'path': 'a.ts'}}</tool_call>"),
        Some(vec![tc("read", r#"{"path":"a.ts"}"#)])
    );
    // A closing tag missing before the next call does not swallow that call.
    assert_eq!(
        calls(
            "<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}\n<tool_call>{\"name\": \"bash\", \"arguments\": {\"command\": \"ls\"}}</tool_call>"
        ),
        Some(vec![tc("read", r#"{"path":"a"}"#), tc("bash", r#"{"command":"ls"}"#)])
    );
    // A tag inside the content of a call that is picked up belongs to that content.
    let nested = json!({ "path": "t.md", "content": "<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>" });
    assert_eq!(
        calls(&format!(
            "```json\n{}\n```",
            json!({ "name": "write", "arguments": nested })
        )),
        Some(vec![tc("write", &nested.to_string())])
    );
    // … also an opening tag alone, in a call written in the same form.
    let mention = json!({ "path": "README.md", "content": "Models write <tool_call> tags." });
    assert_eq!(
        calls(&format!(
            "<tool_call>{}</tool_call>",
            json!({ "name": "write", "arguments": mention })
        )),
        Some(vec![tc("write", &mention.to_string())])
    );
    assert_eq!(
        calls(r#"<function=write>{"path": "a.md", "content": "use <function=name> syntax"}</function>"#),
        Some(vec![tc(
            "write",
            r#"{"path": "a.md", "content": "use <function=name> syntax"}"#
        )])
    );
    // A call followed by more prose stays text when a later one ends the reply; it is counted.
    let r = extract_text_calls(
        "Read a.\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>\nAnd b.\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"b\"}}</tool_call>",
    )
    .unwrap();
    assert_eq!(r.calls, vec![tc("read", r#"{"path":"b"}"#)]);
    assert_eq!(r.skipped, 1);
    // Examples in a code block or inline code are not counted, nor a call inside another one.
    let a = r#"{"name": "read", "arguments": {"path": "a"}}"#;
    let last = format!("<tool_call>{a}</tool_call>");
    assert_eq!(
        extract_text_calls(&format!("Example:\n```json\n{a}\n```\nNow:\n{last}"))
            .unwrap()
            .skipped,
        0
    );
    assert_eq!(
        extract_text_calls(&format!("See `<tool_call>{a}</tool_call>` then\n{last}"))
            .unwrap()
            .skipped,
        0
    );
    let inner = json!({ "path": "t.md", "content": r#"<function=read>{"path": "a"}</function>"# });
    assert_eq!(
        extract_text_calls(&format!(
            "Write it.\n<tool_call>{}</tool_call>\nThen:\n{last}",
            json!({ "name": "write", "arguments": inner })
        ))
        .unwrap()
        .skipped,
        1
    );
    // Not calls: an example followed by more text, one shown in a code block, a tool definition,
    // a JSON answer after a long explanation, and a call that cannot be read.
    assert_eq!(
        extract_text_calls(
            "Write <tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call> and it runs."
        ),
        None
    );
    assert_eq!(
        extract_text_calls(
            "The format:\n```\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>\n```"
        ),
        None
    );
    // … also when the code block is never closed.
    assert_eq!(
        extract_text_calls("```\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>"),
        None
    );
    assert_eq!(
        extract_text_calls("```xml\n<function=read>{\"path\": \"a\"}</function>"),
        None
    );
    assert_eq!(
        extract_text_calls(
            "```json\n{\"name\": \"read\", \"description\": \"Read a file\", \"parameters\": {\"type\": \"object\"}}\n```"
        ),
        None
    );
    assert_eq!(
        extract_text_calls(&format!(
            "{}\n```json\n{{\"name\": \"read\", \"arguments\": {{\"path\": \"a\"}}}}\n```",
            "Here is how the protocol works. ".repeat(12)
        )),
        None
    );
    assert_eq!(extract_text_calls("<tool_call>{not json at all}</tool_call>"), None);
    assert_eq!(extract_text_calls("Just an answer."), None);
}

fn formats(at: &str) -> HashMap<&'static str, String> {
    HashMap::from([
        (
            "<tool_call>",
            format!(
                "I'll read it.\n<tool_call>\n{{\"name\": \"read\", \"arguments\": {{\"path\": {at}}}}}\n</tool_call>"
            ),
        ),
        (
            "```json",
            format!("I'll read it.\n```json\n{{\"name\": \"read\", \"parameters\": {{\"path\": {at}}}}}\n```"),
        ),
        (
            "<function=>",
            format!("I'll read it.\n<function=read>{{\"path\": {at}}}</function>"),
        ),
    ])
}

fn text_of_block(c: &AssistantContent) -> Option<&str> {
    match c {
        AssistantContent::Text(t) => Some(&t.text),
        _ => None,
    }
}

fn result_text(r: &ToolResultMessage, i: usize) -> String {
    r.content.get(i).and_then(|c| c.as_text()).unwrap_or("").to_string()
}

fn has_custom(entries: &[SessionEntry]) -> bool {
    entries
        .iter()
        .any(|e| matches!(e, SessionEntry::Custom { plugin, .. } if plugin == "text-tool-calls"))
}

/// text-tool-calls: a `format` call in the reply runs, and the model is told to call natively.
async fn a_call_in_the_reply_runs(format: &str) {
    let fx = fixture();
    let text = formats(&fx.at)[format].clone();
    let ran = run(&fx, say_msg(&text), DEFAULT, RunOptions::default()).await;
    assert_eq!(ran.requests.len(), 2);
    assert_eq!(ran.stored.stop_reason, StopReason::ToolUse);
    assert_eq!(ran.stored.content.len(), 2);
    assert_eq!(text_of_block(&ran.stored.content[0]), Some("I'll read it."));
    let AssistantContent::ToolCall(c) = &ran.stored.content[1] else {
        panic!("not a call: {:?}", ran.stored.content)
    };
    let hex = c.id.strip_prefix("textcall").unwrap_or("");
    assert!(
        hex.len() == 16 && hex.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()),
        "{}",
        c.id
    );
    assert_eq!(c.name, "read");
    assert_eq!(Value::Object(c.input.clone()), json!({ "path": fx.file }));
    assert_eq!(ran.results.len(), 1);
    assert!(!ran.results[0].is_error);
    assert!(result_text(&ran.results[0], 0).contains("call tools with native tool calls"));
    assert!(json_of(&ran.results[0].content).contains("export const answer = 42;"));
    // The history the model sees next holds the native call; the session keeps what it wrote.
    match &ran.requests[1].messages[1] {
        Message::Assistant(a) => assert_eq!(a.content, ran.stored.content),
        other => panic!("{other:?}"),
    }
    let wanted = json!({ "text": text, "calls": [c.id] });
    assert!(
        ran.entries
            .iter()
            .any(|e| matches!(e, SessionEntry::Custom { plugin, kind, data }
            if plugin == "text-tool-calls" && kind == "text_tool_call" && *data == wanted)),
        "{:?}",
        ran.entries
    );
}

#[tokio::test]
async fn a_tool_call_tag_call_in_the_reply_runs_and_the_model_is_told_to_call_natively() {
    a_call_in_the_reply_runs("<tool_call>").await;
}

#[tokio::test]
async fn a_json_fence_call_in_the_reply_runs_and_the_model_is_told_to_call_natively() {
    a_call_in_the_reply_runs("```json").await;
}

#[tokio::test]
async fn a_function_tag_call_in_the_reply_runs_and_the_model_is_told_to_call_natively() {
    a_call_in_the_reply_runs("<function=>").await;
}

#[tokio::test]
async fn broken_json_and_another_harnesss_names_in_a_text_call_are_mended_by_tool_repair() {
    let fx = fixture();
    let text = format!(
        "<tool_call>{{\"name\": \"read_file\", \"arguments\": {{file_path: {}, limit: '1',}}}}</tool_call>",
        fx.at.replace('"', "'")
    );
    let ran = run(
        &fx,
        say_msg(&text),
        &["tool-repair", "text-tool-calls"],
        RunOptions::default(),
    )
    .await;
    assert_eq!(ran.stored.content.len(), 1);
    let AssistantContent::ToolCall(c) = &ran.stored.content[0] else {
        panic!("{:?}", ran.stored.content)
    };
    assert_eq!(c.name, "read");
    assert_eq!(Value::Object(c.input.clone()), json!({ "path": fx.file, "limit": 1 }));
    assert!(!ran.results[0].is_error);
    let note = result_text(&ran.results[0], 0);
    assert!(note.contains(r#"tool "read_file" → "read""#), "{note}");
    assert!(note.contains(r#"argument "file_path" → "path""#), "{note}");
    assert!(note.contains("call tools with native tool calls"), "{note}");
    assert!(json_of(&ran.results[0].content).contains("export const answer = 42;"));
    assert!(
        ran.entries
            .iter()
            .any(|e| matches!(e, SessionEntry::ToolRepair { original, .. }
            if original.name == "read_file" && original.input == json!({})
                && original.raw_input.as_deref().is_some_and(|r| r.contains("limit: '1',")))),
        "{:?}",
        ran.entries
    );
}

#[tokio::test]
async fn a_write_cut_off_where_the_server_stopped_on_a_closing_tag_in_its_content_does_not_run_82() {
    let fx = fixture();
    let out = fx.root.path().join("cut.txt");
    std::fs::write(&out, "before\n").unwrap();
    let content = "Example:\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>\nDone.";
    let whole = format!(
        "<tool_call>{}</tool_call>",
        json!({ "name": "write", "arguments": { "path": path_str(&out), "content": content } })
    );
    // The server stops on the inner </tool_call> (and drops it): the reply ends inside the content string.
    let cut = whole[..whole.find("</tool_call>").unwrap()].to_string();
    assert_eq!(extract_text_calls(&cut), None);
    let ran = run(
        &fx,
        say_msg(&cut),
        &["tool-repair", "text-tool-calls"],
        RunOptions::default(),
    )
    .await;
    assert!(ran.results.is_empty());
    assert_eq!(ran.stored.content.len(), 1);
    assert_eq!(text_of_block(&ran.stored.content[0]), Some(cut.as_str()));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "before\n");
    // Whole, the tags in its content make it no call either: left alone, as before.
    assert_eq!(extract_text_calls(&whole), None);
    // Without its closing tag at the end, a complete body still runs, also one that is only badly quoted.
    let first = |t: &str| calls(t).and_then(|c| c.first().map(|c| c.name.clone()));
    assert_eq!(
        first("<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}").as_deref(),
        Some("read")
    );
    assert_eq!(
        first("<tool_call>{'name': 'read', 'arguments': {'path': 'a'}}").as_deref(),
        Some("read")
    );
    assert_eq!(
        extract_text_calls(r#"<function=write>{"path": "f", "content": "x <function=read>{\"path\": \"a\"}"#),
        None
    );
    // A server that keeps the stop string it stopped on: the reply ends with the inner closing tag (#136).
    assert_eq!(extract_text_calls(&format!("{cut}</tool_call>")), None);
    assert_eq!(
        extract_text_calls(
            r#"<function=write>{"path": "f", "content": "x <function=read>{\"path\": \"a\"}</function>"#
        ),
        None
    );
    assert_eq!(
        first("<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>").as_deref(),
        Some("read")
    );
}

#[tokio::test]
async fn a_call_whose_json_cannot_be_mended_still_tells_the_model_to_call_natively_85() {
    let fx = fixture();
    let text = format!(
        "<tool_call>{{\"name\": \"read\", \"arguments\": {{\"path\": {}, \"limit\": }}}}</tool_call>",
        fx.at
    );
    let ran = run(&fx, say_msg(&text), DEFAULT, RunOptions::default()).await;
    assert_eq!(ran.results.len(), 1);
    assert!(ran.results[0].is_error);
    let text = json_of(&ran.results[0].content);
    assert!(text.contains("not valid JSON"), "{text}");
    assert!(text.contains("call tools with native tool calls"), "{text}");
}

#[tokio::test]
async fn without_tool_repair_another_harnesss_tool_name_still_resolves() {
    let fx = fixture();
    let text = format!(
        "<tool_call>{{\"name\": \"read_file\", \"arguments\": {{\"path\": {}}}}}</tool_call>",
        fx.at
    );
    let ran = run(&fx, say_msg(&text), DEFAULT, RunOptions::default()).await;
    assert_eq!(ran.stored.content.len(), 1);
    let AssistantContent::ToolCall(c) = &ran.stored.content[0] else {
        panic!("{:?}", ran.stored.content)
    };
    assert_eq!(c.name, "read");
    assert_eq!(c.input.get("path"), Some(&json!(fx.file)));
    assert!(!ran.results[0].is_error);
}

#[tokio::test]
async fn leaves_alone_a_reply_with_a_native_call_an_unknown_tool_an_example_or_signed_thinking() {
    let fx = fixture();
    let tagged = format!(
        "<tool_call>{{\"name\": \"read\", \"arguments\": {{\"path\": {}}}}}</tool_call>",
        fx.at
    );
    let native = msg(vec![say(&tagged), call("n1", "read", json!({ "path": fx.file }))]);
    let with_native = run(&fx, native.clone(), DEFAULT, RunOptions::default()).await;
    assert_eq!(with_native.stored.content, native.content);
    assert_eq!(
        with_native
            .results
            .iter()
            .map(|r| r.tool_call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["n1"]
    );

    let mut max_tokens = say_msg(&tagged);
    max_tokens.stop_reason = StopReason::MaxTokens;
    let untouched = vec![
        say_msg(r#"<tool_call>{"name": "launch_rockets", "arguments": {"count": 3}}</tool_call>"#),
        // A name only a typo away from a tool (head → read) is not taken for it.
        say_msg("Example:\n```json\n{\"name\": \"head\", \"arguments\": {\"n\": 5}}\n```"),
        say_msg(&format!(
            "To call a tool, write {tagged} on its own, and the harness runs it."
        )),
        msg(vec![
            block(json!({ "type": "thinking", "thinking": "…", "signature": "sig" })),
            say(&tagged),
        ]),
        max_tokens,
    ];
    for m in untouched {
        let ran = run(&fx, m.clone(), DEFAULT, RunOptions::default()).await;
        assert_eq!(ran.requests.len(), 1, "{:?}", m.content);
        assert_eq!(ran.stored.content, m.content);
        assert!(ran.results.is_empty());
        assert!(!has_custom(&ran.entries));
    }
}

#[tokio::test]
async fn tells_the_model_when_an_earlier_call_in_the_text_did_not_run() {
    let fx = fixture();
    let text = format!(
        "Read a.\n<tool_call>{{\"name\": \"read\", \"arguments\": {{\"path\": \"a\"}}}}</tool_call>\nAnd this.\n<tool_call>{{\"name\": \"read\", \"arguments\": {{\"path\": {}}}}}</tool_call>",
        fx.at
    );
    let ran = run(&fx, say_msg(&text), DEFAULT, RunOptions::default()).await;
    assert_eq!(ran.results.len(), 1);
    let note = result_text(&ran.results[0], 0);
    assert!(
        note.contains("1 other call written earlier in the text did not run"),
        "{note}"
    );
}

/// What a cloud model may answer when asked what a write call looks like.
fn example(target: &Path) -> AssistantMessage {
    say_msg(&format!(
        "It looks like this:\n```json\n{{\"name\": \"write\", \"arguments\": {{\"path\": {}, \"content\": \"{{}}\"}}}}\n```",
        serde_json::to_string(&path_str(target)).unwrap()
    ))
}

#[tokio::test]
async fn off_for_a_model_served_from_a_public_address_or_with_no_base_url_unless_always_is_set() {
    let fx = fixture();
    let target = fx.root.path().join("config.json");
    std::fs::write(&target, r#"{"keep": true}"#).unwrap();
    let ex = example(&target);
    for base_url in [Some("https://api.anthropic.com".to_string()), None] {
        let ran = run(
            &fx,
            ex.clone(),
            &["tool-repair", "text-tool-calls"],
            RunOptions {
                base_url: Some(base_url),
                mode: PermissionMode::Edits,
                ..RunOptions::default()
            },
        )
        .await;
        assert_eq!(ran.requests.len(), 1);
        assert_eq!(ran.stored.content, ex.content);
        assert!(ran.results.is_empty());
    }
    assert_eq!(std::fs::read_to_string(&target).unwrap(), r#"{"keep": true}"#);
    // This machine and the local network count as local; always: true picks calls up from any model.
    let local = [
        ("http://[::1]:8080/v1", json!({})),
        ("http://192.168.1.20:1234/v1", json!({})),
        ("http://llama:8080/v1", json!({})),
        ("http://host.docker.internal:11434/v1", json!({})),
        ("https://api.example.com/v1", json!({ "always": true })),
    ];
    let text = formats(&fx.at)["<tool_call>"].clone();
    for (base_url, settings) in local {
        let ran = run(
            &fx,
            say_msg(&text),
            DEFAULT,
            RunOptions {
                base_url: Some(Some(base_url.into())),
                settings,
                ..RunOptions::default()
            },
        )
        .await;
        assert_eq!((base_url, ran.results.len()), (base_url, 1));
    }
}

/// An approve that records the reason and says no.
fn denier(asked: &Arc<Mutex<Vec<String>>>) -> ApproveFn {
    let asked = asked.clone();
    Arc::new(move |req: ApprovalRequest| {
        asked.lock().unwrap_or_else(|e| e.into_inner()).push(req.reason.clone());
        Box::pin(async { Ok(ApprovalAnswer::new(ApprovalDecision::Deny)) })
    })
}

#[tokio::test]
async fn in_edits_mode_a_write_picked_up_from_text_is_asked_about_a_read_is_not() {
    let fx = fixture();
    let target = fx.root.path().join("config.json");
    std::fs::write(&target, r#"{"keep": true}"#).unwrap();
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let ran = run(
        &fx,
        example(&target),
        &["tool-repair", "text-tool-calls"],
        RunOptions {
            mode: PermissionMode::Edits,
            approve: Some(denier(&asked)),
            ..RunOptions::default()
        },
    )
    .await;
    {
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].contains("written as text"), "{}", asked[0]);
    }
    assert!(ran.results[0].is_error);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), r#"{"keep": true}"#);
    let text = formats(&fx.at)["<tool_call>"].clone();
    let read = run(
        &fx,
        say_msg(&text),
        DEFAULT,
        RunOptions {
            mode: PermissionMode::Edits,
            approve: Some(denier(&asked)),
            ..RunOptions::default()
        },
    )
    .await;
    assert!(!read.results[0].is_error);
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn in_agent_mode_a_write_picked_up_from_text_is_asked_about_not_judged() {
    let fx = fixture();
    let target = fx.root.path().join("config.json");
    std::fs::write(&target, r#"{"keep": true}"#).unwrap();
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    // A judge asked about the write would answer this, and the write would run unasked.
    let judge = say_msg(r#"{"safe": true, "reason": "inside the working directory"}"#);
    let ran = run(
        &fx,
        example(&target),
        &["tool-repair", "text-tool-calls", "agent-judge"],
        RunOptions {
            mode: PermissionMode::Agent,
            approve: Some(denier(&asked)),
            then: vec![judge],
            ..RunOptions::default()
        },
    )
    .await;
    {
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].contains("written as text"), "{}", asked[0]);
    }
    assert!(ran.results[0].is_error);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), r#"{"keep": true}"#);
}
