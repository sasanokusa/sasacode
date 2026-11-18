//! Regenerates the fixtures in `crates/sasacode-protocol/tests/fixtures` that the protocol and GUI
//! tests read: real `sasacode -p --output jsonl --control stdio` runs against the fake model server,
//! and the session logs they wrote. The binary is the one `SASACODE_E2E_BIN` names, as in `cli_e2e`:
//!
//!   cargo build -p sasacode-cli
//!   SASACODE_E2E_BIN=target/debug/sasacode cargo run -p sasacode-testing --example protocol-fixtures              # every scenario
//!   SASACODE_E2E_BIN=target/debug/sasacode cargo run -p sasacode-testing --example protocol-fixtures -- work stop # only these
//!
//! Run it again whenever the CLI's events or the session format change, and commit the result.
//!
//!   run      one approval, one tool, a reply (the smallest whole run)        → run.jsonl, session.jsonl
//!   work     read, edit and write with their diffs, a repaired call, a task list, two commands of
//!            one reply (each is asked about right before it runs: one answered late, one denied
//!            with a reason), "always", a failing command, a subagent, a message sent mid-run
//!                                                                            → work.jsonl, work-session.jsonl
//!   stop     aborted while a command runs                                    → stop.jsonl
//!   cancel   aborted while an approval waits                                 → cancel.jsonl
//!   error    the model server fails                                          → error.jsonl
//!   dropped  the connection drops mid-reply and the reply is asked again     → dropped.jsonl
//!   turns    the turn limit                                                  → turns.jsonl
//!   loop     loop-guard stops a run that repeats one call                    → loop.jsonl

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, bail};
use sasacode_testing::harness::{E2E_BIN_ENV, LINE_TIMEOUT, Live, Sandbox, e2e_bin, fake_config, workspace_root};
use sasacode_testing::{FakeServer, Reply, ToolCallSpec};
use serde_json::{Value, json};

/// How long an approval that is held waits for the next one before it is answered anyway.
const HOLD: Duration = Duration::from_millis(1500);

/// Session ids are random; every run's is written as this one, so a test can name it (the GUI's
/// app tests open work-session.jsonl by it).
const SESSION_ID: &str = "17cae3ae";

/// How an approval is answered.
enum Answer {
    /// At once, with this decision and, for a denial, a reason.
    Send(&'static str, Option<&'static str>),
    /// Left waiting until the next approval arrives, or [`HOLD`] passes without one; then allowed.
    Hold,
    /// Never.
    Never,
}

const ALLOW: Answer = Answer::Send("allow", None);

struct Scenario {
    prompt: &'static str,
    replies: Vec<Reply>,
    /// Top-level keys added to the fake server's config.
    config: Value,
    args: &'static [&'static str],
    /// Files the project starts with.
    files: &'static [(&'static str, &'static str)],
    /// Called for every event before it is answered.
    on: fn(&Value, &mut Live),
    answer: fn(&Value) -> Answer,
    /// Also keep the session log, under this name.
    session: Option<&'static str>,
}

impl Scenario {
    fn new(prompt: &'static str, replies: Vec<Reply>) -> Scenario {
        Scenario {
            prompt,
            replies,
            config: json!({}),
            args: &[],
            files: &[],
            on: |_, _| {},
            answer: |_| ALLOW,
            session: None,
        }
    }
}

const CLIENT: &str = r#"export async function request(url: string) {
  let attempt = 0;
  while (true) {
    try {
      return await fetch(url);
    } catch (err) {
      attempt += 1;
      // retry twice, 500 ms apart
      if (attempt > 2) throw err;
      await new Promise((r) => setTimeout(r, 500));
    }
  }
}
"#;
const RETRY: &str = "export function backoff(attempt: number, base = 200): number {
  return base * 2 ** attempt;
}
";
const FAILING: &str = "sh -c 'echo boom; exit 1'";

fn calls(calls: Vec<(&str, Value)>) -> Reply {
    Reply::tool_calls(
        calls
            .into_iter()
            .map(|(name, args)| ToolCallSpec::new(name, args))
            .collect(),
    )
}

fn read(path: &str) -> Reply {
    calls(vec![("read", json!({ "path": path }))])
}

fn bash(command: &str) -> Reply {
    calls(vec![("bash", json!({ "command": command }))])
}

fn todos(status: [&str; 3]) -> Reply {
    let items = ["遅延の計算を切り出す", "client.tsを繋ぎ替える", "動作を確認する"];
    let todos: Vec<Value> = items
        .iter()
        .zip(status)
        .map(|(content, status)| json!({ "content": content, "status": status }))
        .collect();
    calls(vec![("todo_write", json!({ "todos": todos }))])
}

/// `reply`, with reasoning before it.
fn thought(reasoning: &str, reply: Reply) -> Reply {
    Reply {
        reasoning: Some(reasoning.into()),
        ..reply
    }
}

const SCENARIOS: [&str; 8] = ["run", "work", "stop", "cancel", "error", "dropped", "turns", "loop"];

fn scenario(name: &str) -> Option<Scenario> {
    Some(match name {
        "run" => Scenario {
            config: json!({ "permissions": { "mode": "ask" } }),
            on: |e, run| {
                if e["type"] == "approval_request" {
                    run.send(&json!("not json"));
                }
            },
            session: Some("session.jsonl"),
            ..Scenario::new(
                "ファイルを作って",
                vec![
                    thought(
                        "まずファイルを作る。",
                        bash("echo hi > made.txt").with_text("ファイルを作ります。"),
                    ),
                    Reply::text("**完了**しました。`made.txt` を作成しています。"),
                ],
            )
        },
        "work" => Scenario {
            files: &[
                ("src/client.ts", CLIENT),
                (
                    "README.md",
                    "# demo\n\nRequests retry twice with a fixed 500 ms delay.\n",
                ),
            ],
            on: |e, run| {
                // Typed while the run is busy: the model sees it at its next turn.
                let command = e["call"]["input"]["command"].as_str().unwrap_or_default();
                if e["type"] == "tool_start" && e["call"]["name"] == "bash" && command.starts_with("grep") {
                    run.send(&json!({ "type": "message", "text": "テストも足して" }));
                }
            },
            answer: |e| match e["args"]["command"].as_str() {
                Some("echo one") => Answer::Hold,
                Some("echo two") => Answer::Send("deny", Some("それは不要")),
                Some(FAILING) => Answer::Send("always", None),
                _ => ALLOW,
            },
            session: Some("work-session.jsonl"),
            ..Scenario::new(
                "再試行を指数バックオフにして",
                vec![
                    thought(
                        "まず現在の実装を読む。",
                        read("src/client.ts").with_text("現在の再試行処理を確認します。"),
                    ),
                    bash("grep -n retry src/client.ts README.md"),
                    // A name and an argument no tool has: tool-repair mends both.
                    calls(vec![("read_file", json!({ "file": "README.md" }))]),
                    todos(["in_progress", "pending", "pending"]),
                    calls(vec![
                        ("write", json!({ "path": "src/retry.ts", "content": RETRY })),
                        (
                            "edit",
                            json!({
                                "path": "src/client.ts",
                                "old_string": "      await new Promise((r) => setTimeout(r, 500));",
                                "new_string": "      await new Promise((r) => setTimeout(r, backoff(attempt)));",
                            }),
                        ),
                    ])
                    .with_text("遅延の計算を`src/retry.ts`に切り出します。"),
                    todos(["completed", "completed", "in_progress"]),
                    // Two calls in one reply: both wait for an answer at the same time.
                    calls(vec![
                        ("bash", json!({ "command": "echo one" })),
                        ("bash", json!({ "command": "echo two" })),
                    ]),
                    bash(FAILING),
                    // The same command again: "always" answered it for the rest of this run.
                    bash(FAILING),
                    calls(vec![(
                        "task",
                        json!({ "description": "構成を調べる", "prompt": "src を読んで構成を報告して" }),
                    )]),
                    // The subagent's two requests.
                    read("src/retry.ts"),
                    Reply::text("`src/`には`client.ts`と`retry.ts`があります。"),
                    todos(["completed", "completed", "completed"]),
                    Reply::text(
                        "**完了**しました。\n\n- `src/retry.ts`を新設\n- `src/client.ts`の待ち時間を`backoff()`に置き換え",
                    ),
                ],
            )
        },
        "stop" => Scenario {
            on: |e, run| {
                if e["type"] == "tool_start" {
                    run.send(&json!({ "type": "abort" }));
                }
            },
            ..Scenario::new("しばらく待って", vec![bash("sleep 30").with_text("30秒待ちます。")])
        },
        "cancel" => Scenario {
            on: |e, run| {
                if e["type"] == "approval_request" {
                    run.send(&json!({ "type": "abort" }));
                }
            },
            answer: |_| Answer::Never,
            ..Scenario::new("ファイルを消して", vec![bash("rm -f made.txt")])
        },
        "error" => Scenario::new("こんにちは", vec![Reply::status(500)]),
        "dropped" => Scenario::new(
            "こんにちは",
            vec![
                Reply::text("途中まで書いたところで").with_cut(),
                Reply::text("**やり直し**ました。"),
            ],
        ),
        "turns" => Scenario {
            files: &[("a.txt", "a\n")],
            args: &["--max-turns", "1"],
            ..Scenario::new("読んで", vec![read("a.txt"), read("a.txt")])
        },
        "loop" => Scenario {
            files: &[("a.txt", "a\n")],
            ..Scenario::new("読んで", (0..16).map(|_| read("a.txt")).collect())
        },
        _ => return None,
    })
}

fn approval(id: &Value, decision: &str, feedback: Option<&str>) -> Value {
    let mut message = json!({ "type": "approval", "id": id, "decision": decision });
    if let Some(feedback) = feedback {
        message["feedback"] = json!(feedback);
    }
    message
}

fn generate(name: &str, sc: Scenario, program: &Path, out: &Path) -> anyhow::Result<()> {
    let server = FakeServer::start(sc.replies).context("the fake server")?;
    let sandbox = Sandbox::with_program(&fake_config(server.url(), sc.config), program);
    for (path, content) in sc.files {
        let path = sandbox.project.join(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, content)?;
    }
    let mut args = vec!["-p", sc.prompt, "--output", "jsonl", "--control", "stdio"];
    args.extend_from_slice(sc.args);
    let mut run = sandbox.start(args);
    let mut lines: Vec<Value> = Vec::new();
    let mut held: Vec<Value> = Vec::new();
    let release = |run: &mut Live, held: &mut Vec<Value>| {
        for id in held.drain(..) {
            run.send(&approval(&id, "allow", None));
        }
    };
    loop {
        // A held approval is answered when the next one arrives, or after a moment if none does.
        let e = match run.next_within(if held.is_empty() { LINE_TIMEOUT } else { HOLD }) {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) if held.is_empty() => bail!("{name}: no event for {} s", LINE_TIMEOUT.as_secs()),
            Err(_) => {
                release(&mut run, &mut held);
                continue;
            }
        };
        lines.push(e.clone());
        (sc.on)(&e, &mut run);
        if e["type"] == "approval_request" {
            match (sc.answer)(&e) {
                Answer::Hold => held.push(e["id"].clone()),
                Answer::Never => {}
                Answer::Send(decision, feedback) => {
                    run.send(&approval(&e["id"], decision, feedback));
                    release(&mut run, &mut held);
                }
            }
        }
        if e["type"] == "agent_end" {
            run.end();
        }
    }
    // stop, cancel and error end with a non-zero code; what the run printed is the fixture either way.
    run.exited();
    let id = pin_session_id(&mut lines).with_context(|| format!("{name}: no session event first"))?;
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(out.join(format!("{name}.jsonl")), text)?;
    let mut wrote = format!(
        "{name}.jsonl ({} events, {} replies unused)",
        lines.len(),
        server.left()
    );
    if let Some(session) = sc.session {
        let log = std::fs::read_to_string(session_log(&sandbox.home.join("sessions"))?)?;
        let header = format!("\"id\":\"{id}\"");
        if !log.lines().next().is_some_and(|l| l.contains(&header)) {
            bail!("{name}: the session log does not start with the header of session {id}");
        }
        std::fs::write(
            out.join(session),
            log.replacen(&header, &format!("\"id\":\"{SESSION_ID}\""), 1),
        )?;
        wrote += &format!(" and {session}");
    }
    println!("wrote {wrote}");
    Ok(())
}

/// Writes [`SESSION_ID`] in place of the run's session id in its first event, `session`, whose
/// `path` ends `_<id>.jsonl`; the id it replaced.
fn pin_session_id(lines: &mut [Value]) -> Option<String> {
    let first = lines.first_mut().filter(|e| e["type"] == "session")?;
    let id = first["id"].as_str()?.to_owned();
    let path = first["path"].as_str()?;
    let path = path.strip_suffix(&format!("_{id}.jsonl"))?;
    first["path"] = json!(format!("{path}_{SESSION_ID}.jsonl"));
    first["id"] = json!(SESSION_ID);
    Some(id)
}

/// The one log under `sessions`: one folder for the project, one `.jsonl` in it.
fn session_log(sessions: &Path) -> anyhow::Result<std::path::PathBuf> {
    let only = |dir: &Path, keep: fn(&Path) -> bool| -> anyhow::Result<std::path::PathBuf> {
        let found: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| dir.display().to_string())?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|p| keep(p))
            .collect();
        match &found[..] {
            [one] => Ok(one.clone()),
            _ => bail!("expected one entry in {}, found {found:?}", dir.display()),
        }
    };
    let dir = only(sessions, |p| p.is_dir())?;
    only(&dir, |p| p.extension().is_some_and(|x| x == "jsonl"))
}

fn main() -> anyhow::Result<()> {
    let Some(program) = e2e_bin() else {
        bail!(
            "{E2E_BIN_ENV} names no binary: SASACODE_E2E_BIN=target/debug/sasacode cargo run -p sasacode-testing --example protocol-fixtures"
        );
    };
    let out = workspace_root().join("crates/sasacode-protocol/tests/fixtures");
    let wanted: Vec<String> = std::env::args().skip(1).collect();
    let names: Vec<&str> = if wanted.is_empty() {
        SCENARIOS.to_vec()
    } else {
        wanted.iter().map(String::as_str).collect()
    };
    let mut scenarios = Vec::new();
    for name in names {
        let sc = scenario(name).with_context(|| format!("no scenario \"{name}\" (known: {})", SCENARIOS.join(", ")))?;
        scenarios.push((name, sc));
    }
    for (name, sc) in scenarios {
        generate(name, sc, &program, &out)?;
    }
    Ok(())
}
