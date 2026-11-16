//! Draws the window in its main states without a display and writes each as a PNG under
//! `target/snapshots`, to look at:
//!
//!   cargo test -p sasacode-gui -- --ignored snapshots
//!
//! The states come from the recorded runs in `sasacode-protocol/tests/fixtures`, fed to the window
//! as a run's process would feed them. Nothing is compared: fonts differ from machine to machine.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use iced::{Settings, Size};
use iced_test::Simulator;
use sasacode_protocol::{Event, SessionEntry, SessionSummary, parse_event};

use crate::app::{App, Chat, Clip, Msg, Options, Tab};
use crate::diff::Diff;
use crate::i18n::Strings;
use crate::layout::Layout;
use crate::overlay::Overlay;
use crate::transcript::Transcript;

pub(crate) fn fixture(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sasacode-protocol/tests/fixtures")
        .join(name);
    std::fs::read_to_string(p).unwrap()
}

pub(crate) fn window(lang: &str) -> App {
    let mut app = App::blank(
        Options {
            program: "sasacode".into(),
            cwd: "/Users/me/src/orders-api".into(),
            session: None,
            layout: Layout::default(),
            prefs: crate::layout::Prefs::default(),
            layout_file: None,
        },
        Strings::get(lang),
    );
    let prompts = [
        ("再試行を指数バックオフにして", 40, 28),
        ("注文一覧のページングが2ページ目で重複する", 3 * 3600, 41),
        ("READMEのセットアップ手順を今の構成に合わせる", 26 * 3600, 12),
        ("CIでだけ落ちるタイムゾーンのテストを調べて", 3 * 86_400, 63),
    ];
    app.sessions = prompts
        .iter()
        .enumerate()
        .map(|(i, (prompt, ago, count))| SessionSummary {
            id: format!("0a1b2c3d-{i}"),
            path: PathBuf::from(format!("/sessions/{i}.jsonl")),
            cwd: "/Users/me/src/orders-api".into(),
            created_at: String::new(),
            modified: SystemTime::now() - Duration::from_secs(*ago),
            first_prompt: (*prompt).to_owned(),
            message_count: *count,
            model: None,
        })
        .collect();
    app
}

/// Feeds a recorded run to the window, up to the first event `stop` says yes to. Time passes
/// between the events, so durations have something to show.
pub(crate) fn play(app: &mut App, name: &str, prompt: &str, stop: impl Fn(&Event) -> bool) {
    let mut now = Instant::now();
    app.chat.session = Some("0a1b2c3d-0".into());
    app.chat.transcript.send(prompt, now);
    for line in fixture(name).lines() {
        let event = parse_event(line).unwrap();
        if stop(&event) {
            break;
        }
        now += match event {
            Event::MessageUpdate { .. } => Duration::from_millis(40),
            Event::ToolEnd { .. } => Duration::from_millis(2600),
            _ => Duration::from_millis(450),
        };
        app.chat.transcript.apply(&event, now);
    }
    app.now = now;
}

/// The code block of [`markdown_reply`], as it is copied.
pub(crate) const REPLY_CODE: &str =
    "await new Promise((r) => setTimeout(r, 200 * 2 ** attempt)); // 待ち時間\nif (attempt > 2) throw err;";

/// A finished exchange whose reply holds a bold word of kanji and kana, inline code, a list, a
/// code block and a link.
pub(crate) fn markdown_reply(app: &mut App) {
    let mut now = Instant::now();
    app.chat.session = Some("0a1b2c3d-0".into());
    app.chat.transcript.send("何を変えた？", now);
    for line in [
        r###"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"## 変更点\n\n待ち時間を**指数バックオフ**に変えました。`src/client.ts`の1行だけです。\n\n1. 再試行は2回までのまま\n2. 待ち時間は200ミリ秒から倍々\n\n```ts\nawait new Promise((r) => setTimeout(r, 200 * 2 ** attempt)); // 待ち時間\nif (attempt > 2) throw err;\n```\n\n詳しくは[MDNの解説](https://developer.mozilla.org/)を見てください。"}],"provider":"fake","model":"m","usage":{"input":1200,"output":60},"stopReason":"stop"}}"###,
        r#"{"type":"agent_end","cause":"done"}"#,
    ] {
        now += Duration::from_secs(2);
        app.chat.transcript.apply(&parse_event(line).unwrap(), now);
    }
    app.now = now;
}

/// What the last message of [`long_log`] says.
pub(crate) const LONG_LOG_LAST: &str = "最後にREADMEも直して";

/// The recorded conversation read back from its log twice over: longer than a window is tall. Its
/// last message is [`LONG_LOG_LAST`].
pub(crate) fn long_log(app: &mut App) {
    let log = fixture("work-session.jsonl");
    let twice = format!("{log}\n{}", log.replace("テストも足して", LONG_LOG_LAST));
    let entries: Vec<SessionEntry> = twice
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    app.chat.transcript = Transcript::from_entries(&entries);
    app.chat.loaded = true;
    app.chat.session = Some("0a1b2c3d-0".into());
}

fn shoot(app: &App, name: &str, size: (f32, f32)) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    // A file that is already there would be compared with, not written.
    for old in std::fs::read_dir(&dir).unwrap().flatten() {
        if old.file_name().to_string_lossy().starts_with(&format!("{name}-")) {
            std::fs::remove_file(old.path()).unwrap();
        }
    }
    let settings = Settings {
        default_font: crate::view::UI,
        ..Settings::default()
    };
    let mut ui = Simulator::with_size(settings, Size::new(size.0, size.1), app.view());
    let snapshot = ui.snapshot(&app.theme()).unwrap();
    assert!(snapshot.matches_image(dir.join(format!("{name}.png"))).unwrap());
}

const WIDE: (f32, f32) = (1280.0, 800.0);

#[test]
#[ignore = "writes pictures to look at; run it by name"]
fn snapshots() {
    let never = |_: &Event| false;

    // A conversation that has not started.
    shoot(&window("ja"), "01-blank", WIDE);

    // A run that waits for an approval of a command.
    let mut app = window("ja");
    // Up to where `echo one` would start: its approval waits.
    play(
        &mut app,
        "work.jsonl",
        "再試行を指数バックオフにして",
        |e| matches!(e, Event::ToolStart { call, .. } if call.input["command"] == "echo one"),
    );
    let _written = app.pretend_running();
    app.tab = Tab::Plan;
    shoot(&app, "02-approval-command", WIDE);

    // The same run, finished: its steps folded away, the changes in the inspector.
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", never);
    shoot(&app, "03-done", WIDE);

    // With the steps of the second exchange shown, the commands in the inspector, in the dark.
    app.set_dark(true);
    app.chat.folds.insert(1, true);
    app.tab = Tab::Commands;
    shoot(&app, "04-steps-dark", (1280.0, 1100.0));
    app.tab = Tab::Usage;
    app.insp = true;
    shoot(&app, "05-usage-dark", WIDE);

    // A run in the middle of a command, with a message sent meanwhile.
    let mut app = window("ja");
    play(&mut app, "stop.jsonl", "しばらく待って", |e| {
        matches!(e, Event::ToolEnd { .. })
    });
    let _written = app.pretend_running();
    app.chat.transcript.queue("やっぱり10秒でいい");
    app.now += Duration::from_secs(12);
    shoot(&app, "06-running", WIDE);

    // Interrupted; stopped by a plugin; an error.
    let mut app = window("ja");
    play(&mut app, "stop.jsonl", "しばらく待って", never);
    shoot(&app, "07-aborted", WIDE);
    let mut app = window("ja");
    play(&mut app, "loop.jsonl", "読んで", never);
    app.insp = false;
    shoot(&app, "08-plugin-stop", WIDE);
    let mut app = window("ja");
    play(&mut app, "error.jsonl", "こんにちは", never);
    app.insp = false;
    shoot(&app, "09-error", WIDE);

    // An approval of an edit: the card shows the change the call asks for.
    let mut app = window("ja");
    let mut now = Instant::now();
    app.chat.transcript.send("タイムアウトを30秒にして", now);
    for line in [
        r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"設定ファイルのタイムアウトを変更します。"},{"type":"tool_call","id":"call_e","name":"edit","input":{"path":"/Users/me/src/orders-api/config/http.json","old_string":"{\n  \"retries\": 2,\n  \"timeoutMs\": 10000\n}","new_string":"{\n  \"retries\": 2,\n  \"timeoutMs\": 30000\n}"}}],"provider":"fake","model":"m","usage":{"input":1200,"output":60},"stopReason":"tool_use"}}"#,
        r#"{"type":"approval_request","id":"approval-1","callId":"call_e","tool":"edit","args":{"path":"/Users/me/src/orders-api/config/http.json","old_string":"{\n  \"retries\": 2,\n  \"timeoutMs\": 10000\n}","new_string":"{\n  \"retries\": 2,\n  \"timeoutMs\": 30000\n}"},"reason":"rule ask: edit(config/**)","cwd":"/Users/me/src/orders-api"}"#,
    ] {
        now += Duration::from_secs(2);
        app.chat.transcript.apply(&parse_event(line).unwrap(), now);
    }
    app.now = now;
    let _written = app.pretend_running();
    app.insp = false;
    shoot(&app, "10-approval-edit", WIDE);

    // A reply with what a reply can hold: a bold word of kanji and kana, inline code, a list, a
    // code block and a link.
    let mut app = window("ja");
    markdown_reply(&mut app);
    app.insp = false;
    shoot(&app, "16-reply-markdown", WIDE);
    // Its code block copied: the button says so for a moment.
    let _ = app.update(Msg::Copy(Clip::Text(REPLY_CODE.into())));
    shoot(&app, "17-copied", (1280.0, 560.0));

    // A long conversation read back from its log in a short window: the newest is in view without
    // the window having scrolled it there.
    let mut app = window("ja");
    long_log(&mut app);
    app.insp = false;
    shoot(&app, "18-long-log", (1280.0, 520.0));
    // Scrolled back to read: the way to the newest.
    app.reading = true;
    shoot(&app, "19-reading-back", (1280.0, 520.0));

    // Both side panels widened by their edges, the inspector's still held: its edge is lit, and
    // the list's longer names show more of themselves.
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", |_| false);
    app.width = 1440.0;
    app.side_w = 340.0;
    app.insp_w = 520.0;
    app.drag = Some(crate::app::Edge::Inspector);
    shoot(&app, "20-resized-panels", (1440.0, 800.0));

    // A conversation read back from its log, in English, in a narrow window.
    let mut app = window("en");
    let entries: Vec<SessionEntry> = fixture("work-session.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    app.chat.transcript = Transcript::from_entries(&entries);
    app.chat.loaded = true;
    app.chat.session = Some("0a1b2c3d-0".into());
    app.chat.folds.insert(1, true);
    app.width = 1000.0;
    shoot(&app, "11-loaded-en-narrow", (1000.0, 900.0));
    app.insp = false;
    app.width = 760.0;
    app.side = false;
    shoot(&app, "12-loaded-en-small", (760.0, 700.0));

    // The input at its fullest, a run that waits for an answer, where the room is least: in
    // English, at the width where both panels still show, and in the smallest window.
    for (lang, name, size) in [
        ("en", "13-approval-en", WIDE),
        ("ja", "14-approval-tight", (1180.0, 800.0)),
        ("ja", "15-approval-smallest", (760.0, 520.0)),
    ] {
        let mut app = window(lang);
        play(
            &mut app,
            "work.jsonl",
            "再試行を指数バックオフにして",
            |e| matches!(e, Event::ToolStart { call, .. } if call.input["command"] == "echo one"),
        );
        let _written = app.pretend_running();
        app.tab = Tab::Commands;
        app.width = size.0;
        shoot(&app, name, size);
    }

    // What opens over a conversation: the command palette, as opened and as typed in, the
    // settings and the list of keys.
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", never);
    app.overlay = Some(Overlay::palette());
    shoot(&app, "21-palette", WIDE);
    app.overlay = Some(Overlay::Palette {
        query: "テーマ".into(),
        at: 1,
    });
    shoot(&app, "22-palette-found", WIDE);
    app.overlay = Some(Overlay::Settings);
    shoot(&app, "23-settings", WIDE);
    app.overlay = Some(Overlay::Keys);
    shoot(&app, "24-keys", (1280.0, 900.0));

    // `@` offers the project's files, `/` the window's commands.
    let mut app = window("ja");
    app.files = [
        "src/client.ts",
        "src/client.test.ts",
        "docs/clients.md",
        "README.md",
        "src/",
    ]
    .map(String::from)
    .to_vec();
    app.files_asked = true;
    type_in(&mut app, "ここを見て @cli");
    app.insp = false;
    shoot(&app, "25-mention", (1280.0, 640.0));
    type_in(&mut app, "/");
    shoot(&app, "26-slash", (1280.0, 800.0));

    // The git tab: the branch ahead of its upstream, a changed file and a new one, and how full
    // the conversation is in the status bar.
    let mut app = window("ja");
    app.chat.transcript.apply(
        &parse_event(r#"{"type":"session","id":"0a1b2c3d-0","path":"/s.jsonl","contextWindow":64000}"#).unwrap(),
        app.now,
    );
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", never);
    app.git = crate::git::Status {
        repo: true,
        top: "/Users/me/src/orders-api".into(),
        branch: Some("retry-backoff".into()),
        upstream: true,
        ahead: 2,
        files: vec![
            crate::git::File {
                path: "src/client.ts".into(),
                index: 'M',
                tree: ' ',
                diff: Diff::parse(
                    "@@ -10,4 +10,4 @@\n   for (let attempt = 0; ; attempt++) {\n-    await sleep(200);\n+    await sleep(200 * 2 ** attempt);\n     if (attempt > 2) throw err;\n   }\n",
                ),
            },
            crate::git::File {
                path: "notes/retry.md".into(),
                index: '?',
                tree: '?',
                diff: Diff::create("# 再試行\n\n待ち時間は倍々にする。\n"),
            },
        ],
    };
    app.tab = Tab::Git;
    app.commit_msg = "再試行を指数バックオフに".into();
    shoot(&app, "27-git", WIDE);

    // The list with a conversation named, one pinned, one archived away, one with news, and the
    // pointer on another; the last message found by what it says; a message's actions on hover.
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", never);
    app.prefs.names.insert("0a1b2c3d-2".into(), "README整備".into());
    app.prefs.pinned = vec!["0a1b2c3d-3".into()];
    app.prefs.archived.insert("0a1b2c3d-1".into());
    let mut news = Chat::new(7);
    news.session = Some("0a1b2c3d-2".into());
    news.unread = true;
    app.parked.push(news);
    app.row_hover = Some("0a1b2c3d-3".into());
    app.find_in = Some("テスト".into());
    app.ex_hover = Some(0);
    app.insp = false;
    shoot(&app, "28-list-and-find", WIDE);

    // A side question, answered over the conversation.
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", never);
    let mut side = Chat::new(crate::overlay::SIDE);
    let mut now = app.now;
    side.transcript.send("リトライの上限はどこで決めてる？", now);
    for line in [
        r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"`src/client.ts`の`attempt > 2`です。3回目の失敗でそのまま投げます。"}],"provider":"fake","model":"m","usage":{"input":900,"output":30},"stopReason":"stop"}}"#,
        r#"{"type":"agent_end","cause":"done"}"#,
    ] {
        now += Duration::from_secs(2);
        side.transcript.apply(&parse_event(line).unwrap(), now);
    }
    app.aside = Some(side);
    app.overlay = Some(Overlay::Side);
    shoot(&app, "29-side-question", WIDE);
}

/// `text` in the input, the cursor at its end, as if typed.
fn type_in(app: &mut App, text: &str) {
    use iced::widget::text_editor::{Action, Content, Motion};
    app.chat.editor = Content::with_text(text);
    app.chat.editor.perform(Action::Move(Motion::DocumentEnd));
    app.dismissed = false;
    app.pick = 0;
}
