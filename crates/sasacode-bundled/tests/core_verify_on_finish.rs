//! project-checks' verifyOnFinish: the named checks run when the model ends a turn without tool
//! calls after changing files or after a check failed; a failure goes back to the model (at most
//! twice a run), a pass says nothing, and a run that changed only tests or added skip markers gets
//! one note. The model is a replayed script; the checks are real shell commands in a temp dir.

mod support_core;

use std::collections::HashMap;
use std::path::Path;

use indexmap::IndexMap;
use serde_json::{Value, json};

use sasacode_ai::{AssistantMessage, Message};
use sasacode_bundled::project_checks::{Check, finish_checks, runs_check};
use sasacode_plugin_api::{PermissionMode, StopCause};
use support_core::*;

fn write(id: &str, path: &str, content: &str) -> AssistantMessage {
    reply(vec![call(id, "write", json!({ "path": path, "content": content }))])
}

fn bash(id: &str, command: &str) -> AssistantMessage {
    reply(vec![call(id, "bash", json!({ "command": command }))])
}

fn says(text: &str) -> AssistantMessage {
    reply(vec![say(text)])
}

/// One run of `script` with project-checks set up as `settings`, in `dir`, in auto mode (bash runs
/// unasked, as with the model's own checks). How it ended, how many requests, and what the plugin
/// sent the model.
async fn run(dir: &Path, settings: Value, script: Vec<AssistantMessage>) -> (StopCause, usize, Vec<String>) {
    let s = setup(
        script,
        &["project-checks"],
        SetupOpts {
            cwd: Some(path_str(dir)),
            settings: HashMap::from([("project-checks".to_string(), settings)]),
            ..SetupOpts::default()
        },
    )
    .await;
    s.agent.permissions().set_mode(PermissionMode::Auto);
    let cause = s.agent.prompt_text("fix it").await.unwrap_or(StopCause::Error);
    let sent = s
        .agent
        .messages()
        .iter()
        .filter_map(|m| match m {
            Message::User(u) => Some(serde_json::to_string(&u.content).unwrap_or_default()),
            _ => None,
        })
        .filter(|t| t.contains("[project-checks]"))
        .collect();
    (cause, s.requests().len(), sent)
}

fn settings(command: &str) -> Value {
    json!({ "checks": { "test": command, "lint": "true" }, "verifyOnFinish": ["test"] })
}

fn runs(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("runs.log")).map_or(0, |t| t.lines().count())
}

#[tokio::test]
async fn a_failing_check_goes_back_to_the_model_twice_then_the_run_ends() {
    let dir = temp_dir("verify-");
    let script = vec![
        write("1", "src/a.ts", "export const n = 2;\n"),
        says("Fixed."),
        says("Fixed now."),
        says("Really fixed."),
    ];
    let (cause, requests, sent) = run(dir.path(), settings("printf 'expected 3, got 2\\n'; exit 1"), script).await;
    assert_eq!((cause, requests), (StopCause::Done, 4));
    assert_eq!(sent.len(), 2, "{sent:?}");
    for s in &sent {
        assert!(s.contains("the check `test`") && s.contains("expected 3, got 2"), "{s}");
    }
}

#[tokio::test]
async fn a_pass_says_nothing_and_a_run_that_changed_nothing_runs_nothing() {
    let dir = temp_dir("verify-");
    let check = settings("echo ran >> runs.log");
    let (cause, requests, sent) = run(dir.path(), check.clone(), vec![says("Nothing to change.")]).await;
    assert_eq!(
        (cause, requests, sent.len(), runs(dir.path())),
        (StopCause::Done, 1, 0, 0)
    );

    let script = vec![write("1", "src/a.ts", "x\n"), says("Done.")];
    let (cause, requests, sent) = run(dir.path(), check.clone(), script).await;
    assert_eq!(
        (cause, requests, sent.len(), runs(dir.path())),
        (StopCause::Done, 2, 0, 1)
    );

    // The model ran the check through project_check after its last change, and it passed: not again.
    let ran = reply(vec![call("2", "project_check", json!({ "name": "test" }))]);
    let script = vec![write("1", "src/a.ts", "y\n"), ran, says("Done.")];
    let (cause, requests, sent) = run(dir.path(), check.clone(), script).await;
    assert_eq!(
        (cause, requests, sent.len(), runs(dir.path())),
        (StopCause::Done, 3, 0, 2)
    );
    // Through bash it runs again: `bun test | tail` passes as tail does, so a bash pass says nothing.
    let script = vec![
        write("1", "src/a.ts", "z\n"),
        bash("2", "echo ran >> runs.log"),
        says("Done."),
    ];
    let (cause, requests, sent) = run(dir.path(), check, script).await;
    assert_eq!(
        (cause, requests, sent.len(), runs(dir.path())),
        (StopCause::Done, 3, 0, 4)
    );
}

#[tokio::test]
async fn a_check_that_failed_earlier_counts_even_when_no_file_changed() {
    let dir = temp_dir("verify-");
    let script = vec![
        bash("1", "test -f fixed.txt"),
        says("I'll check the test file."),
        write("2", "fixed.txt", "ok\n"),
        says("Done."),
    ];
    let (cause, requests, sent) = run(dir.path(), settings("test -f fixed.txt"), script).await;
    assert_eq!((cause, requests), (StopCause::Done, 4));
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(
        sent[0].contains("(test -f fixed.txt) ran, and it failed"),
        "{}",
        sent[0]
    );
}

#[tokio::test]
async fn only_tests_changed_or_skip_markers_added_get_one_note_even_when_the_checks_pass() {
    let dir = temp_dir("verify-");
    let script = vec![
        write("1", "tests/sum.test.ts", "it.skip('adds', () => {});\n"),
        says("Done."),
        says("Done."),
    ];
    let (cause, requests, sent) = run(dir.path(), settings("true"), script).await;
    assert_eq!((cause, requests), (StopCause::Done, 3));
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].contains("[project-checks] Note: only test files changed in this run (tests/sum.test.ts)"));
    assert!(sent[0].contains("`it.skip(`"), "{}", sent[0]);

    // The code changed too, `process.exit(` is not `xit(`, and an iterator's `.skip(1)` skips no
    // test: no note.
    let script = vec![
        write("1", "src/cli.ts", "process.exit(1);\n"),
        write("2", "test/cli.test.ts", "test('exits', () => {});\n"),
        write("3", "tests/args.rs", "let rest = args.iter().skip(1);\n"),
        says("Done."),
    ];
    let (cause, requests, sent) = run(dir.path(), settings("true"), script).await;
    assert_eq!((cause, requests, sent.len()), (StopCause::Done, 4, 0), "{sent:?}");
}

#[test]
fn verify_on_finish_names_only_checks_written_in_the_settings() {
    let checks = |names: &[&str]| -> IndexMap<String, Check> {
        names
            .iter()
            .map(|n| {
                let check = Check {
                    command: format!("run {n}"),
                    timeout: 120.0,
                };
                (n.to_string(), check)
            })
            .collect()
    };
    let explicit = json!({ "checks": { "test": "run test" }, "verifyOnFinish": ["test"] });
    let found = finish_checks(&explicit, &checks(&["test"])).unwrap();
    assert_eq!(found.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["test"]);
    assert!(finish_checks(&json!({}), &checks(&["test"])).unwrap().is_empty());
    // Found in package.json: a script nobody wrote into the settings would run unasked.
    let detected = finish_checks(&json!({ "verifyOnFinish": ["test"] }), &checks(&["test"])).unwrap_err();
    assert!(detected.to_string().contains("written in `checks`"), "{detected}");
    let unknown = json!({ "checks": { "test": "run test" }, "verifyOnFinish": ["lint"] });
    let e = finish_checks(&unknown, &checks(&["test"])).unwrap_err();
    assert!(e.to_string().contains("unknown check: lint"), "{e}");
}

#[test]
fn a_bash_call_runs_a_check_where_a_command_starts() {
    for command in [
        "bun test packages/tools",
        "cd app && bun test packages/tools 2>&1",
        "bun test packages/tools/test/edit.test.ts",
        "(bun test packages/tools)",
        "echo start\nbun test packages/tools",
    ] {
        assert!(runs_check(command, "bun test packages/tools"), "{command}");
    }
    for command in [
        "grep -r 'bun test packages/tools' docs",
        "bun test packages/toolsx",
        "echo bun test packages/tools",
    ] {
        assert!(!runs_check(command, "bun test packages/tools"), "{command}");
    }
    assert!(!runs_check("grep -r true src", "true"));
}
