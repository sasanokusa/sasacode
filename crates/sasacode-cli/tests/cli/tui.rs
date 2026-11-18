//! The TUI wired to a real harness (`sasacode` without -p): setup from a config, the bundled
//! plugins, sessions and the model catalog, driven through a stand-in terminal against the fake
//! model server. Each test runs in a home of its own (`support::child`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::task::JoinHandle;

use sasacode_cli::setup::{Harness, SetupOptions, setup};
use sasacode_cli::tui::run_with;
use sasacode_testing::{FakeServer, Reply};
use sasacode_tui::testing::FakeTerminal;

use crate::support::{Home, child, child_with, merged, provider, sink, with_quiet_builtins};
use crate::test_server::{Res, TestServer};

/// The global config: "fake" on the fake server (and every built-in provider on `sink`), `fake/m`,
/// no retries, no update check; `extra`'s top-level keys replace these.
fn configure(t: &Home, fake: &FakeServer, sink: &TestServer, extra: Value) -> Value {
    let config = merged(
        json!({
            "providers": with_quiet_builtins(&sink.url(), json!({ "fake": provider("openai-chat", fake.url()) })),
            "model": "fake/m",
            "maxRetries": 0,
            "updateCheck": false,
            // It registers its provider again with no key variable: listing would ask the keychain.
            "plugins": { "disabled": ["openai-codex"] },
        }),
        extra,
    );
    t.config(&config);
    config
}

async fn harness(t: &Home, resume: Option<&str>) -> Arc<Harness> {
    setup(SetupOptions {
        resume: resume.map(str::to_string),
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup")
}

const WIDTH: usize = 100;
const HEIGHT: usize = 40;

/// What a `w`×`h` terminal shows after `out`, one trimmed string per row: printed graphemes at the
/// cursor, cursor moves, line feeds and clears. Styles, modes, links and other strings are skipped.
fn screen(out: &str, w: usize, h: usize) -> Vec<String> {
    let blank = || vec![" ".to_string(); w];
    let mut rows: Vec<Vec<String>> = (0..h).map(|_| blank()).collect();
    let (mut r, mut c) = (0usize, 0usize);
    let b = out.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            0x1b => match b.get(i + 1) {
                Some(b'[') => {
                    let mut j = i + 2;
                    while j < b.len() && (0x20..=0x3f).contains(&b[j]) {
                        j += 1;
                    }
                    let Some(&end) = b.get(j) else { break };
                    let params = &out[i + 2..j];
                    i = j + 1;
                    if params.starts_with(['?', '<', '>', '=']) {
                        continue;
                    }
                    let n: Vec<usize> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
                    let at = |k: usize| n.get(k).copied().filter(|&v| v > 0).unwrap_or(1);
                    match end {
                        b'H' | b'f' => (r, c) = ((at(0) - 1).min(h - 1), (at(1) - 1).min(w - 1)),
                        b'A' => r = r.saturating_sub(at(0)),
                        b'B' => r = (r + at(0)).min(h - 1),
                        b'C' => c = (c + at(0)).min(w - 1),
                        b'D' => c = c.saturating_sub(at(0)),
                        b'G' => c = (at(0) - 1).min(w - 1),
                        b'J' => {
                            let mode = n[0];
                            for (y, row) in rows.iter_mut().enumerate() {
                                for (x, cell) in row.iter_mut().enumerate() {
                                    let wipe = match mode {
                                        0 => (y, x) >= (r, c),
                                        1 => (y, x) <= (r, c),
                                        _ => true,
                                    };
                                    if wipe {
                                        *cell = " ".into();
                                    }
                                }
                            }
                        }
                        b'K' => {
                            let mode = n[0];
                            for (x, cell) in rows[r].iter_mut().enumerate() {
                                let wipe = match mode {
                                    0 => x >= c,
                                    1 => x <= c,
                                    _ => true,
                                };
                                if wipe {
                                    *cell = " ".into();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // OSC, APC, DCS, PM: up to BEL or ESC \.
                Some(b']' | b'_' | b'P' | b'^') => {
                    let mut j = i + 2;
                    while j < b.len() && b[j] != 0x07 && !(b[j] == 0x1b && b.get(j + 1) == Some(&b'\\')) {
                        j += 1;
                    }
                    i = if b.get(j) == Some(&0x07) { j + 1 } else { j + 2 };
                }
                Some(_) => i += 2,
                None => break,
            },
            b'\r' => {
                c = 0;
                i += 1;
            }
            b'\n' => {
                if r + 1 < h {
                    r += 1;
                } else {
                    rows.remove(0);
                    rows.push(blank());
                }
                i += 1;
            }
            0x08 => {
                c = c.saturating_sub(1);
                i += 1;
            }
            x if x < 0x20 || x == 0x7f => i += 1,
            _ => {
                let g = sasacode_tui::text::graphemes(&out[i..]).next().unwrap_or(" ");
                let gw = sasacode_tui::text::visible_width(g);
                if c < w {
                    rows[r][c] = g.to_string();
                    for k in 1..gw {
                        if c + k < w {
                            rows[r][c + k] = String::new();
                        }
                    }
                }
                c += gw;
                i += g.len();
            }
        }
    }
    rows.into_iter()
        .map(|row| row.concat().trim_end().to_string())
        .collect()
}

/// The TUI on a 100×40 stand-in terminal.
struct Ui {
    term: FakeTerminal,
    run: JoinHandle<anyhow::Result<i32>>,
}

impl Ui {
    fn start(h: &Arc<Harness>) -> Ui {
        let term = FakeTerminal::new(WIDTH as u16, HEIGHT as u16);
        let run = tokio::spawn(run_with(h.clone(), String::new(), Box::new(term.clone())));
        Ui { term, run }
    }

    async fn typ(&self, text: &str) {
        self.term.type_text(text);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    async fn key(&self, id: &str) {
        self.term.key(id);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    /// Type `line` and press enter.
    async fn send(&self, line: &str) {
        self.typ(line).await;
        self.key("enter").await;
    }

    /// Wait (up to ten seconds) until `needle` was written in one piece since `from`, or more rows
    /// of the screen hold it than did at `from`. The full screen rewrites only the cells that
    /// changed, so a reply that streamed in over several frames is never one run in the output,
    /// only on the screen.
    async fn until(&self, from: usize, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let holding = |rows: &[String]| rows.iter().filter(|row| row.contains(needle)).count();
        let out = self.term.output();
        let before = holding(&screen(out.get(..from).unwrap_or(""), WIDTH, HEIGHT));
        loop {
            if self.term.text(from).contains(needle) {
                return;
            }
            let rows = screen(&self.term.output(), WIDTH, HEIGHT);
            if holding(&rows) > before {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "never drew {needle:?}; the screen:\n{}\nwritten since:\n{}",
                rows.join("\n"),
                self.term.text(from)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn exited(self) -> (i32, FakeTerminal) {
        // Leaving ends the plugins and closes the session; on the Windows runner that has taken
        // longer than 10 s once in several runs (the e2e tests there wait 30 s too).
        let code = match tokio::time::timeout(Duration::from_secs(30), self.run).await {
            Ok(Ok(Ok(code))) => code,
            other => panic!(
                "the TUI did not exit: {other:?}; the screen:\n{}",
                screen(&self.term.output(), WIDTH, HEIGHT).join("\n")
            ),
        };
        (code, self.term)
    }
}

fn text_of(request: &Value) -> String {
    request["messages"].to_string()
}

#[test]
fn the_screen_puts_changed_cells_where_the_cursor_moved() {
    // Two frames of a streamed reply: the second rewrites only the cells after "fro".
    let out = "\x1b[?1049h\x1b[2J\x1b[H\x1b[1;3Hhello fro\x1b[?25l\x1b[?2026h\x1b[1;12Hm\x1b[1;14H\x1b[1mthe\x1b[0m \
               \x1b]8;;https://x\x1b\\日\x1b]8;;\x1b\\本\x1b[2;1Hgone\x1b[2;1H\x1b[K\x1b[3;5Hx\x1b[2Dy";
    let rows = screen(out, 20, 3);
    assert_eq!(rows, ["  hello from the 日本", "", "   yx"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tui_sends_a_prompt_shows_the_reply_switches_the_model_and_exits_0() {
    let Some(t) = child() else { return };
    let fake = FakeServer::with_models(
        vec![Reply::text("hello from the fake server")],
        vec![json!({ "id": "m" }), json!({ "id": "m2" }), json!({ "id": "other" })],
    )
    .expect("the fake server");
    let sink = sink();
    configure(&t, &fake, &sink, json!({}));
    let h = harness(&t, None).await;
    let ui = Ui::start(&h);
    ui.send("hi").await;
    ui.until(0, "hello from the fake server").await;
    assert_eq!(fake.requests().len(), 1);
    assert_eq!(fake.requests()[0]["model"], "m");

    // /model alone: the picker lists what the provider's server lists; typing filters it.
    let from = ui.term.mark();
    ui.send("/model").await;
    ui.until(from, "Choose a model").await;
    ui.until(from, "fake/m2").await;
    ui.typ("m2").await;
    ui.until(from, "filter: m2").await;
    ui.key("enter").await;
    ui.until(from, "Model: fake/m2").await;
    assert_eq!(h.agent.model().spec(), "fake/m2");
    // With the model named, no picker. (A name that starts another listed one would be completed
    // to it on enter, as in the TypeScript TUI: an exact match is left out of the completions.)
    let from = ui.term.mark();
    ui.send("/model fake/other").await;
    ui.until(from, "Model: fake/other").await;
    assert_eq!(h.agent.model().spec(), "fake/other");

    ui.send("/exit").await;
    let (code, term) = ui.exited().await;
    assert_eq!(code, 0);
    // The full screen is the default, and it was left.
    let out = term.output();
    assert!(out.starts_with("\x1b[?1049h"), "{:?}", &out[..out.len().min(40)]);
    assert!(out.contains("\x1b[?1049l"));
    assert_eq!(term.stops(), 1);
    // The session was saved, and the way back is printed after leaving.
    let session = h.agent.session().expect("a session");
    assert!(session.path().exists());
    let after = term.text(out.rfind("\x1b[?1049l").unwrap_or(0));
    assert!(after.contains(&format!("sasacode -r {}", session.id())), "{after}");
    // What was sent is kept for ↑ in the next session.
    let history = std::fs::read_to_string(&h.history_path).expect("the history file");
    assert_eq!(history, "\"hi\"\n\"/model\"\n\"/model fake/other\"\n\"/exit\"\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_is_continued_with_c_reopened_with_resume_and_forked() {
    let Some(t) = child() else { return };
    let fake =
        FakeServer::start(vec![Reply::text("first answer"), Reply::text("second answer")]).expect("the fake server");
    let sink = sink();
    // Inline, no bell: the tui settings reach the TUI.
    configure(
        &t,
        &fake,
        &sink,
        json!({ "tui": { "altScreen": false, "bell": false } }),
    );

    let h = harness(&t, None).await;
    let ui = Ui::start(&h);
    ui.send("remember this").await;
    ui.until(0, "first answer").await;
    ui.send("/exit").await;
    let (code, term) = ui.exited().await;
    assert_eq!(code, 0);
    assert!(
        !term.output().contains("\x1b[?1049h"),
        "inline mode never enters the alternate screen"
    );
    let first = h.agent.session().expect("a session").id().to_string();

    // -c: the conversation is on screen at once, and the next prompt goes with it.
    let h = harness(&t, Some("last")).await;
    assert_eq!(h.agent.session().map(|s| s.id().to_string()), Some(first.clone()));
    let ui = Ui::start(&h);
    ui.until(0, "first answer").await;
    assert!(ui.term.text(0).contains("remember this"));
    ui.send("and then?").await;
    ui.until(0, "second answer").await;
    assert!(text_of(&fake.requests()[1]).contains("remember this"));
    // ctrl+d on an empty input leaves too.
    ui.key("ctrl+d").await;
    let (code, term) = ui.exited().await;
    assert_eq!(code, 0);
    assert_eq!(term.stops(), 1);

    // /resume in a new session: the picker lists the earlier one, and choosing it shows it.
    let h = harness(&t, None).await;
    assert_ne!(h.agent.session().map(|s| s.id().to_string()), Some(first.clone()));
    let ui = Ui::start(&h);
    ui.send("/resume").await;
    ui.until(0, "Session to resume").await;
    let from = ui.term.mark();
    ui.key("enter").await;
    ui.until(from, "Session resumed").await;
    ui.until(from, "second answer").await;
    assert_eq!(h.agent.session().map(|s| s.id().to_string()), Some(first.clone()));
    assert_eq!(h.agent.messages().len(), 4);

    // /fork from the newest message: a new session with what came before it.
    let from = ui.term.mark();
    ui.send("/fork").await;
    ui.until(from, "Branch from which message?").await;
    ui.key("enter").await;
    ui.until(from, "Branched. Edit the message and send it").await;
    let forked = h.agent.session().expect("a session");
    assert_ne!(forked.id(), first);
    assert!(forked.path().exists());
    assert_eq!(h.agent.messages().len(), 2);
    ui.key("ctrl+c").await; // clears the message put back in the input
    ui.send("/exit").await;
    assert_eq!(ui.exited().await.0, 0);
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("the file").permissions().mode() & 0o777
}

#[tokio::test(flavor = "multi_thread")]
async fn language_from_the_tui_is_saved_in_the_global_config_keeping_its_other_keys() {
    let Some(t) = child() else { return };
    let fake = FakeServer::start(Vec::new()).expect("the fake server");
    let sink = sink();
    let written = configure(&t, &fake, &sink, json!({ "thinking": "low", "tui": { "bell": true } }));
    let path = t.home.join("config.json");
    #[cfg(unix)]
    let before = mode(&path);
    let h = harness(&t, None).await;
    let ui = Ui::start(&h);
    ui.send("/language ja").await;
    ui.until(0, "表示言語: 日本語").await;

    // `{ ...readJson(path), lang }` written as JSON.stringify(v, null, 2) plus a newline: every key
    // kept in its place, lang added at the end.
    let mut expected = written.as_object().cloned().expect("an object");
    expected.insert("lang".into(), json!("ja"));
    let pretty = |v: &serde_json::Map<String, Value>| format!("{}\n", serde_json::to_string_pretty(v).expect("json"));
    assert_eq!(std::fs::read_to_string(&path).expect("config.json"), pretty(&expected));
    // An existing file keeps its mode, as writeFileSync's `mode` only applies to a new one.
    #[cfg(unix)]
    assert_eq!(mode(&path), before);

    // Switching back keeps the key where it is.
    let from = ui.term.mark();
    ui.send("/language en").await;
    ui.until(from, "Interface language: English").await;
    expected.insert("lang".into(), json!("en"));
    assert_eq!(std::fs::read_to_string(&path).expect("config.json"), pretty(&expected));
    ui.send("/exit").await;
    assert_eq!(ui.exited().await.0, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_newer_release_is_announced_in_the_transcript() {
    // Served from this process for the child (main.ts's notifyIfNewer before runTui).
    let releases = TestServer::start(|req| {
        if req.path == "/latest/download/SHA256SUMS" {
            let host = req.header("host").unwrap_or_default();
            Res::redirect(&format!("http://{host}/download/v9.9.9/SHA256SUMS"))
        } else {
            Res::text(404, "not here")
        }
    });
    let Some(t) = child_with(&[("SASACODE_DOWNLOAD_BASE", &releases.url())]) else {
        return;
    };
    let fake = FakeServer::start(Vec::new()).expect("the fake server");
    let sink = sink();
    configure(&t, &fake, &sink, json!({ "updateCheck": true }));
    let h = harness(&t, None).await;
    let ui = Ui::start(&h);
    ui.until(0, "a newer version is available: v9.9.9").await;
    ui.send("/exit").await;
    assert_eq!(ui.exited().await.0, 0);
}
