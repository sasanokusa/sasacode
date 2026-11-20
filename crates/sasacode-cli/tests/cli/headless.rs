//! packages/cli/test/cli.test.ts, the headless and session tests (the config ones are lib tests in
//! src/config_tests.rs), and headless-pipe.test.ts.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use sasacode_ai::{AssistantContent, AssistantMessage, Message, StopReason, Usage, UserContent};
use sasacode_cli::setup::{SetupOptions, setup};
use sasacode_plugin_api::StopCause;
use sasacode_testing::harness::{parse_lines, roles};
use sasacode_testing::{FakeServer, ReplayOptions, ReplayProvider, Reply};

use crate::support::{Bin, child, chunk, fake_config, provider, replay_provider, sse, sse_body};
use crate::test_server::{Res, TestServer};

/// A reply from the replay provider "test", model "m" (stopReason stop, empty usage, timestamp 0).
pub fn text(t: &str) -> AssistantMessage {
    AssistantMessage {
        content: vec![AssistantContent::text(t)],
        api: "replay".into(),
        provider: "test".into(),
        model: "m".into(),
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        timestamp: 0,
        replay_scope: None,
        error_message: None,
    }
}

/// `registerApi("replay", replayProvider(script))`, kept to read its requests.
pub fn replay(script: Vec<AssistantMessage>) -> Arc<ReplayProvider> {
    let provider = Arc::new(ReplayProvider::new(script, ReplayOptions::default()));
    sasacode_ai::register_api("replay", provider.clone());
    provider
}

fn new(cwd: &str) -> SetupOptions {
    SetupOptions::new(cwd)
}

/// `/sasacode -r (\S+)/.exec(text)?.[1]`.
fn resume_id(text: &str) -> Option<String> {
    text.match_indices("sasacode -r ").find_map(|(i, m)| {
        let id: String = text[i + m.len()..].chars().take_while(|c| !c.is_whitespace()).collect();
        (!id.is_empty()).then_some(id)
    })
}

/// The session logs under `home`.
fn session_logs(home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for dir in std::fs::read_dir(home.join("sessions")).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            out.push(f.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

#[test]
fn headless_jsonl_emits_the_whole_run_and_exits_0() {
    let server = FakeServer::start(vec![Reply::text("hello from replay")]).expect("the fake server");
    let bin = Bin::new(&fake_config(server.url(), json!({})));
    let r = bin.jsonl("hi", &["--no-session"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let events = r.events;
    assert_eq!(events[0]["id"], Value::Null); // --no-session
    assert_eq!(events[0]["path"], Value::Null);
    let keys: Vec<&String> = events[0].as_object().map(|o| o.keys().collect()).unwrap_or_default();
    assert_eq!(keys, ["type", "id", "path", "contextWindow"]);
    assert_eq!(events[1]["type"], "message_end"); // the user message
    assert_eq!(events[1]["message"]["role"], "user");
    assert!(events.iter().any(|e| e["type"] == "message_update"));
    assert_eq!(events.last(), Some(&json!({ "type": "agent_end", "cause": "done" })));
    assert!(Value::Array(events).to_string().contains("hello from replay"));
}

// About 200 KB: well past a pipe's 64 KB buffer.
fn long_answer() -> String {
    let mut s: String = (0..6000).map(|i| format!("line {i:04} {}\n", "x".repeat(24))).collect();
    s.push_str("END OF ANSWER\n");
    s
}

/// What `sasacode -p hi --no-session --output <output>` wrote into a pipe nobody read until a second
/// after the answer came (or it exited), its exit code, and whether it was still running then.
struct SlowPipe {
    stdout: String,
    code: Option<i32>,
    waited_for_the_reader: bool,
}

fn slow_pipe(output: &str) -> SlowPipe {
    let answer = long_answer();
    let answered: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let server = {
        let answered = answered.clone();
        TestServer::start(move |req| match req.path.as_str() {
            "/v1/models" => {
                Res::json(json!({ "object": "list", "data": [{ "id": "m", "object": "model", "owned_by": "x" }] }))
            }
            "/v1/chat/completions" => {
                let mut events: Vec<Value> = answer
                    .as_bytes()
                    .chunks(4000)
                    .map(|part| chunk("m", json!({ "content": String::from_utf8_lossy(part) }), None))
                    .collect();
                events.push(chunk("m", json!({}), Some("stop")));
                *answered.lock().unwrap() = Some(Instant::now());
                sse(sse_body(&events))
            }
            _ => Res::text(404, ""),
        })
    };
    let bin = Bin::new(&json!({
        "providers": { "fake": provider("openai-chat", &format!("{}/v1", server.url())) },
        "model": "fake/m",
        "plugins": { "disabled": ["openai-codex"] },
    }));
    let mut child = bin
        .command(["-p", "hi", "--no-session", "--output", output])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("sasacode");
    // Without the fix sasacode exits while most of the answer is still waiting for the reader; with
    // it, it waits for the reader, so the reader is let go after a while either way.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let came = answered
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() > Duration::from_secs(1));
        if came || Instant::now() > deadline || child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let waited_for_the_reader = child.try_wait().ok().flatten().is_none();
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_string(&mut stdout).expect("its output");
    }
    let code = child.wait().expect("its exit").code();
    SlowPipe {
        stdout,
        code,
        waited_for_the_reader,
    }
}

#[test]
fn headless_returns_only_once_its_output_has_gone_out_to_a_slow_reader_102() {
    let run = slow_pipe("jsonl");
    // The run is over a second after the answer came: only the output is still on its way, and
    // sasacode is still there (main would exit here, dropping the rest).
    assert!(run.waited_for_the_reader);
    assert_eq!(run.code, Some(0));
    let events = parse_lines(&run.stdout);
    assert_eq!(events.last(), Some(&json!({ "type": "agent_end", "cause": "done" })));
}

#[cfg(unix)]
#[test]
fn p_into_a_slow_pipe_reader_the_whole_answer_arrives_and_jsonl_ends_with_agent_end_102() {
    let text = std::thread::spawn(|| slow_pipe("text"));
    let jsonl = slow_pipe("jsonl");
    let plain = text.join().expect("the text run");
    assert_eq!(plain.code, Some(0));
    assert_eq!(plain.stdout.len(), long_answer().len());
    assert!(plain.stdout.ends_with("END OF ANSWER\n"));
    assert_eq!(jsonl.code, Some(0));
    let events = parse_lines(&jsonl.stdout);
    assert_eq!(events.last(), Some(&json!({ "type": "agent_end", "cause": "done" })));
}

#[test]
fn one_shot_runs_can_be_chained_with_r_id_p_no_tmux_needed() {
    let server =
        FakeServer::start(vec![Reply::text("first answer"), Reply::text("second answer")]).expect("the fake server");
    let bin = Bin::new(&fake_config(server.url(), json!({})));
    let first = bin.run(["-p", "remember 42"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let id = resume_id(&first.stderr).unwrap_or_else(|| panic!("no `sasacode -r <id>` in: {}", first.stderr));
    assert!(
        first.stderr.contains(&format!("sasacode -r {id} -p")),
        "{}",
        first.stderr
    );
    // The id is the saved session's.
    let logs = session_logs(&bin.sb.home);
    assert!(logs.iter().any(|l| l.ends_with(&format!("_{id}.jsonl"))), "{logs:?}");
    let second = bin.run(["-r", &id, "-p", "what number?"]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    assert!(second.stdout.contains("second answer"), "{}", second.stdout);
    let request = &server.requests()[1];
    assert_eq!(roles(request), ["system", "user", "assistant", "user"]);
    assert!(request["messages"][1].to_string().contains("remember 42"));
}

#[tokio::test(flavor = "multi_thread")]
async fn no_session_a_resumed_conversation_is_read_and_nothing_more_is_written_to_it_33() {
    let Some(t) = child() else { return };
    t.config(&json!({ "providers": { "test": replay_provider() }, "model": "test/m" }));
    replay(vec![text("ok"), text("ok"), text("ok")]);
    let first = setup(new(&t.cwd())).await.unwrap();
    assert_eq!(first.agent.prompt_text("kept").await.unwrap(), StopCause::Done);
    let session = first.agent.session().unwrap();
    let (id, path) = (session.id().to_string(), session.path().to_path_buf());
    let before = std::fs::read_to_string(&path).unwrap();
    let resumed = setup(SetupOptions {
        resume: Some(id),
        no_session: true,
        ..new(&t.cwd())
    })
    .await
    .unwrap();
    assert!(resumed.agent.session().is_none());
    assert_eq!(resumed.agent.messages().len(), 2);
    assert_eq!(
        resumed.agent.prompt_text("not saved (-r)").await.unwrap(),
        StopCause::Done
    );
    let later = setup(SetupOptions {
        no_session: true,
        ..new(&t.cwd())
    })
    .await
    .unwrap();
    later.load_session(&path).await.unwrap();
    assert_eq!(
        later.agent.prompt_text("not saved (/resume)").await.unwrap(),
        StopCause::Done
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resumed_compacted_session_shows_everything_said_the_model_gets_the_summary() {
    let Some(t) = child() else { return };
    t.config(&json!({ "providers": { "test": replay_provider() }, "model": "test/m" }));
    replay(vec![text("a1"), text("a2")]);
    let first = setup(new(&t.cwd())).await.unwrap();
    first.agent.prompt_text("before compaction").await.unwrap();
    first
        .agent
        .replace_messages(vec![Message::user(vec![UserContent::text(
            "[Summary of the earlier conversation, compacted to save context]\n\nx",
        )])])
        .unwrap();
    first.agent.prompt_text("after compaction").await.unwrap();
    let id = first.agent.session().unwrap().id().to_string();
    let resumed = setup(SetupOptions {
        resume: Some(id),
        ..new(&t.cwd())
    })
    .await
    .unwrap();
    assert!(
        serde_json::to_string(&resumed.agent.messages()[0])
            .unwrap()
            .contains("[Summary of")
    );
    let said: Vec<String> = resumed
        .transcript()
        .iter()
        .map(|m| serde_json::to_value(m).unwrap()["content"].to_string())
        .collect();
    let expected = ["before compaction", "a1", "after compaction", "a2"];
    assert_eq!(said.len(), expected.len(), "{said:?}");
    for (s, e) in said.iter().zip(expected) {
        assert!(s.contains(e), "{s} should contain {e}");
    }
    // Not compacted: the history as restored.
    let plain = setup(new(&t.cwd())).await.unwrap();
    assert_eq!(plain.transcript(), plain.agent.messages());
}

#[tokio::test(flavor = "multi_thread")]
async fn m_beats_the_model_a_resumed_session_ended_with() {
    let Some(t) = child() else { return };
    t.config(&json!({
        "providers": { "test": replay_provider(), "other": replay_provider() },
        "model": "test/m",
    }));
    replay(vec![text("ok")]);
    let first = setup(new(&t.cwd())).await.unwrap();
    first.agent.prompt_text("hi").await.unwrap();
    let id = first.agent.session().unwrap().id().to_string();
    let resumed = setup(SetupOptions {
        resume: Some(id.clone()),
        ..new(&t.cwd())
    })
    .await
    .unwrap();
    assert_eq!(resumed.agent.model().provider, "test");
    let chosen = setup(SetupOptions {
        resume: Some(id),
        model: Some("other/n".into()),
        ..new(&t.cwd())
    })
    .await
    .unwrap();
    assert_eq!(chosen.agent.model().spec(), "other/n");
}

// Unix permission bits; on Windows the user profile's ACL keeps ~/.sasacode private.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_sasacode_home_is_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let Some(t) = child() else { return };
    t.config(&json!({ "providers": { "test": replay_provider() }, "model": "test/m" }));
    std::fs::set_permissions(&t.home, std::fs::Permissions::from_mode(0o755)).unwrap();
    setup(SetupOptions {
        no_session: true,
        ..new(&t.cwd())
    })
    .await
    .unwrap();
    assert_eq!(std::fs::metadata(&t.home).unwrap().permissions().mode() & 0o777, 0o700);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_judge_model_that_does_not_resolve_fails_at_startup() {
    let Some(t) = child() else { return };
    t.config(&json!({
        "providers": { "test": replay_provider() },
        "model": "test/m",
        "judgeModel": "nowhere/x",
    }));
    let r = setup(SetupOptions {
        no_session: true,
        ..new(&t.cwd())
    })
    .await;
    assert!(r.is_err());
}

#[cfg(unix)]
#[test]
fn without_a_home_directory_or_sasacode_home_it_exits_and_the_project_config_is_not_taken_for_the_global_one() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let (proj, mark) = (dir.path().join("proj"), dir.path().join("MCP-STARTED"));
    std::fs::create_dir_all(proj.join(".sasacode")).unwrap();
    let config = json!({
        "mcpServers": { "x": { "command": "/usr/bin/touch", "args": [mark.to_string_lossy()] } },
        "permissions": { "mode": "auto" },
    });
    std::fs::write(proj.join(".sasacode/config.json"), config.to_string()).unwrap();
    for env in [&[][..], &[("HOME", ""), ("USERPROFILE", ""), ("SASACODE_HOME", "")]] {
        let out = std::process::Command::new(crate::support::bin())
            .args(["-p", "go", "--output", "jsonl", "--stdin"])
            .current_dir(&proj)
            .env_clear()
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{env:?}: {stderr}");
        assert!(stderr.contains("set SASACODE_HOME"), "{env:?}: {stderr}");
        assert!(!mark.exists(), "{env:?}: the project's server started");
    }
}

/// A run that is not on a terminal asks nothing before git runs: a folder not kept as allowed is
/// left alone, and the folders used before are written as known the first time.
#[test]
fn a_run_off_the_terminal_does_not_ask_about_git_and_runs_it_only_in_a_folder_kept() {
    let server = FakeServer::start(vec![Reply::text("ok")]).expect("the fake server");
    let bin = Bin::new(&fake_config(
        server.url(),
        json!({ "plugins": { "enabled": ["git-context"] } }),
    ));
    let r = bin.jsonl("hi", &["--no-session"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        !r.stderr.contains("Run git") && !r.stderr.contains("gitを実行"),
        "{}",
        r.stderr
    );
    let home = &bin.sb.home;
    let kept = sasacode_protocol::git_folders::read(home)
        .unwrap()
        .expect("written the first time");
    assert!(
        !kept.contains_key(&sasacode_protocol::git_folders::key(&bin.cwd())),
        "{kept:?}"
    );
}

/// The list of folders git may run in is started by the first run, with no plugin that runs git
/// loaded: the folder of a conversation kept from then on is not on it.
#[test]
fn the_first_run_starts_the_git_folders_whatever_plugins_load() {
    let server = FakeServer::start(vec![Reply::text("ok"), Reply::text("ok")]).expect("the fake server");
    let bin = Bin::new(&fake_config(server.url(), json!({})));
    let home = &bin.sb.home;
    assert!(sasacode_protocol::git_folders::read(home).unwrap().is_none());
    for _ in 0..2 {
        let r = bin.jsonl("hi", &[]);
        assert_eq!(r.code, 0, "{}", r.stderr);
    }
    let kept = sasacode_protocol::git_folders::read(home)
        .unwrap()
        .expect("written by the first run");
    assert!(
        !kept.contains_key(&sasacode_protocol::git_folders::key(&bin.cwd())),
        "{kept:?}"
    );
    assert!(
        sasacode_protocol::git_folders::projects(home).len() == 1,
        "the conversations were kept"
    );
}
