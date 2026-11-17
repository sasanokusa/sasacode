//! Uses the window without a display and without a process: clicks and keys go in the way a
//! user's do, and what the window would write to its run comes out of a channel.

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::text_editor;
use iced::{Settings, Size};
use iced_test::Simulator;
use sasacode_protocol::{ControlMessage, Decision, Event};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{App, Clip, Edge, FIND_IN, INPUT, INSP_W, Msg, SEARCH, SIDE_W, Tab, dragging, global_key, shortcut};
use crate::input::Command;
use crate::snapshots::{LONG_LOG_LAST, REPLY_CODE, long_log, markdown_reply, play, window};
use crate::transcript::{Answer, Kind, Note, Step};

/// The window as it is drawn, for `act` to click and type in; then the window is told what that
/// produced. Tall enough that nothing of the conversation is scrolled out of reach.
fn act(app: &mut App, act: impl FnOnce(&mut Simulator<'_, Msg>)) {
    let settings = Settings {
        default_font: crate::view::ui(),
        ..Settings::default()
    };
    let produced: Vec<Msg> = {
        let mut ui = Simulator::with_size(settings, Size::new(app.width, 1400.0), app.view());
        act(&mut ui);
        ui.into_messages().collect()
    };
    for msg in produced {
        let _ = app.update(msg);
    }
}

/// A run that waits for the user to answer about `echo one`.
fn waiting() -> (App, UnboundedReceiver<ControlMessage>, String) {
    let mut app = window("ja");
    play(
        &mut app,
        "work.jsonl",
        "再試行を指数バックオフにして",
        |e| matches!(e, Event::ToolStart { call, .. } if call.input["command"] == "echo one"),
    );
    let written = app.pretend_running();
    let id = app
        .chat
        .transcript
        .pending_approval()
        .expect("an approval waits")
        .id
        .clone();
    (app, written, id)
}

fn answer_of(app: &App, id: &str) -> Option<Answer> {
    app.chat
        .transcript
        .exchanges
        .iter()
        .flat_map(|e| &e.steps)
        .find_map(|step| match step {
            crate::transcript::Step::Approval(a) if a.id == id => a.answer.clone(),
            _ => None,
        })
}

#[test]
fn the_buttons_of_the_card_answer_the_approval_that_waits() {
    for (decision, answer) in [
        (Decision::Allow, Answer::Allowed),
        (Decision::Always, Answer::Always),
        (Decision::Deny, Answer::Denied(String::new())),
    ] {
        let (mut app, mut written, id) = waiting();
        let s = app.s;
        let label = match decision {
            Decision::Allow => s.allow,
            Decision::Always => s.always_cmd,
            Decision::Deny => s.deny,
        };
        act(&mut app, |ui| {
            ui.click(label).expect("the button is there");
        });
        assert_eq!(
            written.try_recv().ok(),
            Some(ControlMessage::Approval {
                id: id.clone(),
                decision,
                feedback: None,
                args: None
            })
        );
        assert_eq!(answer_of(&app, &id), Some(answer));
        // Answered once: nothing waits, and no other key or click can answer it again.
        assert!(app.chat.transcript.pending_approval().is_none());
        let _ = app.update(Msg::CommandEnter { shift: false });
        assert!(written.try_recv().is_err());
    }
}

#[test]
fn a_reply_typed_while_an_approval_waits_denies_it_and_says_why() {
    let (mut app, mut written, id) = waiting();
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.typewrite("それは不要");
        ui.tap_key(Named::Enter);
    });
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            id: id.clone(),
            decision: Decision::Deny,
            feedback: Some("それは不要".into()),
            args: None
        })
    );
    assert_eq!(answer_of(&app, &id), Some(Answer::Denied("それは不要".into())));
    assert!(app.chat.editor.text().trim().is_empty());
}

#[test]
fn the_chord_allows_only_when_nothing_is_typed() {
    let (mut app, mut written, id) = waiting();
    let _ = app.update(Msg::CommandEnter { shift: false });
    assert!(matches!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            decision: Decision::Allow,
            ..
        })
    ));
    assert_eq!(answer_of(&app, &id), Some(Answer::Allowed));

    // With a reply typed it is the reply that counts: a denial with its reason.
    let (mut app, mut written, id) = waiting();
    app.chat.editor = text_editor::Content::with_text("先にテストを書いて");
    let _ = app.update(Msg::CommandEnter { shift: false });
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            id,
            decision: Decision::Deny,
            feedback: Some("先にテストを書いて".into()),
            args: None
        })
    );
}

/// Whether the window as it is drawn has `label` in it.
fn says(app: &App, label: &str) -> bool {
    let settings = Settings {
        default_font: crate::view::ui(),
        ..Settings::default()
    };
    let mut ui = Simulator::with_size(settings, Size::new(app.width, 1400.0), app.view());
    ui.find(label).is_ok()
}

/// With Enter set to break the line the chord is ⌘⇧↵, and the card says so: it said ⌘↵, which
/// sends there, and an empty ⌘↵ allows nothing.
#[test]
fn with_enter_breaking_the_line_the_card_names_the_chord_that_allows() {
    let (mut app, mut written, id) = waiting();
    app.prefs.enter_sends = false;
    assert!(says(&app, &format!("{}⇧↵で許可", crate::view::MOD)));
    assert!(!says(&app, &format!("{}↵で許可", crate::view::MOD)));
    let _ = app.update(Msg::CommandEnter { shift: false });
    assert!(written.try_recv().is_err());
    let _ = app.update(Msg::CommandEnter { shift: true });
    assert!(matches!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            decision: Decision::Allow,
            ..
        })
    ));
    assert_eq!(answer_of(&app, &id), Some(Answer::Allowed));
}

/// A reply that broke off says why with the steps hidden too. Read from a log, the exchange has no
/// end of its run, and its error was one of the hidden steps: nothing was shown under the message.
#[test]
fn with_the_steps_hidden_a_reply_cut_short_still_says_why() {
    let mut app = window("ja");
    app.prefs.steps = false;
    let lines = [
        serde_json::json!({"type":"session","version":3,"id":"aaaa0000","cwd":"/w","createdAt":"2026-01-01T00:00:00.000Z"}),
        serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"ゆっくり答えて"}],"timestamp":1}}),
        serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"ゆっくり"}],"provider":"p","model":"m","usage":{"input":1,"output":1},"stopReason":"error","errorMessage":"the stream ended before the reply was complete","timestamp":2}}),
    ];
    let entries: Vec<sasacode_protocol::SessionEntry> = lines
        .iter()
        .map(|l| serde_json::from_value(l.clone()).unwrap())
        .collect();
    app.chat.transcript = crate::transcript::Transcript::from_entries(&entries);
    assert!(says(&app, "the stream ended before the reply was complete"));
    // With the steps shown it is among them, opened.
    app.prefs.steps = true;
    app.chat.folds.insert(0, true);
    assert!(says(&app, "the stream ended before the reply was complete"));

    // Answered after all, the exchange ended well: the error is one of the hidden steps again.
    let mut lines = lines.to_vec();
    lines.push(serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"最後まで答えました"}],"provider":"p","model":"m","usage":{"input":1,"output":1},"stopReason":"stop","timestamp":3}}));
    let entries: Vec<sasacode_protocol::SessionEntry> = lines
        .iter()
        .map(|l| serde_json::from_value(l.clone()).unwrap())
        .collect();
    app.chat.transcript = crate::transcript::Transcript::from_entries(&entries);
    app.chat.folds.clear();
    app.prefs.steps = false;
    assert!(app.chat.transcript.exchanges[0].answer().is_some());
    assert!(!says(&app, "the stream ended before the reply was complete"));
}

#[test]
fn no_bare_key_is_a_shortcut() {
    let none = Modifiers::empty();
    assert!(matches!(
        shortcut(&Key::Named(Named::Enter), Modifiers::COMMAND),
        Some(Msg::CommandEnter { shift: false })
    ));
    assert!(shortcut(&Key::Named(Named::Enter), none).is_none());
    for letter in ["y", "n", "a", "b", "i"] {
        assert!(shortcut(&Key::Character(letter.into()), none).is_none(), "{letter}");
    }
    assert!(matches!(shortcut(&Key::Named(Named::Escape), none), Some(Msg::Stop)));
    assert!(matches!(
        shortcut(&Key::Character("b".into()), Modifiers::COMMAND),
        Some(Msg::ToggleSide)
    ));
    assert!(matches!(
        shortcut(&Key::Character("i".into()), Modifiers::COMMAND),
        Some(Msg::ToggleInspector)
    ));
    assert!(matches!(
        shortcut(&Key::Character("n".into()), Modifiers::COMMAND),
        Some(Msg::NewSession)
    ));
    // The search, the way back to the input, and the inspector's tabs.
    for key in ["k", "l", "1", "2", "3", "4"] {
        assert!(shortcut(&Key::Character(key.into()), none).is_none(), "{key}");
    }
    let chord = |key: &str| shortcut(&Key::Character(key.into()), Modifiers::COMMAND);
    assert!(matches!(chord("k"), Some(Msg::Find)));
    assert!(matches!(chord("l"), Some(Msg::FocusInput)));
    assert!(matches!(chord("1"), Some(Msg::Tab(Tab::Changes))));
    assert!(matches!(chord("2"), Some(Msg::Tab(Tab::Plan))));
    assert!(matches!(chord("3"), Some(Msg::Tab(Tab::Commands))));
    assert!(matches!(chord("4"), Some(Msg::Tab(Tab::Usage))));
    // ⌘O: the system's folder picker; what is chosen opens in a window of its own, the folder
    // shown or nothing chosen in none.
    assert!(matches!(
        chord("o"),
        Some(Msg::Command(crate::input::Command::OpenFolder))
    ));
    // Esc stops whatever else is held with it.
    assert!(matches!(
        shortcut(&Key::Named(Named::Escape), Modifiers::COMMAND),
        Some(Msg::Stop)
    ));
}

#[test]
fn a_code_block_is_copied_by_its_button_which_says_so_for_a_moment() {
    let mut app = window("ja");
    markdown_reply(&mut app);
    let s = app.s;
    // The first copy button is the code block's: it comes before the reply's, under it.
    act(&mut app, |ui| {
        ui.click(s.copy)
            .expect("a finished reply's code block has a copy button");
    });
    assert_eq!(app.copied, Some(Clip::Text(REPLY_CODE.into())));
    act(&mut app, |ui| {
        ui.find(s.copied).expect("the button says it copied");
    });
    // Only the last copy's time running out clears what the button says.
    let _ = app.update(Msg::Copy(Clip::Reply(0)));
    let _ = app.update(Msg::Copied(1));
    assert_eq!(app.copied, Some(Clip::Reply(0)));
    let _ = app.update(Msg::Copied(2));
    assert_eq!(app.copied, None);
}

#[test]
fn what_is_copied_is_the_source_not_what_is_shown() {
    let mut app = window("ja");
    markdown_reply(&mut app);
    let reply = app.copy_text(&Clip::Reply(0)).expect("the reply");
    assert!(reply.starts_with("## 変更点") && reply.contains("```ts") && reply.contains("**指数バックオフ**"));
    assert_eq!(app.copy_text(&Clip::Session).as_deref(), Some("0a1b2c3d-0"));
    // A link that opens the window here, on this conversation.
    let link = app.copy_text(&Clip::Link).unwrap();
    let to = crate::link::parse(&link).unwrap();
    assert_eq!(
        (to.folder.as_deref(), to.session.as_deref()),
        (Some(app.cwd.as_path()), Some("0a1b2c3d-0"))
    );

    // A command without the `$ ` it is shown with, and all it wrote.
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", |_| false);
    let bash = app
        .chat
        .transcript
        .exchanges
        .iter()
        .flat_map(|e| &e.steps)
        .find_map(|step| match step {
            Step::Tool(tool) if tool.kind() == Kind::Bash => Some(tool),
            _ => None,
        })
        .expect("the run ran a command");
    assert_eq!(
        app.copy_text(&Clip::Command(bash.id.clone())).as_deref(),
        bash.arg("command")
    );
    assert_eq!(
        app.copy_text(&Clip::Output(bash.id.clone())).as_deref(),
        Some(bash.shown_output())
    );
    // Nothing to copy is no copy.
    assert_eq!(app.copy_text(&Clip::Command("no-such-call".into())), None);
    assert_eq!(app.copy_text(&Clip::Reply(99)), None);

    // An error, whole.
    let mut app = window("ja");
    play(&mut app, "error.jsonl", "こんにちは", |_| false);
    app.chat
        .transcript
        .note(Note::Error("sasacodeが途中で終了しました\nline two".into()));
    let (i, j) = app
        .chat
        .transcript
        .exchanges
        .iter()
        .enumerate()
        .find_map(|(i, e)| {
            e.steps
                .iter()
                .position(|step| matches!(step, Step::Note(Note::Error(_))))
                .map(|j| (i, j))
        })
        .expect("an error note");
    assert!(app.copy_text(&Clip::Step(i, j)).is_some_and(|text| text.contains('\n')));
}

#[test]
fn a_long_conversation_opens_on_its_newest_without_being_scrolled_there() {
    let mut app = window("ja");
    long_log(&mut app);
    app.insp = false;
    let s = app.s;
    let settings = Settings {
        default_font: crate::view::ui(),
        ..Settings::default()
    };
    let mut ui = Simulator::with_size(settings, Size::new(1280.0, 700.0), app.view());
    let first = ui.find(s.loaded_note).expect("the note over a loaded conversation");
    assert!(first.visible_bounds().is_none(), "the oldest is out of view");
    let last = ui.find(LONG_LOG_LAST).expect("the last message");
    assert!(last.visible_bounds().is_some(), "the newest is in view");
}

#[test]
fn reading_back_holds_the_conversation_by_its_start_until_the_newest_is_reached() {
    let mut app = window("ja");
    long_log(&mut app);
    let s = app.s;
    let scrolled = |app: &mut App, offset: f32| {
        let _ = app.update(Msg::Scrolled { offset, room: 2000.0 });
    };
    // Held by its end: a nudge is not reading back.
    scrolled(&mut app, 10.0);
    assert!(!app.reading);
    scrolled(&mut app, 300.0);
    assert!(app.reading);
    // Until the scroll that keeps the place is made, what is reported is measured from the end it
    // was held by before, and is not listened to.
    scrolled(&mut app, 1990.0);
    assert!(app.reading);
    let _ = app.update(Msg::Settled);
    // Held by its start now: 300 from the end is still reading back; 10 from it is the newest.
    scrolled(&mut app, 1700.0);
    assert!(app.reading);
    scrolled(&mut app, 1990.0);
    assert!(!app.reading);
    let _ = app.update(Msg::Settled);

    // The button over it goes back to the newest.
    scrolled(&mut app, 300.0);
    let _ = app.update(Msg::Settled);
    assert!(app.reading);
    act(&mut app, |ui| {
        ui.click(s.latest)
            .expect("reading back, the way to the newest is shown");
    });
    assert!(!app.reading);
    let _ = app.update(Msg::Settled);

    // So does a new conversation.
    scrolled(&mut app, 300.0);
    let _ = app.update(Msg::Settled);
    let _ = app.update(Msg::NewSession);
    assert!(!app.reading);
}

#[test]
fn a_conversation_copied_by_a_command_says_so_in_the_bar() {
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", |_| false);
    let s = app.s;
    let _ = app.update(Msg::Command(Command::Export));
    assert_eq!(app.copied, Some(Clip::Conversation));
    act(&mut app, |ui| {
        ui.find(s.copied).expect("the bar says it was copied");
    });
}

#[test]
fn a_named_conversation_is_called_by_its_name_above_it_too() {
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", |_| false);
    let _ = app.update(Msg::Rename("0a1b2c3d-0".into()));
    let _ = app.update(Msg::OverlayText("バックオフの件".into()));
    let _ = app.update(Msg::RenameSave);
    // Not only the list: the list is put away, and the head of the conversation says it.
    app.side = false;
    act(&mut app, |ui| {
        ui.find("バックオフの件").expect("the head says the name it was given");
    });
}

/// Esc pressed and let go, and what the window's own keys make of it after the widgets have had
/// it: taken by a field, or not.
fn escape(ui: &mut Simulator<'_, Msg>) -> Option<Msg> {
    use iced::keyboard::{Event as K, Location, key::NativeCode, key::Physical};
    let status = ui.tap_key(Named::Escape);
    let key = Key::Named(Named::Escape);
    let pressed = iced::Event::Keyboard(K::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: Physical::Unidentified(NativeCode::Unidentified),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    });
    global_key(pressed, status, iced::window::Id::unique())
}

#[test]
fn one_esc_in_the_search_of_the_conversation_puts_it_away() {
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", |_| false);
    let _ = app.update(Msg::Command(Command::Find));
    let mut heard = None;
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(FIND_IN)).expect("the search is open");
        ui.typewrite("テスト");
        heard = escape(ui);
    });
    assert_eq!(app.find_in.as_deref(), Some("テスト"));
    // The field took the Esc for itself; the window hears that it did, and puts the search away.
    assert!(matches!(heard, Some(Msg::Escaped)), "{heard:?}");
    let _ = app.update(heard.unwrap());
    assert_eq!(app.find_in, None);

    // Typed in the input since, an Esc that another field took leaves the search open; the next,
    // the window's own, puts it away.
    let _ = app.update(Msg::Command(Command::Find));
    let _ = app.update(Msg::FindInText("テスト".into()));
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.typewrite("a");
    });
    let _ = app.update(Msg::Escaped);
    assert!(app.find_in.is_some());
    let _ = app.update(Msg::Stop);
    assert_eq!(app.find_in, None);

    // Gone to the list's search with ⌘K, nothing typed yet: an Esc there is the search's.
    let _ = app.update(Msg::Command(Command::Find));
    let _ = app.update(Msg::Find);
    let _ = app.update(Msg::Escaped);
    assert!(app.find_in.is_some());
}

/// ⌘ and `key`, pressed and let go, as macOS sends them: the key comes with its letter as text.
fn command_chord(letter: &str) -> Vec<iced::Event> {
    pressed(Key::Character(letter.into()), Modifiers::COMMAND, letter)
}

/// `key` pressed and let go with `modifiers` held, coming with `text`.
fn pressed(key: Key, modifiers: Modifiers, text: &str) -> Vec<iced::Event> {
    use iced::keyboard::{Event as K, Location, key::NativeCode, key::Physical};
    let physical = Physical::Unidentified(NativeCode::Unidentified);
    vec![
        iced::Event::Keyboard(K::ModifiersChanged(modifiers)),
        iced::Event::Keyboard(K::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: physical,
            location: Location::Standard,
            modifiers,
            text: Some(text.into()),
            repeat: false,
        }),
        iced::Event::Keyboard(K::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key: physical,
            location: Location::Standard,
            modifiers,
        }),
        iced::Event::Keyboard(K::ModifiersChanged(Modifiers::empty())),
    ]
}

/// With Enter set to break the line, ⌘↵ in the input adds to the run that works, and only ⌘⇧↵
/// stops it and sends now. ⌘↵ used to be taken by the window as the chord: it stopped the run.
#[test]
fn with_enter_breaking_the_line_the_command_enter_adds_to_the_run_and_does_not_stop_it() {
    let mut app = window("ja");
    app.prefs.enter_sends = false;
    play(&mut app, "stop.jsonl", "しばらく待って", |e| {
        matches!(e, Event::ToolEnd { .. })
    });
    let mut written = app.pretend_running();
    let enter = || Key::Named(Named::Enter);
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.typewrite("一行目");
        ui.simulate(pressed(enter(), Modifiers::empty(), "\r"));
        ui.typewrite("二行目");
    });
    // Enter broke the line and sent nothing.
    assert!(written.try_recv().is_err());
    assert_eq!(app.chat.editor.text().trim(), "一行目\n二行目");
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.simulate(pressed(enter(), Modifiers::COMMAND, "\r"));
    });
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Message {
            text: "一行目\n二行目".into()
        })
    );
    assert!(written.try_recv().is_err(), "the run was stopped");
    assert!(app.running());

    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.typewrite("今すぐこれ");
        ui.simulate(pressed(enter(), Modifiers::COMMAND | Modifiers::SHIFT, "\r"));
    });
    assert_eq!(written.try_recv().ok(), Some(ControlMessage::Abort { call_id: None }));
    assert_eq!(app.chat.next.as_deref(), Some("今すぐこれ"));
}

#[test]
fn a_chord_pressed_in_the_search_is_the_windows_not_a_letter_typed() {
    let mut app = window("ja");
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(SEARCH)).expect("the search is there");
        ui.typewrite("ab");
    });
    assert_eq!(app.filter, "ab");
    // The search's own field would type the `l` of ⌘L, and the window would never see the chord.
    let mut seen = Vec::new();
    {
        let settings = Settings {
            default_font: crate::view::ui(),
            ..Settings::default()
        };
        let mut ui = Simulator::with_size(settings, Size::new(app.width, 1400.0), app.view());
        ui.click(iced::widget::Id::from(SEARCH)).expect("the search is there");
        ui.simulate(command_chord("l"));
        seen.extend(ui.into_messages());
    }
    assert!(seen.iter().any(|m| matches!(m, Msg::FocusInput)), "{seen:?}");
    assert!(!seen.iter().any(|m| matches!(m, Msg::Filter(_))), "{seen:?}");
    for msg in seen {
        let _ = app.update(msg);
    }
    assert_eq!(app.filter, "ab");
}

#[test]
fn the_search_is_reached_by_its_chord_even_where_the_list_is_hidden() {
    let mut app = window("ja");
    app.width = 1000.0;
    assert!(!app.shows_side());
    let _ = app.update(Msg::Find);
    assert!(app.shows_side() && !app.insp);
    // Typed into it, the list is what it finds.
    app.filter = "readme".into();
    assert_eq!(app.found().count(), 1);
    app.filter = "見当たらない".into();
    assert_eq!(app.found().count(), 0);
}

#[test]
fn what_is_sent_while_the_agent_works_goes_to_the_run_and_waits_in_view() {
    let mut app = window("ja");
    play(&mut app, "stop.jsonl", "しばらく待って", |e| {
        matches!(e, Event::ToolEnd { .. })
    });
    let mut written = app.pretend_running();
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.typewrite("やっぱり10秒でいい");
        ui.tap_key(Named::Enter);
    });
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Message {
            text: "やっぱり10秒でいい".into()
        })
    );
    assert_eq!(app.chat.transcript.queued, ["やっぱり10秒でいい"]);
    // Still the same exchange: it becomes one of its own when the model is given it.
    assert_eq!(app.chat.transcript.exchanges.len(), 1);

    let _ = app.update(Msg::Stop);
    assert_eq!(written.try_recv().ok(), Some(ControlMessage::Abort { call_id: None }));
}

#[test]
fn the_steps_fold_and_the_tabs_switch_by_a_click() {
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", |_| false);
    let s = app.s;
    assert!(!app.steps_open(0) && !app.steps_open(1));
    act(&mut app, |ui| {
        ui.click(s.show_steps)
            .expect("a finished exchange has its steps folded");
    });
    assert!(app.steps_open(0) && !app.steps_open(1));

    act(&mut app, |ui| {
        ui.click(s.tab_cmd).expect("the inspector has a commands tab");
    });
    assert_eq!(app.tab, Tab::Commands);

    // The footer of a finished run leads to what it changed, and opens the inspector for it.
    app.insp = false;
    act(&mut app, |ui| {
        ui.click(s.review).expect("a run that changed files says so");
    });
    assert!(app.insp && app.tab == Tab::Changes);
}

#[test]
fn a_panels_edge_dragged_sets_its_width_as_far_as_the_conversation_has_room() {
    let mut app = window("ja");
    assert_eq!(app.width, 1280.0);
    let _ = app.update(Msg::Grab(Edge::Side));
    let _ = app.update(Msg::Drag(300.0));
    assert_eq!(app.side_w, 300.0);
    // No wider than leaves the conversation its room next to the inspector: 1280 - 380 - 2 - 554.
    let _ = app.update(Msg::Drag(900.0));
    assert_eq!(app.side_w, 344.0);
    assert!(app.shows_side() && app.insp);
    // No narrower than its narrowest, short of folding.
    let _ = app.update(Msg::Drag(150.0));
    assert_eq!(app.side_w, 180.0);
    let _ = app.update(Msg::Drop);
    // Let go, the pointer moves nothing.
    let _ = app.update(Msg::Drag(260.0));
    assert_eq!(app.side_w, 180.0);

    // The inspector's edge is dragged from the right.
    let _ = app.update(Msg::Grab(Edge::Inspector));
    let _ = app.update(Msg::Drag(1280.0 - 1.0 - 420.0));
    let _ = app.update(Msg::Drop);
    assert_eq!(app.insp_w, 420.0);
    // The conversation has what the panels leave it.
    assert_eq!(crate::view::room(&app), 1280.0 - 181.0 - 421.0);

    // In a window too narrow for both, a narrower inspector makes room for the list.
    let mut app = window("ja");
    app.width = 1100.0;
    assert!(!app.shows_side());
    let _ = app.update(Msg::Grab(Edge::Inspector));
    let _ = app.update(Msg::Drag(1100.0 - 1.0 - 290.0));
    let _ = app.update(Msg::Drop);
    assert_eq!(app.insp_w, 290.0);
    assert!(app.shows_side() && app.insp);
}

#[test]
fn a_panel_dragged_past_its_narrowest_folds_and_opens_again_in_the_same_drag() {
    // As a real drag goes: through every place on the way, its narrowest among them.
    let drag = |app: &mut App, edge: Edge, path: &[f32]| {
        let _ = app.update(Msg::Grab(edge));
        for &x in path {
            let _ = app.update(Msg::Drag(x));
        }
    };
    let mut app = window("ja");
    app.side_w = 300.0;
    drag(&mut app, Edge::Side, &[260.0, 200.0, 170.0, 130.0, 100.0]);
    assert!(!app.shows_side());
    // It keeps the width it had when its edge was taken, not the narrowest it passed through.
    assert_eq!(app.side_w, 300.0);
    // Back out in the same drag, it opens where the pointer is.
    let _ = app.update(Msg::Drag(200.0));
    assert!(app.shows_side());
    assert_eq!(app.side_w, 200.0);
    let _ = app.update(Msg::Drag(40.0));
    let _ = app.update(Msg::Drop);
    assert!(!app.side);
    // Its button opens it at that width.
    let _ = app.update(Msg::ToggleSide);
    assert!(app.shows_side());
    assert_eq!(app.side_w, 300.0);

    // The inspector folds toward the window's right edge, likewise.
    drag(&mut app, Edge::Inspector, &[950.0, 1000.0, 1050.0, 1100.0, 1270.0]);
    let _ = app.update(Msg::Drop);
    assert!(!app.insp);
    let _ = app.update(Msg::ToggleInspector);
    assert!(app.insp);
    assert_eq!(app.insp_w, INSP_W);
}

#[test]
fn an_edge_is_taken_by_the_pointer_follows_it_and_is_let_go() {
    use iced::Point;
    use iced::mouse::{Button, Event as Mouse};
    let moved = |x: f32| {
        iced::Event::Mouse(Mouse::CursorMoved {
            position: Point::new(x, 300.0),
        })
    };
    let mut app = window("ja");
    // The list's line is at its width: the pointer on it takes the edge.
    act(&mut app, |ui| {
        ui.point_at((SIDE_W + 0.5, 300.0));
        ui.simulate([
            moved(SIDE_W + 0.5),
            iced::Event::Mouse(Mouse::ButtonPressed(Button::Left)),
        ]);
    });
    assert_eq!(app.drag, Some(Edge::Side));
    assert_eq!(app.edge_hover, Some(Edge::Side));
    // While it is held, the pointer is followed by what listens to the whole window, out of it
    // too: the simulator does not run it, so what it hears is given to it here.
    let window = iced::window::Id::unique();
    let hear = |app: &mut App, event: iced::Event| {
        if let Some(msg) = dragging(event, iced::event::Status::Ignored, window) {
            let _ = app.update(msg);
        }
    };
    let release = || iced::Event::Mouse(Mouse::ButtonReleased(Button::Left));
    hear(&mut app, moved(320.0));
    assert_eq!(app.side_w, 320.0);
    // Past the window's right, the list takes all the room the conversation leaves it.
    hear(&mut app, moved(2000.0));
    assert_eq!(app.side_w, 1280.0 - INSP_W - 2.0 - 554.0);
    // Let go past its left, the list is folded, and opens again at the width it was taken at.
    hear(&mut app, moved(-40.0));
    hear(&mut app, release());
    assert_eq!(app.drag, None);
    assert!(!app.side);
    let _ = app.update(Msg::ToggleSide);
    assert!(app.side);
    assert_eq!(app.side_w, SIDE_W);

    // A click on the edge quick enough to be let go before anything follows the drag ends it all
    // the same.
    act(&mut app, |ui| {
        ui.point_at((SIDE_W + 0.5, 300.0));
        ui.simulate([
            moved(SIDE_W + 0.5),
            iced::Event::Mouse(Mouse::ButtonPressed(Button::Left)),
            release(),
        ]);
    });
    assert_eq!(app.drag, None);

    // A double click on the inspector's edge gives it back the width it opened at.
    app.insp_w = 500.0;
    let line = 1280.0 - 500.0 - 1.0;
    act(&mut app, |ui| {
        ui.point_at((line + 0.5, 300.0));
        let press = || iced::Event::Mouse(Mouse::ButtonPressed(Button::Left));
        ui.simulate([moved(line + 0.5), press(), release(), press(), release()]);
    });
    assert_eq!(app.insp_w, INSP_W);
    assert_eq!(app.drag, None);
    // Off the edge, nothing is taken.
    act(&mut app, |ui| {
        ui.point_at((700.0, 300.0));
        ui.simulate([moved(700.0), iced::Event::Mouse(Mouse::ButtonPressed(Button::Left))]);
    });
    assert_eq!(app.drag, None);
}

#[test]
fn in_a_narrow_window_the_two_side_panels_take_turns() {
    let mut app = window("ja");
    app.width = 1000.0;
    assert!(app.insp && !app.shows_side());
    let _ = app.update(Msg::ToggleSide);
    assert!(app.shows_side() && !app.insp);
    let _ = app.update(Msg::ToggleInspector);
    assert!(app.insp && !app.shows_side());
    // Wide enough for both, both show.
    app.width = 1280.0;
    assert!(app.insp && app.shows_side());
}

/// What the input holds, all of it.
fn typed(app: &App) -> String {
    app.chat.editor.text().trim_end_matches('\n').to_owned()
}

#[test]
fn an_at_sign_offers_files_and_tab_takes_the_one_chosen() {
    let mut app = window("ja");
    app.files = ["src/client.ts", "src/client.test.ts", "docs/clients.md", "README.md"]
        .map(String::from)
        .to_vec();
    app.files_asked = true;
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.typewrite("見て @cli");
    });
    assert!(app.offer().is_some(), "{:?}", typed(&app));
    // The second of what is offered, chosen with the arrow and taken with Tab.
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.tap_key(Named::ArrowDown);
    });
    assert_eq!(app.pick, 1);
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.tap_key(Named::Tab);
    });
    assert_eq!(typed(&app), "見て @docs/clients.md ");
    assert!(app.offer().is_none());
}

#[test]
fn a_slash_command_is_run_from_the_input_by_enter() {
    let mut app = window("ja");
    assert!(app.prefs.steps);
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.typewrite("/ste");
    });
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.tap_key(Named::Enter);
    });
    // Run, not sent: the input is empty again and no run started.
    assert!(!app.prefs.steps);
    assert_eq!(typed(&app), "");
    assert!(app.chat.transcript.exchanges.is_empty());
}

#[test]
fn the_up_arrow_in_an_empty_input_brings_back_what_was_sent() {
    let mut app = window("ja");
    app.history = vec!["一つ目".into(), "二つ目".into()];
    for want in ["二つ目", "一つ目"] {
        act(&mut app, |ui| {
            ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
            ui.tap_key(Named::ArrowUp);
        });
        assert_eq!(typed(&app), want);
    }
}

#[test]
fn the_palette_finds_what_is_typed_and_runs_it_by_enter() {
    let mut app = window("ja");
    let _ = app.update(Msg::Show(Some(crate::overlay::Overlay::palette())));
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(crate::app::PALETTE))
            .expect("the palette's field is there");
        ui.typewrite("ダーク");
    });
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(crate::app::PALETTE))
            .expect("the palette's field is there");
        ui.tap_key(Named::Enter);
    });
    assert_eq!(app.prefs.dark, Some(true));
    assert!(app.overlay.is_none());
}

/// The files tab shows the project as a tree while nothing is typed, opened folder by folder; the
/// git tab lists the changed files under their folders, each folder folding its own.
#[test]
fn the_files_are_a_tree_and_the_changed_files_are_listed_by_folder() {
    let mut app = window("ja");
    app.files = ["src/", "src/retry.rs", "src/view/list.rs", "README.md"]
        .map(String::from)
        .to_vec();
    app.files_asked = true;
    app.tab = Tab::Files;
    assert!(says(&app, "src") && says(&app, "README.md"));
    assert!(!says(&app, "retry.rs"), "what is in a shut folder is out of sight");
    let _ = app.update(Msg::TreeFold("src/".into()));
    assert!(says(&app, "retry.rs") && says(&app, "view") && !says(&app, "list.rs"));
    let _ = app.update(Msg::FileQuery("list".into()));
    assert!(says(&app, "src/view/list.rs"), "what is typed finds files as before");

    let changed = |path: &str| crate::git::File {
        path: path.into(),
        index: ' ',
        tree: 'M',
        staged_diff: crate::diff::Diff::default(),
        unstaged_diff: crate::diff::Diff::parse("@@ -1 +1 @@\n-a\n+b\n"),
    };
    app.git = crate::git::Status {
        repo: true,
        top: app.cwd.clone(),
        branch: Some("topic".into()),
        files: vec![changed("src/retry.rs"), changed("README.md"), changed("src/backoff.rs")],
        ..Default::default()
    };
    app.tab = Tab::Git;
    assert!(says(&app, "src/") && says(&app, "(2)") && says(&app, "（最上位）"));
    assert!(says(&app, "retry.rs") && says(&app, "backoff.rs"));
    let _ = app.update(Msg::GitFold("src/".into()));
    assert!(!says(&app, "retry.rs") && says(&app, "README.md"));
}

#[test]
fn a_file_is_staged_by_its_button_and_discarded_only_on_the_second_press() {
    let mut app = window("ja");
    let s = app.s;
    app.git = crate::git::Status {
        repo: true,
        top: app.cwd.clone(),
        branch: Some("main".into()),
        files: vec![crate::git::File {
            path: "a.txt".into(),
            index: ' ',
            tree: 'M',
            staged_diff: crate::diff::Diff::default(),
            unstaged_diff: crate::diff::Diff::parse("@@ -1 +1 @@\n-a\n+b\n"),
        }],
        ..Default::default()
    };
    app.tab = Tab::Git;
    let pressed = |app: &App, label: &str| -> Vec<Msg> {
        let settings = Settings {
            default_font: crate::view::ui(),
            ..Settings::default()
        };
        let mut ui = Simulator::with_size(settings, Size::new(app.width, 1400.0), app.view());
        ui.click(label).expect("the button is there");
        ui.into_messages().collect()
    };
    let stage = pressed(&app, s.stage);
    assert!(
        matches!(&stage[..], [Msg::GitDo(crate::git::Op::Stage(p))] if p == "a.txt"),
        "{stage:?}"
    );
    // Discarding: the first press only asks again.
    for msg in pressed(&app, s.discard) {
        let _ = app.update(msg);
    }
    assert_eq!(app.confirm_discard.as_deref(), Some("a.txt"));
    assert!(!app.git_busy);
    for msg in pressed(&app, s.discard_sure) {
        let _ = app.update(msg);
    }
    assert!(app.confirm_discard.is_none() && app.git_busy);
}

#[test]
fn the_paste_chord_in_the_input_asks_for_the_clipboard_and_puts_in_what_it_has() {
    let mut app = window("ja");
    let mut seen = Vec::new();
    {
        let settings = Settings {
            default_font: crate::view::ui(),
            ..Settings::default()
        };
        let mut ui = Simulator::with_size(settings, Size::new(app.width, 1400.0), app.view());
        ui.click(iced::widget::Id::from(INPUT)).expect("the input is there");
        ui.simulate(command_chord("v"));
        seen.extend(ui.into_messages());
    }
    assert!(seen.iter().any(|m| matches!(m, Msg::Paste)), "{seen:?}");
    // Text goes in where the cursor is; with none, an image goes in as its path.
    let _ = app.update(Msg::Pasted(Some("貼った文".into())));
    assert_eq!(typed(&app), "貼った文");
    let _ = app.update(Msg::PastedImage(Some("/tmp/pasted-1.png".into())));
    assert_eq!(typed(&app), "貼った文 @/tmp/pasted-1.png ");
    let _ = app.update(Msg::PastedImage(None));
    assert_eq!(typed(&app), "貼った文 @/tmp/pasted-1.png ");
}

/// A repository with one file changed in two places, shown in the git tab.
fn changed_repo(app: &mut App) {
    app.git = crate::git::Status {
        repo: true,
        top: app.cwd.clone(),
        branch: Some("main".into()),
        files: vec![crate::git::File {
            path: "a.txt".into(),
            index: 'M',
            tree: 'M',
            staged_diff: crate::diff::Diff::parse("@@ -1 +1 @@\n-a\n+b\n"),
            unstaged_diff: crate::diff::Diff::parse("@@ -9 +9 @@\n-c\n+d\n"),
        }],
        ..Default::default()
    };
    app.tab = Tab::Git;
}

#[test]
fn a_comment_on_a_line_of_a_diff_goes_with_the_next_message_and_not_into_the_history() {
    let mut app = window("ja");
    let w = app.s.w();
    changed_repo(&mut app);
    // Staged and not, apart, each hunk with its own buttons.
    assert!(says(&app, w.staged_head) && says(&app, w.unstaged_head));
    let path = app.cwd.join("a.txt");
    let _ = app.update(Msg::CommentAt(path.clone(), 9, false));
    assert!(says(&app, w.comment_add));
    let _ = app.update(Msg::CommentText("  ここは逆では？ ".into()));
    let _ = app.update(Msg::CommentSave);
    assert!(app.commenting.is_none());
    assert_eq!(app.chat.comments.len(), 1);
    // Shown under its line and above the input, where it can be taken back.
    assert!(says(&app, w.comments_head) && says(&app, "ここは逆では？"));

    // An empty comment puts where the line is into the input instead, as a click did before.
    let _ = app.update(Msg::CommentAt(path, 1, true));
    let _ = app.update(Msg::CommentSave);
    assert_eq!(typed(&app), "@a.txt:1 ");

    let _ = app.update(Msg::Send);
    let sent = &app.chat.transcript.exchanges[0].prompt;
    assert_eq!(
        sent,
        &format!("@a.txt:1\n\n{}\n- @a.txt:9: ここは逆では？", w.comments_block)
    );
    assert!(app.chat.comments.is_empty());
    assert_eq!(app.history.last().map(String::as_str), Some("@a.txt:1"));
}

#[test]
fn comments_alone_can_be_sent() {
    let mut app = window("ja");
    changed_repo(&mut app);
    let _ = app.update(Msg::CommentAt(app.cwd.join("a.txt"), 9, false));
    let _ = app.update(Msg::CommentText("名前を変えて".into()));
    let _ = app.update(Msg::CommentSave);
    let _ = app.update(Msg::Send);
    assert!(
        app.chat.transcript.exchanges[0]
            .prompt
            .ends_with("- @a.txt:9: 名前を変えて")
    );
    assert!(app.history.is_empty());
}

#[test]
fn a_hunk_is_discarded_and_a_pull_request_merged_only_on_the_second_press() {
    let mut app = window("ja");
    changed_repo(&mut app);
    let discard = Msg::GitDo(crate::git::Op::Hunk {
        path: "a.txt".into(),
        act: crate::git::HunkAct::Discard,
        at: 0,
        head: "@@ -9 +9 @@".into(),
    });
    let _ = app.update(discard.clone());
    assert_eq!(app.confirm_discard.as_deref(), Some("a.txt#0"));
    assert!(says(&app, app.s.discard_sure) && !app.git_busy);
    let _ = app.update(discard);
    assert!(app.git_busy && app.confirm_discard.is_none());

    let mut app = window("ja");
    changed_repo(&mut app);
    app.pr = Some(crate::git::Pr {
        number: 7,
        title: "再試行".into(),
        state: "OPEN".into(),
        checks: vec![crate::git::Check {
            name: "rust".into(),
            state: crate::git::CheckState::Failed,
            url: String::new(),
        }],
        ..Default::default()
    });
    let w = app.s.w();
    assert!(says(&app, w.fix_checks) && says(&app, w.check_failed));
    let _ = app.update(Msg::GitDo(crate::git::Op::Merge("merge".into())));
    assert!(app.confirm_merge && !app.git_busy && says(&app, w.merge_sure));
    let _ = app.update(Msg::GitDo(crate::git::Op::Merge("merge".into())));
    assert!(app.git_busy && !app.confirm_merge);
}

#[test]
fn failed_checks_start_a_conversation_to_fix_them_when_the_settings_say_so() {
    use crate::git::CheckState::{Failed, Pending};
    let mut app = window("ja");
    app.prefs.notify = false;
    let pr = |state: crate::git::CheckState| crate::git::Pr {
        number: 9,
        title: "再試行".into(),
        url: "https://example.invalid/pull/9".into(),
        state: "OPEN".into(),
        checks: vec![crate::git::Check {
            name: "rust".into(),
            state,
            url: String::new(),
        }],
        ..Default::default()
    };
    // Off: told, nothing started.
    app.pr = Some(pr(Pending));
    let _ = app.update(Msg::PrLoaded(Some(Box::new(pr(Failed)))));
    assert!(app.auto_fixes.is_empty());
    // On: the logs are read for it, three times a pull request at most.
    let _ = app.update(Msg::SetAutoFix(true));
    assert!(app.prefs.more.auto_fix);
    for _ in 0..5 {
        let _ = app.update(Msg::PrLoaded(Some(Box::new(pr(Pending)))));
        let _ = app.update(Msg::PrLoaded(Some(Box::new(pr(Failed)))));
    }
    assert_eq!(app.auto_fixes[&9], crate::review::AUTO_FIXES);
    // The logs read: a conversation beside the one shown, given them, never in auto.
    app.permission = Some("auto");
    let shown = app.chat.key;
    let _ = app.update(Msg::AutoFixReady("error: missing module 'retry'".into()));
    assert_eq!(app.chat.key, shown, "the one shown stays");
    let fixing = app.parked.last().unwrap();
    assert!(fixing.busy());
    let asked = fixing
        .transcript
        .exchanges
        .last()
        .map(|e| e.prompt.clone())
        .unwrap_or_default();
    assert!(
        asked.contains("missing module 'retry'") && asked.contains("pull/9"),
        "{asked}"
    );
    assert_eq!(
        app.toast.as_deref(),
        Some(crate::i18n::fill(crate::words::JA.auto_fix_started, "n", 9).as_str())
    );
}

#[test]
fn a_pull_requests_running_checks_are_watched_and_their_end_is_told() {
    let mut app = window("ja");
    app.prefs.notify = false;
    let pr = |state: crate::git::CheckState| crate::git::Pr {
        number: 7,
        title: "再試行".into(),
        state: "OPEN".into(),
        checks: vec![crate::git::Check {
            name: "rust".into(),
            state,
            url: String::new(),
        }],
        ..Default::default()
    };
    use crate::git::CheckState::{Failed, Passed, Pending};
    app.pr = Some(pr(Pending));
    assert!(app.watches_checks());
    let _ = app.update(Msg::PrLoaded(Some(Box::new(pr(Pending)))));
    assert!(app.toast.is_none(), "still running: nothing to say");
    let _ = app.update(Msg::PrLoaded(Some(Box::new(pr(Passed)))));
    assert_eq!(app.toast.as_deref(), Some(app.s.w().checks_passed));
    assert!(!app.watches_checks(), "finished: no longer looked at");
    // Run again (a push), and failed.
    app.toast = None;
    let _ = app.update(Msg::PrLoaded(Some(Box::new(pr(Pending)))));
    let _ = app.update(Msg::PrLoaded(Some(Box::new(pr(Failed)))));
    assert_eq!(
        app.toast.as_deref(),
        Some(crate::i18n::fill(app.s.w().checks_failed, "n", 1).as_str())
    );
    // Another pull request read in its place says nothing of the first's checks.
    app.toast = None;
    app.pr = Some(pr(Pending));
    let other = crate::git::Pr {
        number: 8,
        ..pr(Passed)
    };
    let _ = app.update(Msg::PrLoaded(Some(Box::new(other))));
    assert!(app.toast.is_none());
}

#[test]
fn the_branch_list_checks_out_what_is_found_and_makes_a_new_one_only_by_its_whole_name() {
    let mut app = window("ja");
    changed_repo(&mut app);
    app.branch_list = Some(vec!["main".into(), "topic/retry".into()]);
    let _ = app.update(Msg::Command(Command::Branch));
    // Asked for again whenever the list opens.
    assert!(app.branch_list.is_none());
    app.branch_list = Some(vec!["main".into(), "topic/retry".into()]);
    let _ = app.update(Msg::OverlayText("retry".into()));
    let found: Vec<Msg> = app.found_entries().into_iter().map(|e| e.msg).collect();
    assert!(matches!(&found[..], [
        Msg::GitDo(crate::git::Op::Switch(b)),
        Msg::GitDo(crate::git::Op::NewBranch(n)),
    ] if b == "topic/retry" && n == "retry"));
    // Enter takes the first: the branch found, not a new one.
    let _ = app.update(Msg::PaletteRun(0));
    assert!(app.overlay.is_none() && app.git_busy);

    let mut app = window("ja");
    changed_repo(&mut app);
    app.overlay = Some(crate::overlay::Overlay::picker(crate::overlay::Pick::Branches));
    app.branch_list = Some(vec!["main".into()]);
    let _ = app.update(Msg::OverlayText("main".into()));
    assert_eq!(app.found_entries().len(), 1);
    let _ = app.update(Msg::OverlayText("two words".into()));
    assert!(app.found_entries().is_empty());
}

#[test]
fn conversations_whose_pull_request_closed_are_archived_when_the_settings_say_so() {
    let mut app = window("ja");
    let _ = app.update(Msg::SetAutoArchive(true));
    let _ = app.update(Msg::Swept(vec!["a".into(), "b".into()]));
    assert!(app.prefs.archived.contains("a") && app.prefs.archived.contains("b"));
    assert!(app.toast.as_deref().is_some_and(|t| t.contains('2')));
}

#[test]
fn a_conversation_is_deleted_only_on_the_second_press_and_never_while_it_runs() {
    let mut app = window("ja");
    let dir = std::env::temp_dir().join(format!("sasacode-gui-delete-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (gone, kept) = (dir.join("gone.jsonl"), dir.join("kept.jsonl"));
    std::fs::write(&gone, "{}\n").unwrap();
    std::fs::write(&kept, "{}\n").unwrap();
    app.sessions[1].path = gone.clone();
    app.sessions[0].path = kept.clone();
    let (id, running) = (app.sessions[1].id.clone(), app.sessions[0].id.clone());
    let _ = app.set(|p| {
        p.names.insert(id.clone(), "消す会話".into());
        p.pinned.push(id.clone());
        p.groups.insert(id.clone(), "古い".into());
    });

    // Pointed at, its row offers it; the first press only asks.
    app.row_hover = Some(id.clone());
    let _ = app.update(Msg::Delete(id.clone()));
    assert!(gone.exists() && app.sessions.iter().any(|s| s.id == id));
    assert!(says(&app, crate::words::JA.delete_sure));
    let _ = app.update(Msg::Delete(id.clone()));
    assert!(!gone.exists());
    assert!(!app.sessions.iter().any(|s| s.id == id));
    assert!(!app.prefs.names.contains_key(&id) && !app.prefs.pinned.contains(&id));
    assert!(!app.prefs.groups.contains_key(&id));

    // Into the trash, and out of it again by the bar's button: its name, pin and group with it.
    let undo = crate::words::JA.undo;
    act(&mut app, |ui| {
        let _ = ui.click(undo).expect("the bar offers to take it back");
    });
    assert!(gone.exists());
    assert_eq!(app.prefs.names.get(&id).map(String::as_str), Some("消す会話"));
    assert!(app.prefs.pinned.contains(&id) && app.prefs.groups.contains_key(&id));
    app.sessions = window("ja").sessions;
    app.sessions[1].path = gone.clone();
    app.sessions[0].path = kept.clone();

    // The one that runs is kept, from `/delete` too.
    app.chat.session = Some(running.clone());
    let _written = app.pretend_running();
    let _ = app.command(Command::Delete);
    let _ = app.command(Command::Delete);
    assert!(kept.exists() && app.sessions.iter().any(|s| s.id == running));
    assert_eq!(app.toast.as_deref(), Some(crate::words::JA.delete_running));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_conversation_put_in_a_group_is_listed_under_it_and_the_group_folds() {
    let mut app = window("ja");
    let id = app.sessions[0].id.clone();
    let _ = app.update(Msg::Group(id.clone()));
    let _ = app.update(Msg::OverlayText("  タイムゾーン ".into()));
    let _ = app.update(Msg::GroupSave);
    assert_eq!(app.prefs.groups.get(&id).map(String::as_str), Some("タイムゾーン"));
    // Under its group, after those in none.
    assert_eq!(app.found().last().map(|s| s.id.clone()), Some(id.clone()));
    assert!(says(&app, "タイムゾーン"));
    // Folded, it is not listed, unless searched for.
    act(&mut app, |ui| {
        let _ = ui.click("タイムゾーン");
    });
    assert!(!app.found().any(|s| s.id == id));
    assert!(says(&app, "(1)"));
    app.filter = "指数".into();
    assert!(app.found().any(|s| s.id == id));
    app.filter.clear();
    // Another conversation joins it by picking it, and an empty name takes one out.
    let other = app.sessions[2].id.clone();
    let _ = app.update(Msg::Group(other.clone()));
    let _ = app.update(Msg::GroupPick("タイムゾーン".into()));
    assert_eq!(app.prefs.groups.get(&other).map(String::as_str), Some("タイムゾーン"));
    let _ = app.update(Msg::Group(id.clone()));
    let _ = app.update(Msg::OverlayText(String::new()));
    let _ = app.update(Msg::GroupSave);
    assert!(!app.prefs.groups.contains_key(&id) && app.overlay.is_none());
}

#[test]
fn a_file_found_in_the_files_tab_is_read_with_its_lines_and_one_is_commented_on() {
    let mut app = window("ja");
    let dir = std::env::temp_dir().join(format!("sasacode-gui-view-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/retry.ts"),
        "export function retry() {\n  return 2 ** attempt;\n}\n",
    )
    .unwrap();
    app.cwd = dir.clone();
    // ⌘6 opens the tab, which asks for the files once.
    let _ = app.update(shortcut(&Key::Character("6".into()), Modifiers::COMMAND).unwrap());
    assert_eq!(app.tab, Tab::Files);
    assert!(app.files_asked);
    let _ = app.update(Msg::Files(vec![
        "README.md".into(),
        "src/".into(),
        "src/retry.ts".into(),
    ]));
    let _ = app.update(Msg::FileQuery("retry".into()));
    assert_eq!(app.found_files().cloned().collect::<Vec<_>>(), ["src/retry.ts"]);
    act(&mut app, |ui| {
        let _ = ui.click("src/retry.ts");
    });
    let path = dir.join("src/retry.ts");
    let _ = app.update(Msg::FileRead(Box::new(crate::files::read(path.clone(), None))));
    assert!(says(&app, crate::words::JA.files_back), "back");
    // Its lines are drawn colored, which the simulator does not read: they are what was read.
    assert_eq!(
        app.viewing.as_ref().map(|v| v.lines[1].as_str()),
        Some("  return 2 ** attempt;")
    );
    // A click on its second line asks for a comment, which goes with the next message.
    let _ = app.update(Msg::CommentAt(path.clone(), 2, false));
    let _ = app.update(Msg::CommentText("指数の底を変えたい".into()));
    let _ = app.update(Msg::CommentSave);
    assert_eq!(app.chat.comments.len(), 1);
    assert!(says(&app, "指数の底を変えたい"));
    // A reply's link to a file of the project opens it there, at its line.
    let _ = app.update(Msg::CloseFile);
    app.tab = Tab::Changes;
    let _ = app.update(Msg::Link("src/retry.ts:3".into()));
    assert_eq!(app.tab, Tab::Files);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_folders_script_is_listed_in_the_settings_run_from_the_list_and_its_output_shown_to_the_agent() {
    let mut app = window("ja");
    // None yet: the key opens the settings, where one is added.
    let _ = app.update(shortcut(&Key::Character("r".into()), Modifiers::COMMAND | Modifiers::SHIFT).unwrap());
    assert_eq!(app.overlay, Some(crate::app::Overlay::Settings));
    // Its button is at the end of the settings, past where they scroll in this window.
    assert!(says(&app, crate::words::JA.script_add));
    let _ = app.update(Msg::ScriptAdd);
    let _ = app.update(Msg::ScriptName(0, "テスト".into()));
    let _ = app.update(Msg::ScriptCommand(0, "echo ok".into()));
    assert_eq!(app.scripts().len(), 1);
    // Now the key lists it, and Enter runs it.
    let _ = app.update(Msg::Show(None));
    let _ = app.update(Msg::Command(Command::Scripts));
    assert!(says(&app, "テスト"));
    let _ = app.update(Msg::PaletteRun(0));
    assert_eq!(app.script_runs.len(), 1);
    assert_eq!(app.tab, Tab::Commands);
    let id = app.script_runs[0].id;
    let _ = app.update(Msg::ScriptUpdate(
        id,
        crate::scripts::ScriptUpdate::Line("1 failed".into()),
    ));
    let _ = app.update(Msg::ScriptUpdate(id, crate::scripts::ScriptUpdate::Exited(Some(1))));
    assert!(says(&app, "終了コード 1"));
    act(&mut app, |ui| {
        let _ = ui.click(crate::words::JA.script_to_agent);
    });
    assert!(typed(&app).contains("echo ok") && typed(&app).contains("1 failed"));
    // Taken out of the list.
    let _ = app.update(Msg::ScriptRemove(0));
    assert!(app.scripts().is_empty() && app.prefs.scripts.is_empty());
}

#[test]
fn a_shortcut_is_given_a_new_key_in_the_list_of_keys_but_not_one_that_is_taken_or_reserved() {
    let mut app = window("ja");
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Keys)));
    // The palette's key: a key another has is refused, saying whose it is.
    let _ = app.update(Msg::KeyCapture(crate::keys::Act::Palette));
    assert!(says(&app, crate::words::JA.key_waiting));
    let _ = app.update(Msg::KeyCaptured(Key::Character("k".into()), Modifiers::COMMAND));
    assert!(
        app.key_note
            .as_deref()
            .is_some_and(|n| n.contains(crate::i18n::Strings::get("ja").find))
    );
    let _ = app.update(Msg::KeyCaptured(Key::Character("v".into()), Modifiers::COMMAND));
    assert!(app.key_capture.is_some());
    // A free one is taken: the palette opens by it, and no longer by its old key.
    let _ = app.update(Msg::KeyCaptured(
        Key::Character("y".into()),
        Modifiers::COMMAND | Modifiers::SHIFT,
    ));
    assert_eq!(app.key_capture, None);
    assert_eq!(app.prefs.keys.get("palette").map(String::as_str), Some("cmd+shift+y"));
    assert!(matches!(
        shortcut(&Key::Character("y".into()), Modifiers::COMMAND | Modifiers::SHIFT),
        Some(Msg::Show(Some(crate::app::Overlay::Palette { .. })))
    ));
    assert!(shortcut(&Key::Character("p".into()), Modifiers::COMMAND | Modifiers::SHIFT).is_none());
    assert!(says(&app, &crate::keys::Chord::parse("cmd+shift+y").unwrap().show()));
    // Back to its default.
    let _ = app.update(Msg::KeyReset(None));
    assert!(app.prefs.keys.is_empty());
    assert!(shortcut(&Key::Character("p".into()), Modifiers::COMMAND | Modifiers::SHIFT).is_some());
}

#[test]
fn an_image_named_in_the_input_is_shown_small_above_it_and_taken_out_by_its_cross() {
    let mut app = window("ja");
    let dir = std::env::temp_dir().join(format!("sasacode-gui-thumb-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let shot = dir.join("shot 1.png");
    std::fs::write(&shot, "not really a picture").unwrap();
    let _ = app.update(Msg::Dropped(shot.clone()));
    let written = crate::composer::path_ref(&shot, &app.cwd);
    assert_eq!(
        crate::composer::image_refs(&typed(&app), &app.cwd),
        vec![(shot.clone(), written.clone())]
    );
    // Its name under the pointer; the cross takes it out of the input, and the rest stays.
    let _ = app.update(Msg::Edit(text_editor::Action::Edit(text_editor::Edit::Paste(
        std::sync::Arc::new("これを見て".into()),
    ))));
    let _ = app.update(Msg::Unattach(written));
    assert_eq!(typed(&app).trim(), "これを見て");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_outline_lists_each_message_and_the_headings_of_its_replies_and_takes_one_to_go_to() {
    let mut app = window("ja");
    markdown_reply(&mut app);
    let _ = app.update(shortcut(&Key::Character("o".into()), Modifiers::COMMAND | Modifiers::SHIFT).unwrap());
    assert!(says(&app, "1. 何を変えた？") && says(&app, "    変更点"));
    // Typed, it finds the heading; Enter goes there, held by its start.
    let _ = app.update(Msg::OverlayText("変更".into()));
    assert_eq!(app.found_entries().len(), 1);
    let _ = app.update(Msg::PaletteRun(0));
    assert!(app.overlay.is_none() && app.reading);
}

#[test]
fn a_conversation_opened_beside_goes_on_there_and_is_taken_into_the_main_place_to_answer() {
    let mut app = window("ja");
    let dir = std::env::temp_dir().join(format!("sasacode-gui-split-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = |name: &str, said: &str, answer: &str| {
        let path = dir.join(name);
        let lines = [
            serde_json::json!({"type":"session","version":3,"id":name,"cwd":"/w","createdAt":"2026-01-01T00:00:00.000Z"}),
            serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":said}],"timestamp":1}}),
            serde_json::json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":answer}],"provider":"p","model":"m","usage":{"input":1,"output":1},"stopReason":"stop","timestamp":0}}),
        ];
        std::fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        path
    };
    app.sessions[1].path = log("one.jsonl", "横で見る会話", "横の返事");
    app.sessions[2].path = log("two.jsonl", "二つ目の横", "二つ目の返事");
    app.sessions[0].path = log("zero.jsonl", "メインの会話", "メインの返事");
    app.sessions[3].path = log("three.jsonl", "四つ目", "四つ目の返事");
    let (main, one, two) = (
        app.sessions[0].id.clone(),
        app.sessions[1].id.clone(),
        app.sessions[2].id.clone(),
    );
    let _ = app.update(Msg::OpenSession(app.sessions[0].path.clone()));
    assert_eq!(app.chat.session.as_deref(), Some(main.as_str()));

    // From the palette's list: it is drawn beside, the one shown stays.
    let _ = app.command(Command::Split);
    assert!(app.overlay.is_some());
    assert!(
        app.found_entries()
            .iter()
            .all(|e| !matches!(&e.msg, Msg::Split(p) if p == &app.sessions[0].path))
    );
    let _ = app.update(Msg::OverlayText("ページング".into()));
    assert_eq!(app.found_entries().len(), 1);
    let _ = app.update(Msg::PaletteRun(0));
    assert!(app.overlay.is_none());
    assert_eq!(app.split.as_deref(), Some(one.as_str()));
    assert!(says(&app, "横で見る会話") && says(&app, "メインの会話"));
    assert!(says(&app, crate::words::JA.split_swap));

    // ⌘ held, a click in the list puts another there instead of opening it.
    let _ = app.update(Msg::Modifiers(Modifiers::COMMAND));
    act(&mut app, |ui| {
        let _ = ui.click("1日前 · 12件のメッセージ");
    });
    // A shortcut held with ⌘ that opens a conversation opens it in the main place.
    let _ = app.update(Msg::OpenSession(app.sessions[3].path.clone()));
    let _ = app.update(Msg::Modifiers(Modifiers::default()));
    assert_eq!(app.chat.session.as_deref(), Some(app.sessions[3].id.as_str()));
    let _ = app.update(Msg::OpenSession(app.sessions[0].path.clone()));
    assert_eq!(app.chat.session.as_deref(), Some(main.as_str()));
    assert_eq!(app.split.as_deref(), Some(two.as_str()));
    assert!(says(&app, "二つ目の横") && !says(&app, "横で見る会話"));

    // Taken into the main place, the one that was there goes beside.
    act(&mut app, |ui| {
        let _ = ui.click(crate::words::JA.split_swap);
    });
    assert_eq!(app.chat.session.as_deref(), Some(two.as_str()));
    assert_eq!(app.split.as_deref(), Some(main.as_str()));
    assert!(says(&app, "メインの会話") && says(&app, "二つ目の横"));

    // Opened in the main place, the one beside is no longer beside; closed, it is let go.
    let _ = app.update(Msg::SplitSwap);
    assert_eq!(app.chat.session.as_deref(), Some(main.as_str()));
    let _ = app.update(Msg::SplitClose);
    assert!(app.split.is_none() && app.split_chat().is_none());
    assert!(!app.parked.iter().any(|c| c.session.as_deref() == Some(two.as_str())));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_model_writes_the_commit_message_the_pull_request_and_the_next_message_when_asked() {
    use crate::helper::Draft;
    let mut app = window("ja");
    // The commit message, into its field.
    let _ = app.update(Msg::Draft(Draft::Commit));
    assert!(app.drafting.contains(&Draft::Commit));
    let _ = app.update(Msg::Drafted(Draft::Commit, Ok("fix: 再試行を直す".into())));
    assert_eq!(app.commit_msg, "fix: 再試行を直す");
    assert!(app.drafting.is_empty());
    let _ = app.update(Msg::Drafted(Draft::Commit, Err("no key".into())));
    assert!(app.toast.as_deref().is_some_and(|t| t.contains("no key")));

    // The pull request: filled from the branch's commit, then written by the model, then made.
    app.git.branch = Some("topic".into());
    let context = crate::git::PrContext {
        base: "origin/main".into(),
        commits: vec![("fix: 再試行を直す".into(), "理由".into())],
        ..Default::default()
    };
    let _ = app.update(Msg::PrContext(Ok(Box::new(context))));
    assert!(matches!(app.overlay, Some(crate::app::Overlay::Pr)));
    assert!(says(&app, "main へ · 1件のコミット"));
    assert_eq!(
        app.pr_draft.as_ref().map(|p| p.title.as_str()),
        Some("fix: 再試行を直す")
    );
    act(&mut app, |ui| {
        let _ = ui.click(crate::words::JA.pr_draft);
    });
    assert!(app.drafting.contains(&Draft::Pr));
    let _ = app.update(Msg::Drafted(
        Draft::Pr,
        Ok("# 再試行を指数バックオフに\n\n## 変更\n- 待ち時間".into()),
    ));
    let pr = app.pr_draft.as_ref().unwrap();
    assert_eq!(pr.title, "再試行を指数バックオフに");
    assert_eq!(pr.body.text().trim(), "## 変更\n- 待ち時間");
    let _ = app.update(Msg::PrCreate);
    assert!(app.overlay.is_none() && app.pr_draft.is_none());

    // The next message, guessed only when the settings say so, and taken by a click.
    app.chat.transcript.send("直して", std::time::Instant::now());
    let key = app.chat.key;
    assert!(!app.prefs.suggest);
    let _ = app.guess_next(key);
    assert!(app.drafting.is_empty());
    let _ = app.update(Msg::SetSuggest(true));
    let _ = app.guess_next(key);
    assert_eq!(app.drafting, vec![Draft::Next(key, 1)]);
    let _ = app.update(Msg::Drafted(Draft::Next(key, 1), Ok("テストも足して".into())));
    assert!(says(&app, "テストも足して"));
    act(&mut app, |ui| {
        let _ = ui.click("テストも足して");
    });
    assert_eq!(typed(&app).trim(), "テストも足して");
    assert!(app.suggestion.is_none());
    // A guess for a conversation that moved on is not shown.
    let _ = app.update(Msg::Drafted(Draft::Next(key, 0), Ok("古い".into())));
    assert!(app.suggestion.is_none());
}

#[test]
fn the_window_asks_before_it_closes_on_a_run_and_before_auto_and_keeps_what_was_marked() {
    let mut app = window("ja");
    let id = iced::window::Id::unique();
    // Nothing runs: it closes at once. A run going on: it asks.
    let _ = app.update(Msg::CloseAsked(id));
    assert!(app.overlay.is_none());
    app.chat.session = Some(app.sessions[0].id.clone());
    app.chat.transcript.send("長い作業", std::time::Instant::now());
    let _written = app.pretend_running();
    let _ = app.update(Msg::CloseAsked(id));
    assert!(matches!(app.overlay, Some(crate::app::Overlay::Quit(_))));
    assert!(says(&app, "1件の実行が動いています。窓を閉じると止まります。"));
    assert!(app.title().starts_with("● 長い作業 — "));
    let _ = app.update(Msg::Show(None));

    // `auto` is agreed to once.
    let _ = app.update(Msg::Permission("auto"));
    assert!(matches!(app.overlay, Some(crate::app::Overlay::ConfirmAuto)) && app.permission.is_none());
    act(&mut app, |ui| {
        let _ = ui.click(crate::words::JA.auto_yes);
    });
    assert_eq!(app.permission, Some("auto"));
    let _ = app.update(Msg::Permission("edits"));
    let _ = app.update(Msg::Permission("auto"));
    assert!(app.overlay.is_none() && app.permission == Some("auto"));

    // Marked unread, a conversation not open shows it in the list; opened, it is read.
    let other = app.sessions[2].id.clone();
    let _ = app.update(Msg::MarkUnread(other.clone()));
    assert!(app.prefs.more.unread.contains(&other));
    let _ = app.update(Msg::MarkUnread(other.clone()));
    assert!(!app.prefs.more.unread.contains(&other));

    // Written to a file by `/export path`.
    let dir = std::env::temp_dir().join(format!("sasacode-gui-export-{}", std::process::id()));
    app.cwd = dir.clone();
    let _ = app.update(Msg::ExportTo("out/会話.md".into()));
    let written = std::fs::read_to_string(dir.join("out/会話.md")).unwrap();
    assert!(written.contains("長い作業"));
    let _ = std::fs::remove_dir_all(&dir);

    // A long message is folded until opened.
    let long = (1..=30).map(|n| format!("{n}行目")).collect::<Vec<_>>().join("\n");
    app.chat.transcript.send(&long, std::time::Instant::now());
    assert!(says(&app, crate::words::JA.show_whole) && !says(&app, "30行目"));
    let _ = app.update(Msg::Whole(1));
    assert!(says(&app, crate::words::JA.fold_whole));

    // The language and the width are kept with the other choices.
    let _ = app.update(Msg::SetLang(Some("en")));
    assert!(!app.s.ja && app.prefs.more.lang.as_deref() == Some("en"));
    let _ = app.update(Msg::SetWidth(crate::layout::Width::Full));
    assert_eq!(app.prefs.more.chat_width, crate::layout::Width::Full);
}

#[test]
fn a_run_cut_short_by_closing_the_window_is_offered_to_go_on_when_shown_again() {
    let w = crate::words::JA;
    let mut app = window("ja");
    let id = app.sessions[0].id.clone();
    app.chat.session = Some(id.clone());
    app.chat.transcript.send("長い作業", std::time::Instant::now());
    let _written = app.pretend_running();
    let closing = iced::window::Id::unique();
    let _ = app.update(Msg::CloseAsked(closing));
    let _ = app.update(Msg::Quit(closing));
    assert!(app.prefs.more.cut.contains(&id));
    // Not offered while it still runs.
    assert!(!says(&app, w.cut_short));

    // Shown again in the next window: offered, and going on asks for the rest of it.
    let mut again = window("ja");
    again.prefs.more.cut = app.prefs.more.cut.clone();
    again.chat.session = Some(id.clone());
    assert!(says(&again, w.cut_short));
    act(&mut again, |ui| {
        let _ = ui.click(w.cut_go_on);
    });
    assert!(again.chat.busy() && again.prefs.more.cut.is_empty());
    let asked = again.chat.transcript.exchanges.last().map(|e| e.prompt.clone());
    assert_eq!(asked.as_deref(), Some(w.cut_prompt));
    assert!(!says(&again, w.cut_short));

    // Put away, or another run started there: not offered any more.
    let mut away = window("ja");
    away.prefs.more.cut.insert(id.clone());
    away.chat.session = Some(id.clone());
    let _ = away.update(Msg::CutSeen(id.clone()));
    assert!(away.prefs.more.cut.is_empty() && !says(&away, w.cut_short));
    away.prefs.more.cut.insert(id.clone());
    let _ = away.start_run(away.chat.key, "別のこと".into(), false);
    assert!(away.prefs.more.cut.is_empty());
}

#[test]
fn the_git_tab_shows_the_branch_or_a_commit_side_by_side_with_files_marked_viewed() {
    use crate::diff::Diff;
    use crate::scope::Scope;
    let mut app = window("ja");
    app.tab = Tab::Git;
    app.insp = true;
    app.git = crate::git::Status {
        repo: true,
        branch: Some("topic".into()),
        default_branch: Some("main".into()),
        ..Default::default()
    };
    let _ = app.update(Msg::GitScope(Scope::Branch));
    assert!(app.scope_loading);
    let scoped = crate::git::Scoped {
        base: "origin/main".into(),
        files: vec![
            (
                "src/retry.ts".into(),
                Diff::parse("@@ -1 +1 @@\n-const WAIT = 100\n+const WAIT = 200\n"),
            ),
            ("src/util/time.ts".into(), Diff::parse("@@ -0,0 +1 @@\n+export {}\n")),
        ],
    };
    let _ = app.update(Msg::Scoped(
        Scope::Branch,
        vec![("abc1234".into(), "待ち時間".into())],
        Ok(Box::new(scoped.clone())),
    ));
    assert!(says(&app, "origin/mainとの比較") && says(&app, "src/") && says(&app, "src/util/"));
    assert!(says(&app, "0/2件見た") && says(&app, "const WAIT = 200"));
    // Side by side, the removed line is drawn beside the added one.
    let _ = app.update(Msg::SetSplitDiff(true));
    assert!(says(&app, "const WAIT = 100") && says(&app, "const WAIT = 200"));
    // Kept whole, the lines scroll sideways instead of wrapping, side by side or not.
    let _ = app.update(Msg::SetDiffNowrap(true));
    assert!(app.prefs.more.diff_nowrap && says(&app, "折り返さない"));
    assert!(says(&app, "const WAIT = 100") && says(&app, "const WAIT = 200"));
    let _ = app.update(Msg::SetSplitDiff(false));
    assert!(says(&app, "const WAIT = 200"));
    let _ = app.update(Msg::SetSplitDiff(true));
    // Viewed, a file folds away; changed, it is shown again.
    let key = crate::scope::viewed_key("src/retry.ts", &scoped.files[0].1);
    let _ = app.update(Msg::Viewed(key));
    assert!(says(&app, "1/2件見た") && !says(&app, "const WAIT = 200"));
    // An answer for a scope left meanwhile is not shown.
    let _ = app.update(Msg::GitScope(Scope::Changes));
    let _ = app.update(Msg::Scoped(Scope::Branch, Vec::new(), Ok(Box::new(scoped))));
    assert!(app.scoped.is_none());

    // A commit on the branch others start from asks once more.
    app.git.branch = Some("main".into());
    app.git.files = vec![crate::git::File {
        path: "a.txt".into(),
        index: ' ',
        tree: 'M',
        staged_diff: Diff::default(),
        unstaged_diff: Diff::parse("@@ -1 +1 @@\n-a\n+b\n"),
    }];
    let _ = app.update(Msg::GitDo(crate::git::Op::Commit("x".into())));
    assert!(app.confirm_default && !app.git_busy);
    assert!(app.toast.as_deref().is_some_and(|t| t.contains("mainに直接")));
    // The working tree's hunks are kept whole too.
    assert!(app.prefs.more.diff_nowrap && says(&app, "a.txt") && says(&app, "b"));
}

#[test]
fn the_git_tab_shows_what_the_shown_conversations_last_turn_changed() {
    use crate::diff::Diff;
    use crate::scope::Scope;
    let mut app = window("ja");
    let w = crate::words::JA;
    app.tab = Tab::Git;
    app.insp = true;
    app.git = crate::git::Status {
        repo: true,
        branch: Some("topic".into()),
        ..Default::default()
    };
    // The working tree as the turn began and as it ended.
    let key = app.chat.key;
    let _ = app.update(Msg::TurnTree(key, true, Some("a1".into())));
    let _ = app.update(Msg::TurnTree(key, false, Some("b2".into())));
    assert_eq!(
        (app.chat.turn_from.as_deref(), app.chat.turn_to.as_deref()),
        (Some("a1"), Some("b2"))
    );
    assert!(says(&app, w.scope_turn));
    let _ = app.update(Msg::GitScope(Scope::Turn));
    assert!(app.scope_loading);
    let scoped = crate::git::Scoped {
        base: String::new(),
        files: vec![(
            "src/retry.ts".into(),
            Diff::parse("@@ -1 +1 @@\n-const WAIT = 100\n+const WAIT = 200\n"),
        )],
    };
    let _ = app.update(Msg::Scoped(Scope::Turn, Vec::new(), Ok(Box::new(scoped))));
    assert!(says(&app, w.scope_turn_base) && says(&app, "src/") && says(&app, "const WAIT = 200"));
    // Sent again: the next turn's trees are taken anew.
    app.set_input("もう一度");
    let _ = app.update(Msg::Send);
    assert!(app.chat.turn_from.is_none() && app.chat.turn_to.is_none());
    // Another conversation shown: its last turn is not this one's, so back to the changes.
    let _ = app.update(Msg::NewSession);
    assert_eq!(app.git_scope, Scope::Changes);
    assert!(app.scoped.is_none());
}

#[test]
fn unwrapped_diff_lines_are_as_wide_as_the_longest_at_least_the_panel() {
    use crate::view::unwrapped_width;
    assert_eq!(unwrapped_width(["a"].into_iter(), 300.0), 300.0);
    let long = "x".repeat(100);
    let wide = unwrapped_width(["a", long.as_str()].into_iter(), 300.0);
    assert!(wide > 700.0, "{wide}");
    // A wide character takes two cells.
    let ja = "あ".repeat(50);
    assert_eq!(unwrapped_width([ja.as_str()].into_iter(), 0.0), wide);
}

#[test]
fn a_right_click_on_a_row_opens_its_menu_whose_items_act_and_esc_puts_it_away() {
    use crate::rows::MenuFor;
    let mut app = window("ja");
    let id = app.sessions[2].id.clone();
    // A right click on its row.
    act(&mut app, |ui| {
        use iced::mouse::{Button, Event as Mouse};
        let at = ui
            .find("1日前 · 12件のメッセージ")
            .expect("the row is listed")
            .bounds()
            .center();
        ui.point_at(at);
        let _ = ui.simulate([
            iced::Event::Mouse(Mouse::ButtonPressed(Button::Right)),
            iced::Event::Mouse(Mouse::ButtonReleased(Button::Right)),
        ]);
    });
    assert!(matches!(&app.menu, Some(m) if m.what == MenuFor::Row(id.clone())));
    assert!(app.menu.is_some());
    let pin = app.s.pin;
    act(&mut app, |ui| {
        let _ = ui.click(pin).expect("the menu offers to pin it");
    });
    assert!(app.prefs.pinned.contains(&id) && app.menu.is_none());

    // Esc puts it away, and does not stop the run of the one shown.
    let _ = app.update(Msg::OpenMenu(MenuFor::Row(id.clone())));
    let _written = app.pretend_running();
    let _ = app.update(Msg::Stop);
    assert!(app.menu.is_none() && app.running());

    // A file's menu puts it in the input.
    let _ = app.update(Msg::OpenMenu(MenuFor::File(app.cwd.join("src/lib.rs"))));
    let mention = crate::words::JA.menu_mention;
    act(&mut app, |ui| {
        let _ = ui.click(mention).expect("the file's menu offers to mention it");
    });
    assert!(typed(&app).contains("@src/lib.rs"), "{}", typed(&app));
}

#[test]
fn conversations_chosen_together_are_archived_at_once_and_taken_back_by_cmd_z() {
    let mut app = window("ja");
    let (a, b) = (app.sessions[1].id.clone(), app.sessions[3].id.clone());
    // ⇧-click on a row chooses it.
    app.modifiers = Modifiers::SHIFT;
    act(&mut app, |ui| {
        let _ = ui.click("3時間前 · 41件のメッセージ").expect("the row is listed");
    });
    app.modifiers = Modifiers::empty();
    assert!(app.selected.contains(&a));
    let _ = app.update(Msg::Select(b.clone()));
    assert!(says(&app, "2件を選択中"));

    let _ = app.update(Msg::Bulk(crate::rows::Bulk::Archive));
    assert!(app.prefs.archived.contains(&a) && app.prefs.archived.contains(&b));
    assert!(app.selected.is_empty() && !app.found().any(|s| s.id == a || s.id == b));
    assert!(says(&app, "2件をアーカイブしました"));

    // ⌘Z is the window's while no field has the keyboard, and takes it back.
    assert!(matches!(
        shortcut(&Key::Character("z".into()), Modifiers::COMMAND),
        Some(Msg::UndoKey)
    ));
    let _ = app.update(Msg::UndoKeyHeld(true));
    assert!(app.prefs.archived.contains(&a));
    let _ = app.update(Msg::UndoKeyHeld(false));
    assert!(!app.prefs.archived.contains(&a) && !app.prefs.archived.contains(&b));
    let _ = app.update(Msg::Undo);
    assert_eq!(app.toast.as_deref(), Some(crate::words::JA.undo_none));

    // Deleted together, on the second press only.
    let _ = app.update(Msg::Select(a.clone()));
    let _ = app.update(Msg::Bulk(crate::rows::Bulk::Delete));
    assert!(app.sessions.iter().any(|s| s.id == a));
    assert!(says(&app, "もう一度押すと1件の会話を削除します（7日間は元に戻せます）"));
}

#[test]
fn the_list_shows_the_unread_alone_or_the_newest_started_first_and_finds_a_branch() {
    let mut app = window("ja");
    let unread = app.sessions[2].id.clone();
    let _ = app.update(Msg::MarkUnread(unread.clone()));
    let _ = app.update(Msg::ListShow(crate::layout::Show::Unread));
    let shown: Vec<String> = app.found().map(|s| s.id.clone()).collect();
    assert_eq!(shown, vec![unread.clone()]);
    // Kept open while it leaves some out.
    assert!(says(&app, crate::words::JA.show_unread));
    let _ = app.update(Msg::ListShow(crate::layout::Show::All));

    for (i, s) in app.sessions.iter_mut().enumerate() {
        s.created_at = format!("2026-11-0{}T00:00:00Z", i + 1);
    }
    let _ = app.update(Msg::ListOrder(crate::layout::Order::Created));
    let first = app.found().next().map(|s| s.id.clone());
    assert_eq!(first, Some(app.sessions[3].id.clone()));

    let id = app.sessions[2].id.clone();
    let _ = app.set(|p| {
        p.branches.insert(id.clone(), "fix/timezone".into());
    });
    app.prs.insert("fix/timezone".into(), (42, "OPEN".into()));
    app.filter = "fix/time".into();
    assert_eq!(app.found().map(|s| s.id.clone()).collect::<Vec<_>>(), vec![id.clone()]);
    app.filter = "#42".into();
    assert_eq!(app.found().map(|s| s.id.clone()).collect::<Vec<_>>(), vec![id.clone()]);
    app.filter.clear();
    let _ = app.update(Msg::ListOrder(crate::layout::Order::Updated));
    assert!(says(&app, "1日前 · 12件のメッセージ · #42"));
}

#[test]
fn conversations_unused_for_long_are_archived_but_not_a_pinned_one_or_one_taken_back() {
    let mut app = window("ja");
    let (day, three) = (app.sessions[2].id.clone(), app.sessions[3].id.clone());
    let _ = app.update(Msg::Pin(day.clone()));
    let _ = app.update(Msg::SetArchiveAfter(Some(1)));
    // Older than a day: the one 26 hours ago is pinned, the one 3 days ago is archived.
    assert!(app.prefs.archived.contains(&three) && !app.prefs.archived.contains(&day));
    assert!(says(&app, "しばらく使っていない会話を1件アーカイブしました"));
    // Taken back by hand, it stays.
    let _ = app.update(Msg::Archive(three.clone()));
    assert!(!app.prefs.archived.contains(&three) && app.prefs.more.kept.contains(&three));
    app.archive_idle();
    assert!(!app.prefs.archived.contains(&three));
}

#[test]
fn the_search_in_the_conversation_steps_through_what_it_finds_and_counts_them() {
    let mut app = window("ja");
    crate::snapshots::long_log(&mut app);
    let first = app.chat.transcript.exchanges[0]
        .prompt
        .chars()
        .take(6)
        .collect::<String>();
    let _ = app.update(Msg::Command(Command::Find));
    let _ = app.update(Msg::FindInText(first));
    let n = app.found_in().len();
    assert!(n >= 2, "found {n}");
    assert!(says(&app, &format!("1/{n}件")));
    assert_eq!(app.find_current(), Some(app.found_in()[0]));

    // Enter and ⌘G go on, ⌘⇧G back, round from the last to the first.
    assert!(matches!(
        shortcut(&Key::Character("g".into()), Modifiers::COMMAND),
        Some(Msg::FindStep(1))
    ));
    assert!(matches!(
        shortcut(&Key::Character("G".into()), Modifiers::COMMAND | Modifiers::SHIFT),
        Some(Msg::FindStep(-1))
    ));
    let _ = app.update(Msg::FindStep(1));
    assert_eq!(app.find_at, 1);
    assert!(says(&app, &format!("2/{n}件")));
    let _ = app.update(Msg::FindStep(-1));
    let _ = app.update(Msg::FindStep(-1));
    assert_eq!(app.find_at, n - 1);
    // Typing again starts from the first.
    let _ = app.update(Msg::FindInText("README".into()));
    assert_eq!(app.find_at, 0);
}

#[test]
fn calls_that_went_well_and_what_was_thought_are_left_out_when_the_settings_say_so() {
    let mut app = window("ja");
    play(&mut app, "work.jsonl", "再試行を指数バックオフにして", |_| false);
    let s = app.s;
    act(&mut app, |ui| {
        ui.click(s.show_steps)
            .expect("a finished exchange has its steps folded");
    });
    let verbs = [
        s.v_read,
        s.v_edit,
        s.v_write,
        s.v_bash,
        s.v_overwrite,
        s.v_wrote,
        s.v_task,
    ];
    let shown = |app: &App| verbs.iter().filter(|verb| says(app, verb)).count();
    let before = shown(&app);
    assert!(before > 0);
    let _ = app.update(Msg::SetFailedOnly(true));
    assert_eq!(shown(&app), 0);
    let _ = app.update(Msg::SetFailedOnly(false));
    assert_eq!(shown(&app), before);
}

#[test]
fn a_github_alert_is_drawn_under_its_name_and_a_reply_can_be_branched_from() {
    let mut app = window("ja");
    let mut now = std::time::Instant::now();
    app.chat.session = Some("0a1b2c3d-0".into());
    app.chat.transcript.send("気をつけることは？", now);
    for line in [
        r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"> [!WARNING]\n> 本番のデータが消えます\n\n- [x] 済み\n- [ ] まだ"}],"provider":"fake","model":"m","usage":{"input":1,"output":1},"stopReason":"stop"}}"#,
        r#"{"type":"agent_end","cause":"done"}"#,
    ] {
        now += std::time::Duration::from_secs(1);
        app.chat
            .transcript
            .apply(&sasacode_protocol::parse_event(line).unwrap(), now);
    }
    assert!(says(&app, crate::words::JA.alert_warning));
    let fork = crate::words::JA.fork_after;
    act(&mut app, |ui| {
        let _ = ui.click(fork).expect("a finished reply can be branched from");
    });

    // A file named with its line that is not there says so.
    let _ = app.update(Msg::OpenRef("src/nope.rs:3".into()));
    assert!(app.toast.as_deref().is_some_and(|t| t.contains("src/nope.rs:3")));
}

#[test]
fn part_of_a_reply_is_chosen_and_quoted_and_the_text_cannot_be_changed_there() {
    let mut app = window("ja");
    crate::snapshots::markdown_reply(&mut app);
    let quote = crate::words::JA.quote;
    act(&mut app, |ui| {
        let _ = ui.click(quote).expect("a reply offers to quote part of it");
    });
    assert_eq!(app.overlay, Some(crate::app::Overlay::Quote));
    let _ = app.update(Msg::QuoteEdit(text_editor::Action::Edit(text_editor::Edit::Insert(
        'x',
    ))));
    assert!(app.quoting.as_ref().unwrap().1.text().starts_with("## 変更点"));
    // Nothing chosen yet: nothing is quoted.
    let _ = app.update(Msg::QuoteTake);
    assert!(app.overlay.is_some());
    let _ = app.update(Msg::QuoteEdit(text_editor::Action::SelectLine));
    let _ = app.update(Msg::QuoteTake);
    assert!(app.overlay.is_none());
    let quoted = typed(&app);
    assert!(
        quoted.starts_with("> ## 変更点") && !quoted.contains("待ち時間"),
        "{quoted}"
    );
}

#[test]
fn the_conversation_says_when_its_branch_is_no_longer_checked_out() {
    let mut app = window("ja");
    let id = "0a1b2c3d-0".to_owned();
    app.chat.session = Some(id.clone());
    let _ = app.set(|p| {
        p.branches.insert(id.clone(), "fix/retry".into());
    });
    app.git.branch = Some("main".into());
    assert!(says(&app, "この会話はfix/retryで始めましたが、今はmainです"));
    assert!(says(&app, "fix/retryに戻る"));
    let _ = app.update(Msg::BranchSeen(id));
    assert!(!says(&app, "fix/retryに戻る"));
}

/// One line of the run, as the CLI would write it.
fn told(app: &mut App, line: serde_json::Value) {
    let event = sasacode_protocol::parse_event(&line.to_string()).expect("a line the CLI writes");
    let key = app.chat.key;
    let _ = app.update(Msg::Run(key, crate::runner::RunUpdate::Event(Box::new(event))));
}

#[test]
fn a_plugins_question_is_answered_in_the_dock_and_the_run_hears_each_answer() {
    let (mut app, mut written, _) = waiting();
    let _ = app.update(Msg::Answer(Decision::Allow));
    let _ = written.try_recv();
    let ui_response = |id: &str, value: serde_json::Value| ControlMessage::UiResponse { id: id.into(), value };

    // One of its options.
    told(
        &mut app,
        serde_json::json!({"type": "ui_request", "id": "ui-1", "kind": "select", "title": "どの色にしますか",
            "options": [{"value": "red", "label": "赤"}, {"value": "blue", "label": "青", "description": "落ち着いた色"}]}),
    );
    assert!(app.chat.transcript.waits());
    assert!(says(&app, "どの色にしますか") && says(&app, "落ち着いた色"));
    act(&mut app, |ui| {
        ui.click("青").expect("the option is there");
    });
    assert_eq!(written.try_recv().ok(), Some(ui_response("ui-1", "blue".into())));
    assert!(!app.chat.transcript.waits());

    // Yes or no.
    told(
        &mut app,
        serde_json::json!({"type": "ui_request", "id": "ui-2", "kind": "confirm", "title": "続けますか", "message": "3件を消します"}),
    );
    assert!(says(&app, "3件を消します"));
    act(&mut app, |ui| {
        ui.click("いいえ").expect("the answer is there");
    });
    assert_eq!(written.try_recv().ok(), Some(ui_response("ui-2", false.into())));

    // A text, typed after what it offered first.
    told(
        &mut app,
        serde_json::json!({"type": "ui_request", "id": "ui-3", "kind": "input", "title": "名前", "initial": "sasa"}),
    );
    act(&mut app, |ui| {
        ui.click(iced::widget::Id::from(crate::view::dock::ASK))
            .expect("the field is there");
        ui.typewrite("code");
        ui.tap_key(Named::Enter);
    });
    assert_eq!(written.try_recv().ok(), Some(ui_response("ui-3", "sasacode".into())));

    // Several, in the order offered; what it ticked first can be unticked.
    told(
        &mut app,
        serde_json::json!({"type": "ui_request", "id": "ui-4", "kind": "select_many", "title": "どれを入れますか",
            "options": [{"value": "a", "label": "甲"}, {"value": "b", "label": "乙"}, {"value": "c", "label": "丙"}],
            "selected": ["a"]}),
    );
    act(&mut app, |ui| {
        ui.click("丙").expect("the option is there");
    });
    act(&mut app, |ui| {
        ui.click("甲").expect("the option is there");
    });
    act(&mut app, |ui| {
        ui.click("乙").expect("the option is there");
    });
    act(&mut app, |ui| {
        ui.click("決定").expect("the button is there");
    });
    assert_eq!(
        written.try_recv().ok(),
        Some(ui_response("ui-4", serde_json::json!(["b", "c"])))
    );

    // Dismissed, and one the run took back itself.
    for id in ["ui-5", "ui-6"] {
        told(
            &mut app,
            serde_json::json!({"type": "ui_request", "id": id, "kind": "confirm", "title": "?"}),
        );
    }
    let _ = app.update(Msg::AskAnswer("ui-5".into(), serde_json::Value::Null));
    assert_eq!(
        written.try_recv().ok(),
        Some(ui_response("ui-5", serde_json::Value::Null))
    );
    told(&mut app, serde_json::json!({"type": "ui_cancelled", "id": "ui-6"}));
    assert!(!app.chat.transcript.waits());
    assert!(written.try_recv().is_err());
}

#[test]
fn what_the_plugins_say_shows_and_a_judged_call_says_so() {
    let (mut app, _written, _) = waiting();
    told(
        &mut app,
        serde_json::json!({"type": "plugin_status", "key": "todo:left", "text": "残り2件"}),
    );
    assert!(says(&app, "残り2件"));
    told(
        &mut app,
        serde_json::json!({"type": "plugin_status", "key": "todo:left", "text": null}),
    );
    assert!(!says(&app, "残り2件"));
    told(
        &mut app,
        serde_json::json!({"type": "plugin_notify", "level": "info", "text": "保存しました"}),
    );
    assert_eq!(app.toast.as_deref(), Some("保存しました"));
    told(
        &mut app,
        serde_json::json!({"type": "plugin_notify", "level": "warning", "text": "設定が古い"}),
    );
    told(
        &mut app,
        serde_json::json!({"type": "plugin_text", "title": "使用量", "text": "合計 12k", "format": "markdown"}),
    );
    let notes: Vec<&Note> = app
        .chat
        .transcript
        .exchanges
        .last()
        .unwrap()
        .steps
        .iter()
        .filter_map(|s| match s {
            Step::Note(n) => Some(n),
            _ => None,
        })
        .collect();
    assert!(notes.contains(&&Note::Notice("設定が古い".into())));
    assert!(
        notes
            .iter()
            .any(|n| matches!(n, Note::Text { title, .. } if title == "使用量"))
    );
    told(
        &mut app,
        serde_json::json!({"type": "commands", "commands": [{"name": "hello", "description": "say hello", "owner": "asker"}]}),
    );
    assert_eq!(app.plugin_commands[0].name, "hello");
    let call = app.chat.transcript.pending_approval().unwrap().call_id.clone();
    told(
        &mut app,
        serde_json::json!({"type": "judged", "callId": call, "tool": "bash", "verdict": "ask", "reason": "消す操作"}),
    );
    assert!(says(&app, app.s.w().judged_ask));
}

#[test]
fn a_call_can_be_allowed_with_its_arguments_edited_and_stopped_alone() {
    let (mut app, mut written, id) = waiting();
    // Only for a CLI that reads them: an older one would run the call as the model asked it.
    assert!(!says(&app, crate::words::JA.args_edit));
    told(&mut app, serde_json::json!({"type": "commands", "commands": []}));
    act(&mut app, |ui| {
        ui.click(crate::words::JA.args_edit).expect("the button is there");
    });
    assert_eq!(app.overlay, Some(crate::app::Overlay::Args));
    // What is not an object is not sent.
    app.editing_args.as_mut().unwrap().1 = text_editor::Content::with_text("[1, 2");
    let _ = app.update(Msg::ArgsSend);
    assert!(written.try_recv().is_err() && app.toast.is_some());
    app.editing_args.as_mut().unwrap().1 = text_editor::Content::with_text("{\"command\": \"echo safer\"}");
    let _ = app.update(Msg::ArgsSend);
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            id: id.clone(),
            decision: Decision::Allow,
            feedback: None,
            args: Some(serde_json::json!({"command": "echo safer"}))
        })
    );
    assert_eq!(answer_of(&app, &id), Some(Answer::Allowed));
    assert!(app.overlay.is_none());

    // Sent back as it was asked, it is a plain allow.
    let (mut app, mut written, id) = waiting();
    app.chat.newer = true;
    let _ = app.update(Msg::ArgsOpen);
    let _ = app.update(Msg::ArgsSend);
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            id,
            decision: Decision::Allow,
            feedback: None,
            args: None
        })
    );
    let _ = app.update(Msg::StopCall("call-9".into()));
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Abort {
            call_id: Some("call-9".into())
        })
    );
}

#[test]
fn what_always_allowed_is_kept_for_the_conversations_later_runs_and_can_be_taken_back() {
    let (mut app, mut written, _) = waiting();
    app.chat.session = Some("s-always".into());
    for step in app
        .chat
        .transcript
        .exchanges
        .iter_mut()
        .flat_map(|e| e.steps.iter_mut())
    {
        if let Step::Approval(a) = step {
            a.rules = vec!["bash(echo one)".into()];
        }
    }
    let _ = app.update(Msg::Answer(Decision::Always));
    let _ = written.try_recv();
    assert_eq!(app.prefs.more.allowed["s-always"], vec!["bash(echo one)".to_owned()]);
    let spec = crate::runner::RunSpec {
        program: "sasacode".into(),
        cwd: ".".into(),
        prompt: "go".into(),
        resume: None,
        model: None,
        permission: None,
        thinking: None,
        no_session: false,
        lang: None,
        allow: app.prefs.more.allowed["s-always"].clone(),
        trust_project: false,
        slash: false,
        system_file: None,
    };
    assert!(spec.args().windows(2).any(|w| w == ["--allow", "bash(echo one)"]));
    // Listed in the settings, it can be kept in this folder's settings for later conversations.
    let w = crate::words::JA;
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Settings)));
    assert!(says(&app, w.allowed_keep_here) && says(&app, w.allowed_keep_all) && says(&app, w.allowed_keep_note));
    let folder = std::env::temp_dir().join(format!("sasacode-gui-keep-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::create_dir_all(&folder).unwrap();
    app.cwd = folder.clone();
    let _ = app.update(Msg::KeepAllowed("bash(echo one)".into(), true));
    let kept = std::fs::read_to_string(folder.join(".sasacode/config.json")).unwrap();
    assert!(kept.contains("\"bash(echo one)\""), "{kept}");
    assert!(app.toast.as_deref().is_some_and(|t| t.ends_with("に残しました")));
    let _ = app.update(Msg::KeepAllowed("bash(echo one)".into(), true));
    assert!(app.toast.as_deref().is_some_and(|t| t.ends_with("にもうあります")));
    let _ = std::fs::remove_dir_all(&folder);
    let _ = app.update(Msg::ForgetAllowed("bash(echo one)".into()));
    assert!(!app.prefs.more.allowed.contains_key("s-always"));
}

pub(crate) fn listed() -> crate::cli::Listing {
    fn json<T: serde::de::DeserializeOwned>(s: &str) -> T {
        serde_json::from_str(s).unwrap()
    }
    crate::cli::Listing {
        models: Some(json(
            r#"[{"id":"local/gemma4","contextWindow":131072,"current":true},{"id":"cloud/big"}]"#,
        )),
        commands: Some(json(
            r#"[{"name":"help","description":"h","owner":"core-commands","headless":false},
                {"name":"review","description":"変更を見直す","argumentHint":"[path]","owner":"skills","headless":true}]"#,
        )),
        skills: Some(json(
            r#"[{"name":"review","description":"変更を見直す","path":"/s/review"}]"#,
        )),
        plugins: Some(json(
            r#"[{"name":"todo","version":"1.0.0","source":"bundled","enabled":true},
                {"name":"broken","source":"global","enabled":false,"error":"API 9 is newer"}]"#,
        )),
        trust: Some(json(
            r#"{"trusted":false,"needed":true,"complete":true,"items":["plugin lint","MCP server db"],"mcpServers":["db"]}"#,
        )),
        version: Some("0.8.0".into()),
    }
}

#[test]
fn the_plugins_and_skills_commands_are_offered_after_a_slash_and_run_as_commands() {
    let mut app = window("ja");
    app.listing = listed();
    app.plugin_commands = vec![sasacode_protocol::event::Command {
        name: "hello".into(),
        description: "say hello".into(),
        argument_hint: None,
        owner: "asker".into(),
    }];
    app.set_input("/");
    app.chat
        .editor
        .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    assert!(says(&app, "/review") && says(&app, "/hello") && !says(&app, "/help"));
    app.set_input("/rev");
    let Some(crate::input::Offer {
        kind: crate::input::OfferKind::Commands(window_commands, plugins),
        ..
    }) = app.offer()
    else {
        panic!("nothing offered");
    };
    assert!(window_commands.is_empty() && plugins[0].name == "review");
    let _ = app.update(Msg::Complete(None));
    assert_eq!(app.chat.editor.text().trim_end(), "/review");
    assert!(app.is_slash("/review src") && app.is_slash("/hello") && !app.is_slash("/help") && !app.is_slash("review"));
    app.set_input("/review src");
    let _ = app.update(Msg::Send);
    assert!(app.running(), "it runs");
    assert!(app.chat.editor.text().trim().is_empty());
}

#[test]
fn an_untrusted_folder_says_what_trust_would_use_and_a_window_can_use_it_alone() {
    let mut app = window("ja");
    app.listing = listed();
    let w = crate::words::JA;
    assert!(says(&app, w.trust_head) && says(&app, "・MCP server db") && says(&app, w.trust_save));
    act(&mut app, |ui| {
        ui.click(w.trust_once).expect("the button is there");
    });
    assert!(app.trust_once && !says(&app, w.trust_head));
    // Its plugins' code could not all be read: trust cannot be kept, only used here.
    let mut app = window("ja");
    app.listing = listed();
    app.listing.trust.as_mut().unwrap().complete = false;
    assert!(says(&app, w.trust_partial) && !says(&app, w.trust_save));
}

#[test]
fn the_models_the_cli_lists_are_picked_from_a_menu_and_the_rest_from_a_screen() {
    let mut app = window("ja");
    app.listing = listed();
    let _ = app.update(Msg::OpenMenu(crate::rows::MenuFor::Models));
    assert!(says(&app, "cloud/big") && says(&app, "local/gemma4 · 131.1k"));
    act(&mut app, |ui| {
        ui.click("cloud/big").expect("the model is there");
    });
    assert_eq!(app.model, "cloud/big");
    assert!(app.menu.is_none());

    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Extensions)));
    for said in ["todo", "API 9 is newer", "/review", "変更を見直す", "db"] {
        assert!(says(&app, said), "{said}");
    }
}

#[test]
fn a_picture_a_reply_names_is_shown_and_opens_larger() {
    let dir = std::env::temp_dir().join(format!("sasacode-gui-picture-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A PNG of one pixel.
    let png = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1,
        8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf,
        0xc0, 0xf0, 0x1f, 0, 0x05, 0, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0, 0, 0, 0, 0x49, 0x45, 0x4e, 0x44, 0xae,
        0x42, 0x60, 0x82,
    ];
    std::fs::write(dir.join("shot.png"), png).unwrap();
    use crate::files::local_image;
    assert_eq!(local_image("shot.png", &dir), Some(dir.join("shot.png")));
    let absolute = format!("file://{}", dir.join("shot.png").display());
    assert!(local_image(&absolute, &dir).is_some());
    assert!(local_image("https://example.com/shot.png", &dir).is_none());
    assert!(local_image("missing.png", &dir).is_none() && local_image("notes.md", &dir).is_none());

    let mut app = window("ja");
    let _ = app.update(Msg::ViewImage(dir.join("shot.png")));
    let w = crate::words::JA;
    assert!(says(&app, "shot.png") && says(&app, w.image_actual));
    act(&mut app, |ui| {
        ui.click(w.image_actual).expect("the button is there");
    });
    assert!(app.image_actual && says(&app, w.image_fit));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_side_question_can_be_followed_by_another_and_brought_into_the_input() {
    use crate::runner::RunUpdate;
    let mut app = window("ja");
    let _ = app.side_question("この関数は何をする？".into());
    let side = crate::app::SIDE;
    let _ = app.update(Msg::Run(side, RunUpdate::Exited(Some(0))));
    let _ = app.update(Msg::SideType("テストはある？".into()));
    let _ = app.update(Msg::SideAsk);
    let aside = app.aside.as_ref().expect("the side chat is kept");
    let prompts: Vec<&str> = aside.transcript.exchanges.iter().map(|e| e.prompt.as_str()).collect();
    assert_eq!(prompts, ["この関数は何をする？", "テストはある？"]);
    assert!(app.side_typed.is_empty() && aside.running());
    assert!(says(&app, "この関数は何をする？") && says(&app, crate::words::JA.side_bring));
    let _ = app.update(Msg::Run(side, RunUpdate::Exited(Some(0))));
    let _ = app.update(Msg::SideBring);
    assert!(app.aside.is_none() && app.overlay.is_none());
    let typed = app.chat.editor.text();
    assert!(
        typed.contains("> この関数は何をする？") && typed.contains("> テストはある？"),
        "{typed}"
    );
}

#[test]
fn a_title_the_model_writes_names_a_conversation_the_user_did_not() {
    let mut app = window("ja");
    let id = app.sessions[0].id.clone();
    app.prefs.names.remove(&id);
    // Off unless the settings say so.
    let _ = app.write_title(&id, false);
    assert!(app.titling.is_empty());
    app.prefs.more.auto_title = true;
    let _ = app.update(Msg::Titled(id.clone(), false, Ok("「再試行の設計」\n".into())));
    assert_eq!(app.prefs.names[&id], "再試行の設計");
    // One the user gave stays, unless they ask for the model's.
    app.prefs.names.insert(id.clone(), "自分の名前".into());
    let _ = app.update(Msg::Titled(id.clone(), false, Ok("別の題".into())));
    assert_eq!(app.prefs.names[&id], "自分の名前");
    let _ = app.update(Msg::Titled(id.clone(), true, Ok("別の題".into())));
    assert_eq!(app.prefs.names[&id], "別の題");
    let _ = app.update(Msg::OpenMenu(crate::rows::MenuFor::Row(id)));
    assert!(says(&app, crate::words::JA.title_write));
}

#[test]
fn back_and_forward_go_between_the_conversations_opened_and_keys_change_the_next_run() {
    let mut app = window("ja");
    let dir = std::env::temp_dir().join(format!("sasacode-gui-back-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, name) in ["a.jsonl", "b.jsonl"].into_iter().enumerate() {
        let path = dir.join(name);
        let lines = [
            serde_json::json!({"type":"session","version":3,"id":name,"cwd":"/w","createdAt":"2026-01-01T00:00:00.000Z"}),
            serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":name}],"timestamp":1}}),
        ];
        std::fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        app.sessions[i + 1].path = path;
    }
    let (a, b) = (app.sessions[1].clone(), app.sessions[2].clone());
    assert!(app.open_session(&a.path));
    assert!(app.open_session(&b.path));
    let _ = app.update(Msg::Back);
    assert_eq!(app.chat.session.as_deref(), Some(a.id.as_str()));
    let _ = app.update(Msg::Forward);
    assert_eq!(app.chat.session.as_deref(), Some(b.id.as_str()));
    // Opening another clears what was ahead.
    let _ = app.update(Msg::Back);
    let _ = app.update(Msg::NewSession);
    assert!(app.forward.is_empty());
    let _ = app.update(Msg::Back);
    assert_eq!(app.chat.session.as_deref(), Some(a.id.as_str()));

    let tap = |app: &mut App, key: &str, shift: bool| {
        let mut modifiers = Modifiers::COMMAND;
        if shift {
            modifiers |= Modifiers::SHIFT;
        }
        let msg = shortcut(&Key::Character(key.into()), modifiers).expect("bound");
        let _ = app.update(msg);
    };
    app.effort = None;
    tap(&mut app, "t", true);
    assert_eq!(app.effort, Some("off"));
    tap(&mut app, "t", true);
    assert_eq!(app.effort, Some("low"));
    app.permission = Some("agent");
    tap(&mut app, "l", true);
    assert_eq!(app.permission, Some("plan"), "auto is never reached by the key");
    assert_eq!(app.before_plan, Some("agent"));
    tap(&mut app, "l", true);
    assert_eq!(app.permission, None);
    assert!(matches!(
        shortcut(&Key::Character("[".into()), Modifiers::COMMAND),
        Some(Msg::Back)
    ));
    assert!(matches!(
        shortcut(&Key::Character("]".into()), Modifiers::COMMAND),
        Some(Msg::Forward)
    ));
    assert!(matches!(
        shortcut(&Key::Named(Named::Tab), Modifiers::CTRL),
        Some(Msg::Neighbour(1))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Real logs for the window's second and third conversations, in a folder of `tag`'s.
fn real_logs(app: &mut App, tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("sasacode-gui-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (i, name) in ["a.jsonl", "b.jsonl"].into_iter().enumerate() {
        let path = dir.join(name);
        let lines = [
            serde_json::json!({"type":"session","version":3,"id":name,"cwd":"/w","createdAt":"2026-01-01T00:00:00.000Z"}),
            serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":name}],"timestamp":1}}),
        ];
        std::fs::write(&path, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        app.sessions[i + 1].path = path;
    }
    dir
}

#[test]
fn a_folders_window_opens_again_on_its_last_conversation_choices_and_drafts() {
    let mut app = window("ja");
    let dir = real_logs(&mut app, "restore");
    let (a, b) = (app.sessions[1].clone(), app.sessions[2].clone());
    // What a run used becomes the folder's.
    app.model = "a/m".into();
    app.permission = Some("agent");
    app.effort = Some("high");
    let key = app.chat.key;
    let _ = app.start_run(key, "はじめ".into(), false);
    let folder = &app.prefs.more.folders[&app.folder_key()];
    assert_eq!(
        (folder.model.as_str(), folder.permission.as_deref()),
        ("a/m", Some("agent"))
    );
    // Typed and left: kept by the conversation's id; the last one shown is the folder's.
    assert!(app.open_session(&a.path));
    app.set_input("あとで送る");
    assert!(app.open_session(&b.path));
    assert_eq!(app.prefs.more.drafts.get(&a.id).map(String::as_str), Some("あとで送る"));
    app.set_input("こちらも");
    let _ = app.update(Msg::Quit(iced::window::Id::unique()));
    assert_eq!(
        app.prefs.more.folders[&app.folder_key()].last.as_deref(),
        Some(b.id.as_str())
    );
    assert_eq!(app.prefs.more.drafts.get(&b.id).map(String::as_str), Some("こちらも"));

    // A new window of the folder: as it was left, but never running everything without asking.
    let mut prefs = app.prefs.clone();
    prefs.more.folders.get_mut(&app.folder_key()).unwrap().permission = Some("auto".into());
    let mut again = window("ja");
    let _ = real_logs(&mut again, "restore");
    again.prefs = prefs;
    again.restore_folder(false);
    assert_eq!(again.chat.session.as_deref(), Some(b.id.as_str()));
    assert_eq!(again.chat.editor.text().trim_end(), "こちらも");
    assert_eq!(
        (again.model.as_str(), again.permission, again.effort),
        ("a/m", None, Some("high"))
    );
    assert!(again.open_session(&a.path));
    assert_eq!(again.chat.editor.text().trim_end(), "あとで送る");
    // Sent, it is no longer kept.
    let key = again.chat.key;
    let _ = again.start_run(key, "あとで送る".into(), false);
    assert!(!again.prefs.more.drafts.contains_key(&a.id));

    // A new conversation's draft is the folder's, and comes back when the window opens on one.
    let mut fresh = window("ja");
    let _ = fresh.update(Msg::NewSession);
    fresh.set_input("新しい話");
    let _ = fresh.update(Msg::Quit(iced::window::Id::unique()));
    let mut next = window("ja");
    next.prefs = fresh.prefs.clone();
    next.restore_folder(false);
    assert!(next.chat.session.is_none());
    assert_eq!(next.chat.editor.text().trim_end(), "新しい話");
    // Opened on its last conversation, the window's new one (⌘N) has it too.
    let mut on_last = window("ja");
    let _ = real_logs(&mut on_last, "restore");
    on_last.prefs = app.prefs.clone();
    on_last.prefs.more.drafts.insert(app.folder_key(), "新しい話".into());
    on_last.restore_folder(false);
    assert!(on_last.chat.session.is_some());
    let _ = on_last.update(Msg::NewSession);
    assert!(on_last.chat.session.is_none());
    assert_eq!(on_last.chat.editor.text().trim_end(), "新しい話");
    // Asked for one, the window opens on it and not on the last.
    let mut asked = window("ja");
    asked.prefs = app.prefs.clone();
    asked.restore_folder(true);
    assert!(asked.chat.session.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_settings_choose_the_notifications_their_sound_and_the_editor() {
    let mut app = window("ja");
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Settings)));
    let w = crate::words::JA;
    // A switch's label is not text the simulator finds; the editor's heading is.
    assert!(says(&app, w.set_editor) && says(&app, w.editor_ph));
    let _ = app.update(Msg::SetQuietWait(true));
    let _ = app.update(Msg::SetSound(true));
    let _ = app.update(Msg::SetEditor("zed".into()));
    assert!(app.prefs.more.quiet_wait && !app.prefs.more.quiet_done && app.prefs.more.sound);
    assert_eq!(app.prefs.more.editor, "zed");
    // The editors found on the PATH are listed to choose from (here, as if two were).
    app.editors = vec![("VS Code", "code"), ("Zed", "zed")];
    assert!(says(&app, w.editors_found) && says(&app, "VS Code") && says(&app, "Zed"));
    let _ = app.update(Msg::SetEditor("code".into()));
    assert_eq!(app.prefs.more.editor, "code");
    // None found: nothing listed.
    app.editors.clear();
    assert!(!says(&app, w.editors_found));
}

#[test]
fn settings_files_that_cannot_be_read_are_said_when_the_window_opens_and_its_own_is_kept_aside() {
    let w = crate::words::JA;
    let dir = std::env::temp_dir().join(format!("sasacode-gui-unreadable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (home, folder) = (dir.join("home"), dir.join("project"));
    std::fs::create_dir_all(folder.join(".sasacode")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.json"), "{ \"permissions\": ").unwrap();
    std::fs::write(home.join("gui.json"), "{ broken").unwrap();
    // A folder's that reads, and one that holds nothing, are not said.
    std::fs::write(folder.join(".sasacode/config.json"), "  \n").unwrap();
    let mut app = window("ja");
    app.home = Some(home.clone());
    app.cwd = folder;
    app.layout_file = Some(home.join("gui.json"));
    app.check_settings();
    let said: Vec<_> = app.unreadable.iter().map(|(f, _)| f.clone()).collect();
    assert_eq!(said, vec![home.join("gui.json"), home.join("config.json")]);
    assert_eq!(
        std::fs::read_to_string(home.join("gui.json.broken")).unwrap(),
        "{ broken"
    );
    assert!(says(&app, w.unreadable_open));
    // Put away one by one.
    let _ = app.update(Msg::UnreadableSeen(home.join("gui.json")));
    let _ = app.update(Msg::UnreadableSeen(home.join("config.json")));
    assert!(app.unreadable.is_empty() && !says(&app, w.unreadable_open));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_worktree_list_offers_a_branch_to_start_from_and_to_clean_up_old_ones() {
    let mut app = window("ja");
    let w = crate::words::JA;
    let _ = app.update(Msg::Worktrees(
        vec![crate::worktrees::Tree {
            path: "/tmp/home/worktrees/orders-api-1".into(),
            branch: Some("sasacode/1".into()),
        }],
        vec!["main".into(), "origin/release".into()],
    ));
    let labels: Vec<String> = app.worktree_entries("").into_iter().map(|e| e.label).collect();
    assert!(
        labels.contains(&"ブランチmainから新しいワークツリー".to_owned()),
        "{labels:?}"
    );
    assert!(labels.contains(&"ブランチorigin/releaseから新しいワークツリー".to_owned()));
    assert!(labels.contains(&crate::i18n::fill(w.worktree_sweep, "days", crate::worktrees::OLD_DAYS)));
    // Found by what is typed.
    let found: Vec<String> = app.worktree_entries("release").into_iter().map(|e| e.label).collect();
    assert_eq!(found, vec!["ブランチorigin/releaseから新しいワークツリー".to_owned()]);
    // What the clean-up did is said.
    let _ = app.update(Msg::WorktreesSwept(2, vec!["orders-api-3".into()]));
    let said = app.toast.clone().unwrap_or_default();
    assert!(
        said.contains("2件を片付けました") && said.contains("orders-api-3"),
        "{said}"
    );
    let _ = app.update(Msg::SetWorktreePrefix("me/".into()));
    assert_eq!(app.prefs.more.worktree_prefix, "me/");
}

#[test]
fn the_choices_made_now_become_the_defaults_of_a_folder_never_run_in() {
    let w = crate::words::JA;
    let mut app = window("ja");
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Settings)));
    assert!(says(&app, w.defaults_none) && !says(&app, w.defaults_forget));
    app.model = "local/gemma4".into();
    app.permission = Some("edits");
    app.effort = Some("high");
    // Below where the settings are scrolled to: pressed by its message.
    assert!(says(&app, w.defaults_keep));
    let _ = app.update(Msg::KeepDefaults(true));
    let defaults = app.prefs.more.defaults.clone().unwrap();
    assert_eq!(
        (
            defaults.model.as_str(),
            defaults.permission.as_deref(),
            defaults.effort.as_deref()
        ),
        ("local/gemma4", Some("edits"), Some("high"))
    );
    assert!(says(&app, "モデル local/gemma4・権限 edits・推論の深さ high"));

    // A folder with none of its own starts with them; one with its own keeps those.
    let mut fresh = window("ja");
    fresh.prefs.more.defaults = Some(defaults.clone());
    fresh.restore_folder(true);
    assert_eq!(
        (fresh.model.as_str(), fresh.permission, fresh.effort),
        ("local/gemma4", Some("edits"), Some("high"))
    );
    let mut own = window("ja");
    own.prefs.more.defaults = Some(defaults);
    own.prefs.more.folders.insert(
        own.folder_key(),
        crate::layout::Folder {
            model: "fable".into(),
            permission: Some("ask".into()),
            ..Default::default()
        },
    );
    own.restore_folder(true);
    assert_eq!(
        (own.model.as_str(), own.permission, own.effort),
        ("fable", Some("ask"), Some("high"))
    );

    // Forgotten: none.
    let _ = app.update(Msg::KeepDefaults(false));
    assert!(app.prefs.more.defaults.is_none() && says(&app, w.defaults_none));
}

/// sasacode's colour is chosen by name or as #RRGGBB and shows at once, in light and in dark; the
/// fonts are named, for the windows opened after.
#[test]
fn the_accent_colour_shows_at_once_and_the_fonts_are_kept_for_the_next_window() {
    let mut app = window("ja");
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Settings)));
    let w = crate::words::JA;
    assert!(says(&app, w.set_accent) && says(&app, w.accent_blue) && says(&app, w.fonts_note));
    let green = app.t.accent;
    let _ = app.update(Msg::SetAccent("blue".into()));
    assert_eq!(app.t.accent, iced::Color::from_rgb8(0x25, 0x63, 0xEB));
    let _ = app.update(Msg::SetDark(Some(true)));
    assert_eq!(app.t.accent, iced::Color::from_rgb8(0x60, 0xA5, 0xFA));
    let _ = app.update(Msg::SetAccent("#ff8800".into()));
    assert_eq!(app.t.accent, iced::Color::from_rgb8(0xFF, 0x88, 0x00));
    assert_eq!(
        app.t.on_accent,
        iced::Color::from_rgb8(0x0D, 0x0D, 0x0D),
        "dark on a light colour"
    );
    // Half typed, it is not a colour yet: the green stays.
    let _ = app.update(Msg::SetDark(Some(false)));
    let _ = app.update(Msg::SetAccent("#ff8".into()));
    assert_eq!(app.t.accent, green);
    let _ = app.update(Msg::SetUiFont("Noto Sans JP".into()));
    let _ = app.update(Msg::SetCodeFont("JetBrains Mono".into()));
    assert_eq!(
        (app.prefs.more.ui_font.as_str(), app.prefs.more.code_font.as_str()),
        ("Noto Sans JP", "JetBrains Mono")
    );
}

#[test]
fn the_about_screen_says_the_versions_and_what_the_runs_wrote_and_a_warning_is_said_once() {
    use crate::runner::RunUpdate;
    let mut app = window("ja");
    app.listing = listed();
    let key = app.chat.key;
    let _ = app.start_run(key, "やって".into(), false);
    for _ in 0..2 {
        let _ = app.update(Msg::Run(
            key,
            RunUpdate::Stderr("警告: config.json の permissions は古い書き方です".into()),
        ));
    }
    let _ = app.update(Msg::Run(key, RunUpdate::Stderr("debug: started".into())));
    let notes = app
        .chat
        .transcript
        .exchanges
        .iter()
        .flat_map(|e| &e.steps)
        .filter(|s| matches!(s, crate::transcript::Step::Note(Note::Notice(_))))
        .count();
    assert_eq!(notes, 1, "a warning each run says again is noted once");
    assert_eq!(app.log.len(), 3);
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::About)));
    let w = crate::words::JA;
    assert!(says(&app, "0.8.0") && says(&app, w.about_log) && says(&app, w.about_copy));
    let about = app.about_text();
    assert!(
        about.contains("sasacode 0.8.0") && about.contains("debug: started"),
        "{about}"
    );
}

/// The other folders sasacode was used in are listed under the conversations, this one left out,
/// folded until asked and kept so; each opens in a window of its own.
#[test]
fn the_other_folders_are_listed_under_the_conversations_and_open_in_their_own_window() {
    let mut app = window("ja");
    assert!(!says(&app, "ほかのフォルダ"), "none read yet, none listed");
    let _ = app.update(Msg::Others(vec![
        app.cwd.clone(),
        "/Users/me/src/billing-web".into(),
        "/Users/me/work/infra".into(),
    ]));
    assert_eq!(app.others.len(), 2);
    assert!(says(&app, "ほかのフォルダ") && says(&app, "(2)"));
    assert!(!says(&app, "billing-web"));
    let _ = app.update(Msg::FoldFolders);
    assert!(app.prefs.more.folders_open);
    assert!(says(&app, "billing-web") && says(&app, "/Users/me/src") && says(&app, "infra"));
    let _ = app.update(Msg::FoldFolders);
    assert!(!says(&app, "billing-web"));
}

/// The background jobs: listed with their state and when they started, a job's output shown on a
/// click, and one still running stopped only on the second press.
#[test]
fn the_background_jobs_are_listed_and_a_running_one_is_stopped_on_the_second_press() {
    use crate::jobs::Job;
    let mut app = window("ja");
    let _ = app.update(Msg::Command(Command::Jobs));
    assert_eq!(app.overlay, Some(crate::overlay::Overlay::Jobs));
    assert!(says(&app, "バックグラウンドジョブ") && says(&app, "まだありません"));
    let made = |id: &str, command: &str, running: bool, exit_code: Option<i64>| Job {
        id: id.into(),
        command: command.into(),
        cwd: "/Users/me/src/orders-api".into(),
        pid: 0,
        started_at: 1_794_900_000_000,
        exit_code,
        killed: None,
        dir: std::env::temp_dir().join("sasacode-no-such-job"),
        running,
        tail: "test result: ok".into(),
    };
    let _ = app.update(Msg::Jobs(vec![
        made("bg-2", "cargo test --workspace", true, None),
        made("bg-1", "npm run build", false, Some(1)),
    ]));
    assert!(says(&app, "cargo test --workspace") && says(&app, "実行中"));
    assert!(says(&app, "npm run build") && says(&app, "終了（1）"));
    assert!(!says(&app, "test result: ok"));
    let _ = app.update(Msg::JobOpen("bg-1".into()));
    assert!(says(&app, "test result: ok"));
    if cfg!(unix) {
        let _ = app.update(Msg::JobStop("bg-2".into()));
        assert!(says(&app, "もう一度押すと止めます") && app.toast.is_none());
        // The second press: the job is gone from where it was kept, and nothing is signalled.
        let _ = app.update(Msg::JobStop("bg-2".into()));
        assert_eq!(app.toast.as_deref(), Some("もう終わっていました"));
    }
}

/// With no AGENTS.md here, the agent can be asked to write one: the request goes to the input.
#[test]
fn the_agent_is_asked_to_write_an_agents_md_from_the_extensions() {
    let mut app = window("ja");
    let dir = std::env::temp_dir().join(format!("sasacode-gui-write-instr-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    app.cwd = dir.clone();
    app.listing = listed();
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Extensions)));
    assert!(says(&app, "エージェントに書いてもらう"));
    let _ = app.update(Msg::WriteInstructions);
    assert_eq!(app.overlay, None);
    assert!(app.chat.editor.text().contains("AGENTS.mdを書いてください"));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The list shows the conversations done too; the settings name the sasacode run, empty for the
/// one on PATH, and whether to notify with the window in front.
#[test]
fn the_list_shows_those_done_and_the_settings_name_the_sasacode_run() {
    let mut app = window("ja");
    let _ = app.update(Msg::ListTools);
    assert!(says(&app, "完了"));
    let _ = app.update(Msg::ListShow(crate::layout::Show::Done));
    let shown = app.sessions.iter().filter(|s| app.shows(s)).count();
    assert_eq!(shown, app.sessions.len(), "none runs here");
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Settings)));
    let w = crate::words::JA;
    assert!(says(&app, w.set_program) && says(&app, w.program_ph));
    let _ = app.update(Msg::SetProgram("/opt/sasacode/bin/sasacode".into()));
    assert_eq!(app.program, std::path::PathBuf::from("/opt/sasacode/bin/sasacode"));
    assert_eq!(app.prefs.more.program, "/opt/sasacode/bin/sasacode");
    let _ = app.update(Msg::SetProgram(String::new()));
    assert_eq!(app.program, crate::app::program_of(""));
    let _ = app.update(Msg::SetNotifyAlways(true));
    assert!(app.prefs.more.notify_always);
}

/// "Check for updates" asks the CLI on a click and says its line; a check not made says why.
#[test]
fn updates_are_checked_on_a_click_and_what_the_cli_said_is_shown() {
    let mut app = window("ja");
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::About)));
    assert!(says(&app, "更新を確かめる"));
    let _ = app.update(Msg::CheckUpdate);
    assert!(app.checking_update && says(&app, "確かめています…"));
    let _ = app.update(Msg::UpdateChecked(Ok(
        "newer version available: v0.9.0 (this is 0.8.0)".into(),
    )));
    assert!(!app.checking_update);
    assert!(says(&app, "newer version available: v0.9.0 (this is 0.8.0)"));
    assert!(says(&app, crate::words::JA.about_update_hint));
    let _ = app.update(Msg::CheckUpdate);
    let _ = app.update(Msg::UpdateChecked(Err("sasacode: unknown command update".into())));
    assert!(says(&app, "確かめられませんでした：sasacode: unknown command update"));
}

#[test]
fn a_review_is_asked_for_in_the_input_and_the_instruction_files_open_from_the_extensions() {
    let mut app = window("ja");
    let _ = app.update(Msg::AskReview);
    assert!(app.chat.editor.text().contains("git diff"));
    let dir = std::env::temp_dir().join(format!("sasacode-gui-instr-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    app.cwd = dir.clone();
    app.listing = listed();
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Extensions)));
    let w = crate::words::JA;
    assert!(says(&app, w.instr_head) && says(&app, w.instr_make));
    // Made empty, then opened; listed from then on.
    // An editor that opens nothing and ends.
    app.prefs.more.editor = if cfg!(windows) { "cmd /C rem" } else { "true" }.into();
    let _ = app.update(Msg::OpenInstructions(dir.join("AGENTS.md")));
    assert!(dir.join("AGENTS.md").is_file());
    assert!(says(&app, "AGENTS.md") && !says(&app, w.instr_make));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_scratch_conversation_is_not_listed_and_its_log_goes_once_it_is_left() {
    use crate::runner::RunUpdate;
    let mut app = window("ja");
    let home = app.home.clone().unwrap();
    let dir = real_logs(&mut app, "scratch");
    let (a, b) = (app.sessions[1].clone(), app.sessions[2].clone());
    let _ = app.update(Msg::NewScratch);
    assert!(app.chat.scratch && says(&app, crate::words::JA.scratch_tag));
    // Its first run named it as the second conversation's log, and is over.
    app.chat.session = Some(a.id.clone());
    assert!(!app.shows(&a), "a scratch conversation is not listed");
    let _ = app.update(Msg::NewSession);
    assert!(!a.path.exists(), "left, its log is in the trash");
    assert!(std::fs::read_dir(home.join("trash")).unwrap().count() > 0);
    assert!(
        app.prefs
            .more
            .folders
            .get(&app.folder_key())
            .is_none_or(|f| f.last.as_deref() != Some(a.id.as_str())),
        "never what the window opens on again"
    );
    // Left while it runs, it is kept until the run is over, then goes. Sent from, it leaves
    // what was typed for the folder's new conversation alone.
    let folder = app.folder_key();
    app.prefs.more.drafts.insert(folder.clone(), "まだ送らない".into());
    let _ = app.update(Msg::NewScratch);
    let key = app.chat.key;
    let _ = app.start_run(key, "ちょっと聞く".into(), false);
    assert!(app.prefs.more.drafts.contains_key(&folder));
    app.chat.session = Some(b.id.clone());
    let _ = app.update(Msg::NewSession);
    assert!(b.path.exists() && app.parked.iter().any(|c| c.scratch));
    let _ = app.update(Msg::Run(key, RunUpdate::Exited(Some(0))));
    assert!(!app.parked.iter().any(|c| c.scratch));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_scratch_conversation_with_no_folder_runs_in_an_empty_one_and_its_log_goes_from_there() {
    let mut app = window("ja");
    let home = std::env::temp_dir().join(format!("sasacode-gui-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    app.home = Some(home.clone());
    let _ = app.update(Msg::NewBareScratch);
    assert!(app.chat.scratch && app.chat.bare && app.is_bare(app.chat.key));
    assert!(says(&app, crate::words::JA.bare_tag));
    // Its folder: empty, under the home, not this window's.
    let bare = app.bare_folder();
    assert!(bare.is_dir() && bare.starts_with(&home) && bare != app.cwd);
    // Its log is under that folder's; left, it goes to the trash.
    let logs = sasacode_protocol::session_dir(&home, &bare);
    std::fs::create_dir_all(&logs).unwrap();
    let log = logs.join("bare0001.jsonl");
    let lines = [
        serde_json::json!({"type":"session","version":3,"id":"bare0001","cwd":bare.to_string_lossy(),"createdAt":"2026-01-01T00:00:00.000Z"}),
        serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"ちょっと"}],"timestamp":1}}),
    ];
    std::fs::write(&log, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
    app.chat.session = Some("bare0001".into());
    let _ = app.update(Msg::NewSession);
    assert!(!log.exists(), "left, its log is in the trash");
    // The one with a folder is not run in that one.
    let _ = app.update(Msg::NewScratch);
    assert!(app.chat.scratch && !app.is_bare(app.chat.key));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_paragraph_of_a_reply_with_links_offers_them_on_a_right_click() {
    use iced::mouse::{Button, Event as Mouse};
    let mut app = window("ja");
    let w = crate::words::JA;
    let md = crate::transcript::Markdown::new(
        "[手順](https://example.com/guide)と[client.ts](src/client.ts#L8)を見てください。\n\nリンクのない段落です。",
    );
    // Pressed with the right button over the first paragraph: its two links, in order.
    let produced: Vec<Msg> = {
        let settings = Settings {
            default_font: crate::view::ui(),
            ..Settings::default()
        };
        let mut ui = Simulator::with_size(
            settings,
            Size::new(600.0, 400.0),
            crate::view::timeline::reply(&app, &md, false),
        );
        ui.point_at(iced::Point::new(30.0, 10.0));
        let _ = ui.simulate([
            iced::Event::Mouse(Mouse::ButtonPressed(Button::Right)),
            iced::Event::Mouse(Mouse::ButtonReleased(Button::Right)),
        ]);
        ui.into_messages().collect()
    };
    let links = produced.iter().find_map(|m| match m {
        Msg::OpenMenu(crate::rows::MenuFor::Links(links)) => Some(links.clone()),
        _ => None,
    });
    assert_eq!(
        links,
        Some(vec![
            "https://example.com/guide".to_owned(),
            "src/client.ts#L8".to_owned()
        ])
    );
    // Its menu: each opened and copied, and the folder's file in the editor at its line.
    let dir = std::env::temp_dir().join(format!("sasacode-gui-links-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/client.ts"), "export {};\n").unwrap();
    app.cwd = dir.clone();
    let _ = app.update(Msg::OpenMenu(crate::rows::MenuFor::Links(links.unwrap())));
    assert!(says(&app, "開く：https://example.com/guide") && says(&app, "コピー：https://example.com/guide"));
    assert!(says(&app, "開く：src/client.ts#L8") && says(&app, "エディタで開く"));
    assert!(w.menu_open_link.contains("{link}"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_the_search_inside_the_conversation_finds_is_marked_in_the_words_themselves() {
    use iced::advanced::text::{Highlight, Span};
    let mark = Highlight {
        background: iced::Color::BLACK.into(),
        border: iced::Border::default(),
    };
    let spans: Vec<Span<'static, ()>> = vec![Span::new("Retry the REquest"), Span::new("再試行を見る")];
    let pieces = |needle: &str| -> Vec<(String, bool)> {
        crate::view::timeline::found_marked(&spans, needle, mark)
            .into_iter()
            .map(|s| (s.text.to_string(), s.highlight.is_some()))
            .collect()
    };
    let own = |all: &[(&str, bool)]| all.iter().map(|(t, m)| ((*t).to_owned(), *m)).collect::<Vec<_>>();
    assert_eq!(
        pieces("re"),
        own(&[
            ("Re", true),
            ("try the ", false),
            ("RE", true),
            ("quest", false),
            ("再試行を見る", false)
        ])
    );
    assert_eq!(
        pieces("試行"),
        own(&[
            ("Retry the REquest", false),
            ("再", false),
            ("試行", true),
            ("を見る", false)
        ])
    );
    // Nothing looked for: as it was.
    assert_eq!(pieces("").len(), 2);
    // The search's words, lower case; none while nothing is typed.
    let mut app = window("ja");
    app.find_in = Some("  ReTry ".into());
    assert_eq!(app.find_needle().as_deref(), Some("retry"));
    app.find_in = Some("   ".into());
    assert!(app.find_needle().is_none());
}

#[test]
fn a_file_is_edited_and_saved_in_the_files_tab_a_markdown_one_previewed_and_a_picture_shown() {
    use text_editor::{Action, Edit, Motion};
    let mut app = window("ja");
    let dir = std::env::temp_dir().join(format!("sasacode-gui-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    app.cwd = dir.clone();
    let readme = dir.join("README.md");
    std::fs::write(&readme, "# 見出しの行\n本文\n").unwrap();
    let w = crate::words::JA;
    app.tab = Tab::Files;
    app.insp = true;
    let _ = app.update(Msg::FileRead(Box::new(crate::files::read(readme.clone(), None))));
    // As it reads.
    let _ = app.update(Msg::FilePreview(true));
    assert!(app.file_preview.is_some() && says(&app, w.preview_off));
    let _ = app.update(Msg::FilePreview(false));
    // Edited and saved, its last line's end kept.
    let _ = app.update(Msg::FileEditStart);
    assert!(says(&app, w.edit_cancel) && says(&app, w.edit_save));
    let _ = app.update(Msg::FileEditAction(Action::Move(Motion::DocumentEnd)));
    for c in "追記".chars() {
        let _ = app.update(Msg::FileEditAction(Action::Edit(Edit::Insert(c))));
    }
    // Another file is not opened over what is typed.
    let _ = app.update(Msg::ViewFile(dir.join("other.txt"), None));
    assert_eq!(app.toast.as_deref(), Some(w.edit_unsaved));
    let _ = app.update(Msg::FileSave);
    assert_eq!(std::fs::read_to_string(&readme).unwrap(), "# 見出しの行\n本文\n追記\n");
    assert!(app.file_edit.is_none());
    // Shown as it is now.
    assert!(app.viewing.as_ref().unwrap().lines.iter().any(|l| l.contains("追記")));
    // Written by another since it was read: not written over.
    let _ = app.update(Msg::FileEditStart);
    let _ = app.update(Msg::FileEditAction(Action::Edit(Edit::Insert('x'))));
    std::fs::write(&readme, "他の誰か\n").unwrap();
    let _ = app.update(Msg::FileSave);
    assert_eq!(app.toast.as_deref(), Some(w.edit_changed));
    assert_eq!(std::fs::read_to_string(&readme).unwrap(), "他の誰か\n");
    let _ = app.update(Msg::FileEditCancel);
    // A picture is shown as one, not as a file that is not text.
    let picture = dir.join("p.png");
    std::fs::write(&picture, [0x89, b'P', b'N', b'G', 0, 0]).unwrap();
    let _ = app.update(Msg::FileRead(Box::new(crate::files::read(picture, None))));
    let v = app.viewing.as_ref().unwrap();
    assert!(v.image && v.error.is_none());
    assert!(!says(&app, w.edit_start));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn with_autosave_what_is_typed_is_written_a_moment_after_the_last_key_and_kept_open() {
    use std::time::{Duration, Instant};
    use text_editor::{Action, Edit, Motion};
    let mut app = window("ja");
    let dir = std::env::temp_dir().join(format!("sasacode-gui-autosave-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    app.cwd = dir.clone();
    let file = dir.join("notes.txt");
    std::fs::write(&file, "一行目\n").unwrap();
    let w = crate::words::JA;
    app.tab = Tab::Files;
    app.insp = true;
    let _ = app.update(Msg::SetAutosave(true));
    let _ = app.update(Msg::FileRead(Box::new(crate::files::read(file.clone(), None))));
    let _ = app.update(Msg::FileEditStart);
    assert!(says(&app, w.edit_autosaving) && says(&app, w.edit_close));
    let _ = app.update(Msg::FileEditAction(Action::Move(Motion::DocumentEnd)));
    for c in "二行目".chars() {
        let _ = app.update(Msg::FileEditAction(Action::Edit(Edit::Insert(c))));
    }
    // Just typed: not yet.
    let _ = app.update(Msg::FileAutosave);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "一行目\n");
    // A moment later: written, and the editor stays open with nothing left unsaved.
    app.file_edit.as_mut().unwrap().typed_at = Some(Instant::now() - Duration::from_secs(2));
    let _ = app.update(Msg::FileAutosave);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "一行目\n二行目\n");
    let edit = app.file_edit.as_ref().unwrap();
    assert!(!edit.changed() && edit.typed_at.is_none());
    // Another file opens over it: what was typed last is saved first.
    let _ = app.update(Msg::FileEditAction(Action::Edit(Edit::Insert('!'))));
    let _ = app.update(Msg::ViewFile(dir.join("other.txt"), None));
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "一行目\n二行目!\n");
    assert_ne!(app.toast.as_deref(), Some(w.edit_unsaved));
    // Written by another meanwhile: not written over, and told.
    let _ = app.update(Msg::FileEditAction(Action::Edit(Edit::Insert('?'))));
    std::fs::write(&file, "他の誰か\n").unwrap();
    app.file_edit.as_mut().unwrap().typed_at = Some(Instant::now() - Duration::from_secs(2));
    let _ = app.update(Msg::FileAutosave);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "他の誰か\n");
    assert_eq!(app.toast.as_deref(), Some(w.edit_changed));
    // Closed after saving by itself, the tab shows the file as it is now.
    std::fs::write(&file, "一行目\n").unwrap();
    let _ = app.update(Msg::FileRead(Box::new(crate::files::read(file.clone(), None))));
    let _ = app.update(Msg::FileEditStart);
    let _ = app.update(Msg::FileEditAction(Action::Move(Motion::DocumentEnd)));
    let _ = app.update(Msg::FileEditAction(Action::Edit(Edit::Insert('3'))));
    let _ = app.update(Msg::FileEditCancel);
    assert!(app.file_edit.is_none());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "一行目\n3\n");
    assert!(app.viewing.as_ref().unwrap().lines.iter().any(|l| l == "3"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_files_tab_finds_files_by_what_they_hold_and_opens_one_at_its_line() {
    let mut app = window("ja");
    app.tab = Tab::Files;
    app.insp = true;
    let w = crate::words::JA;
    let _ = app.update(Msg::FileGrepMode(true));
    assert!(says(&app, w.grep_ph));
    let _ = app.update(Msg::FileQuery("retry".into()));
    let _ = app.update(Msg::FileGrep);
    assert!(app.grep_busy && says(&app, w.grep_busy));
    let hits = vec![crate::edit::Hit {
        path: "src/retry.rs".into(),
        line: 42,
        text: "fn retry_with_backoff()".into(),
    }];
    // An answer for what was typed before is not shown.
    let _ = app.update(Msg::Grepped("old".into(), Ok(Vec::new())));
    assert!(app.grep_busy);
    let _ = app.update(Msg::Grepped("retry".into(), Ok(hits)));
    assert!(says(&app, "src/retry.rs:42") && says(&app, "fn retry_with_backoff()"));
    let _ = app.update(Msg::Grepped("retry".into(), Ok(Vec::new())));
    assert!(says(&app, w.grep_none));
}

#[test]
fn a_conversation_continued_elsewhere_is_read_again_when_its_log_grows() {
    let mut app = window("ja");
    let dir = real_logs(&mut app, "watch");
    let a = app.sessions[1].clone();
    assert!(app.open_session(&a.path));
    assert!(app.watches_log());
    let _ = app.update(Msg::WatchLog);
    assert_eq!(app.chat.transcript.exchanges.len(), 1);
    // The TUI sends another message in it.
    let more = serde_json::json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"TUIから続けた"}],"timestamp":2}});
    let mut log = std::fs::read_to_string(&a.path).unwrap();
    log.push_str(&format!("{more}\n"));
    std::fs::write(&a.path, log).unwrap();
    let _ = app.update(Msg::WatchLog);
    assert_eq!(app.chat.transcript.exchanges.len(), 2);
    assert!(says(&app, "TUIから続けた"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_usage_screen_shows_the_spans_the_graph_the_days_and_the_models() {
    let mut app = window("ja");
    let today = chrono::Local::now().date_naive();
    let used = crate::usage::Sum {
        tokens: 120_000,
        output: 8_000,
        requests: 12,
        cost: 0.42,
    };
    let mut report = crate::usage::Report {
        today,
        ..Default::default()
    };
    report.spans = [used; 4];
    report.days.insert(today, used);
    report.models.push(crate::usage::ModelUse {
        model: "local/gemma4".into(),
        sum: used,
        last: today,
    });
    report.folders.push(crate::usage::FolderUse {
        folder: "/srv/billing-web".into(),
        sum: used,
    });
    let _ = app.update(Msg::Usage(false));
    assert_eq!(app.overlay, Some(crate::app::Overlay::Usage));
    let w = crate::words::JA;
    assert!(says(&app, w.usage_reading));
    // An answer for the other scope is not shown.
    let _ = app.update(Msg::UsageRead(true, Box::new(report.clone())));
    assert!(app.usage.is_none());
    let _ = app.update(Msg::UsageRead(false, Box::new(report.clone())));
    assert!(says(&app, w.usage_today) && says(&app, w.usage_models) && says(&app, "local/gemma4"));
    assert!(says(&app, "120.0kトークン（出力8.0k）· 12回 · $0.420"));
    let streak = crate::i18n::fill(w.usage_streak, "current", 1).replace("{longest}", "1");
    assert!(says(&app, &streak));
    // `/usage graph` typed, as in the CLI: read again, then the graph alone.
    app.overlay = None;
    app.set_input("/usage graph");
    let _ = app.update(Msg::Send);
    assert_eq!(app.overlay, Some(crate::app::Overlay::Usage));
    assert_eq!(app.usage_part, crate::usage::Part::Graph);
    assert!(app.chat.editor.text().trim().is_empty(), "never sent to the agent");
    let _ = app.update(Msg::UsageRead(false, Box::new(report)));
    assert!(says(&app, &streak), "the graph");
    assert!(!says(&app, w.usage_models), "not the models");
    // Another part, from the screen itself.
    let _ = app.update(Msg::UsagePart(crate::usage::Part::Models));
    assert!(app.usage.is_some(), "not read again");
    assert!(says(&app, w.usage_models) && !says(&app, &streak));
    // Each folder: with all of them read, under all too.
    let _ = app.update(Msg::UsagePart(crate::usage::Part::Folders));
    assert!(says(&app, w.usage_folders) && says(&app, "billing-web") && !says(&app, w.usage_models));
    let _ = app.update(Msg::UsagePart(crate::usage::Part::All));
    assert!(says(&app, w.usage_folders));
    let _ = app.update(Msg::UsagePart(crate::usage::Part::Models));
    // The conversation's usage tab leads to it too.
    app.overlay = None;
    app.tab = Tab::Usage;
    app.insp = true;
    assert!(says(&app, w.usage_open));
    // A part chosen while this folder's is still being read: still this folder's.
    let _ = app.update(Msg::Usage(true));
    let _ = app.update(Msg::UsagePart(crate::usage::Part::History));
    assert!(app.usage_here && app.usage_part == crate::usage::Part::History);
}

#[test]
fn a_message_goes_to_several_models_each_in_a_conversation_of_its_own() {
    let mut app = window("ja");
    app.listing = listed();
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Compare)));
    let w = crate::words::JA;
    assert!(says(&app, w.compare_title) && says(&app, "local/gemma4") && says(&app, w.compare_empty));
    let _ = app.update(Msg::CompareToggle("local/gemma4".into()));
    let _ = app.update(Msg::CompareType("cloud/other".into()));
    let _ = app.update(Msg::CompareAdd);
    app.set_input("同じ質問");
    let _ = app.update(Msg::CompareSend);
    assert!(app.overlay.is_none() && app.chat.editor.text().trim().is_empty());
    let all: Vec<&crate::app::Chat> = std::iter::once(&app.chat)
        .chain(&app.parked)
        .filter(|c| c.model.is_some())
        .collect();
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|c| c.busy() && c.transcript.title() == Some("同じ質問")));
    // Named by its model once its run names it.
    let key = app.chat.key;
    let model = app.chat.model.clone().unwrap();
    let event =
        sasacode_protocol::parse_event(&serde_json::json!({"type":"session","id":"cmp-1"}).to_string()).unwrap();
    let _ = app.update(Msg::Run(key, crate::runner::RunUpdate::Event(Box::new(event))));
    let short = model.rsplit('/').next().unwrap();
    assert_eq!(
        app.prefs.names.get("cmp-1").map(String::as_str),
        Some(format!("同じ質問 · {short}").as_str())
    );
}

#[test]
fn worktrees_are_readied_by_the_folders_command_listed_and_offered_to_remove_but_not_the_windows_own() {
    let mut app = window("ja");
    let w = crate::words::JA;
    // The command that readies them, kept for this folder.
    let _ = app.update(Msg::Show(Some(crate::app::Overlay::Settings)));
    assert!(says(&app, w.set_setup));
    let _ = app.update(Msg::SetWorktreeSetup("npm install".into()));
    assert_eq!(app.worktree_setup().as_deref(), Some("npm install"));
    // Made, and its setup failed: it opens anyway, and says what failed.
    let made = std::path::PathBuf::from("/tmp/home/worktrees/orders-api-20261117-000000");
    let _ = app.update(Msg::WorktreeMade(Ok((made.clone(), Some("npm ERR! missing".into())))));
    let toast = app.toast.clone().unwrap();
    assert!(toast.contains("orders-api-20261117-000000") && toast.contains("npm ERR! missing"));
    // Listed: a new one, and each made here but the one this window is in.
    app.worktree_list = Some(vec![
        crate::worktrees::Tree {
            path: made.clone(),
            branch: Some("sasacode/20261117-000000".into()),
        },
        crate::worktrees::Tree {
            path: app.cwd.clone(),
            branch: Some("sasacode/here".into()),
        },
    ]);
    app.overlay = Some(crate::overlay::Overlay::picker(crate::overlay::Pick::Worktrees));
    let found: Vec<Msg> = app.found_entries().into_iter().map(|e| e.msg).collect();
    assert!(matches!(&found[..], [
        Msg::Command(Command::Worktree),
        Msg::WorktreeSweep,
        Msg::WorktreeRemove(p),
    ] if *p == made));
    // One not made here is never removed.
    let _ = app.update(Msg::WorktreeRemove("/Users/me/elsewhere".into()));
    app.toast = None;
    let _ = app.update(Msg::WorktreeRemoved(
        made,
        Err("contains modified or untracked files".into()),
    ));
    assert!(app.toast.as_deref().is_some_and(|t| t.contains("modified")));
}

/// Plan mode: the run is asked under `ask` and told to plan; a write it asks for is denied by the
/// window, a command is put to the user, and the plan, once written, is gone ahead with in the
/// mode chosen before.
#[test]
fn plan_mode_denies_writes_asks_commands_and_goes_ahead_in_the_mode_chosen_before() {
    let mut app = window("ja");
    app.permission = Some("agent");
    let _ = app.update(Msg::Permission("plan"));
    assert_eq!((app.permission, app.before_plan), (Some("plan"), Some("agent")));
    let key = app.chat.key;
    let _ = app.start(key, "注文一覧のページングを直す計画を立てて".into());
    let (tx, mut written) = tokio::sync::mpsc::unbounded_channel();
    let _ = app.update(Msg::Run(key, crate::runner::RunUpdate::Started(tx)));
    let prompt = app.home.as_ref().unwrap().join("gui").join("plan-mode.md");
    assert!(std::fs::read_to_string(&prompt).unwrap().contains("# Plan mode"));

    told(
        &mut app,
        serde_json::json!({"type": "approval_request", "id": "ap-1", "callId": "c-1", "tool": "edit",
            "args": {"path": "src/orders.rs"}, "reason": "", "cwd": "/Users/me/src/orders-api"}),
    );
    let denied = "プランモードでは書き込みと編集をしません。計画を書いてください。";
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            id: "ap-1".into(),
            decision: Decision::Deny,
            feedback: Some(denied.into()),
            args: None,
        })
    );
    assert_eq!(answer_of(&app, "ap-1"), Some(Answer::Denied(denied.into())));
    assert!(app.chat.transcript.pending_approval().is_none());

    // A command may only look: the user is asked.
    told(
        &mut app,
        serde_json::json!({"type": "approval_request", "id": "ap-2", "callId": "c-2", "tool": "bash",
            "args": {"command": "rg -n page src"}, "reason": "", "cwd": "/Users/me/src/orders-api"}),
    );
    assert!(written.try_recv().is_err());
    assert_eq!(
        app.chat.transcript.pending_approval().map(|a| a.id.as_str()),
        Some("ap-2")
    );
    let _ = app.update(Msg::Answer(Decision::Allow));
    let _ = written.try_recv();

    told(&mut app, serde_json::json!({"type": "agent_end", "cause": "done"}));
    let _ = app.update(Msg::Run(key, crate::runner::RunUpdate::Exited(Some(0))));
    assert!(!app.busy());
    assert!(says(&app, "計画ができました。読んで、よければ進めます"));
    assert!(says(&app, "この計画で進める"));

    let _ = app.update(Msg::PlanGo);
    assert_eq!(app.permission, Some("agent"));
    assert!(!app.chat.planned && app.running());
    assert!(!says(&app, "この計画で進める"));
    std::fs::remove_file(prompt).ok();
}

/// Plan mode is the window's: a folder opened again starts in the mode before it, as for auto.
#[test]
fn plan_mode_is_not_taken_up_again_when_the_folder_is_opened() {
    let mut app = window("ja");
    let _ = app.update(Msg::Permission("plan"));
    let mut prefs = app.prefs.clone();
    prefs.more.folders.entry(app.folder_key()).or_default().permission = Some("plan".into());
    app.prefs = prefs;
    app.restore_folder(true);
    assert_eq!(app.permission, None);
}

/// Automations: written in their screen, kept for the folder, and run when due in a conversation
/// of their own beside the one shown, in their own mode. What the user typed for a new
/// conversation is left as it is, and a run once started is not started again.
#[test]
fn an_automation_runs_when_due_in_a_conversation_of_its_own_and_in_its_own_mode() {
    use crate::automations::{AutoMsg, Every, Kind as Sched};
    let auto = |m: AutoMsg| Msg::Auto(m);
    let mut app = window("ja");
    let _ = app.update(Msg::Command(Command::Automations));
    assert!(matches!(app.overlay, Some(crate::overlay::Overlay::Automations)));
    assert!(says(&app, "まだありません") && says(&app, "新しい自動実行"));
    let _ = app.update(auto(AutoMsg::Save));
    assert!(says(&app, "名前とメッセージを書いてください"));

    let _ = app.update(auto(AutoMsg::Name("朝のCI確認".into())));
    app.auto_form.prompt = text_editor::Content::with_text("昨夜のCIの失敗をまとめて");
    let _ = app.update(auto(AutoMsg::Kind(Sched::Weekdays)));
    let _ = app.update(auto(AutoMsg::At("25:00".into())));
    let _ = app.update(auto(AutoMsg::Save));
    assert!(says(&app, "時刻は09:00のように書いてください"));
    let _ = app.update(auto(AutoMsg::At("08:30".into())));
    // auto is no mode of an automation.
    let _ = app.update(auto(AutoMsg::Mode("auto")));
    assert_eq!(app.auto_form.mode, "plan");
    let _ = app.update(auto(AutoMsg::Mode("ask")));
    let _ = app.update(auto(AutoMsg::Save));
    let here = app.automations_here();
    assert_eq!(here.len(), 1);
    let (id, made) = (here[0].0.clone(), here[0].1.clone());
    assert_eq!(made.every, Every::Weekdays { at: 8 * 60 + 30 });
    assert_eq!((made.mode.as_str(), made.on), ("ask", true));
    assert!(says(&app, "朝のCI確認") && says(&app, "平日08:30"));
    assert!(app.watches_automations());

    // Made a week ago, it is due now.
    let now = chrono::Local::now().timestamp();
    app.prefs.more.automations.get_mut(&id).unwrap().since = now - 7 * 86_400;
    let folder = app.folder_key();
    app.prefs.more.drafts.insert(folder.clone(), "書きかけ".into());
    let (shown, parked) = (app.chat.key, app.parked.len());
    let _ = app.look_at_automations(now);
    assert_eq!(app.parked.len(), parked + 1);
    let chat = app.parked.last().unwrap();
    assert_eq!(
        (chat.mode, chat.automation.as_deref()),
        (None, Some(id.as_str())),
        "its mode is for its own run"
    );
    assert!(chat.running());
    assert_eq!(app.chat.key, shown, "the conversation shown stays");
    assert_eq!(app.prefs.more.automations[&id].last, Some(now));
    assert_eq!(app.prefs.more.drafts.get(&folder).map(String::as_str), Some("書きかけ"));
    assert_eq!(app.toast.as_deref(), Some("自動実行「朝のCI確認」を始めました"));
    // Looked at again, it has run.
    let _ = app.look_at_automations(now + 60);
    assert_eq!(app.parked.len(), parked + 1);

    // Changed: kept as one, counted from now.
    let _ = app.update(auto(AutoMsg::Edit(id.clone())));
    assert!(says(&app, "編集：朝のCI確認"));
    assert_eq!(app.auto_form.at, "08:30");
    let _ = app.update(auto(AutoMsg::Kind(Sched::Hours)));
    let _ = app.update(auto(AutoMsg::Hours("6".into())));
    let _ = app.update(auto(AutoMsg::Save));
    assert_eq!(app.automations_here().len(), 1);
    assert_eq!(app.prefs.more.automations[&id].every, Every::Hours { hours: 6 });
    assert!(says(&app, "6時間ごと"));

    let _ = app.update(auto(AutoMsg::Toggle(id.clone())));
    assert!(!app.watches_automations() && says(&app, "止めています"));
    let _ = app.update(auto(AutoMsg::Delete(id)));
    assert!(app.automations_here().is_empty());
}

/// Two windows of a folder (⌘⇧N opens one): an automation stopped or deleted in one is neither
/// run by the other, which still had it as it was, nor written back by it.
#[test]
fn an_automation_stopped_or_deleted_in_another_window_is_not_run_or_written_back() {
    use crate::automations::{AutoMsg, Automation, Every};
    let dir = std::env::temp_dir().join(format!("sasacode-gui-two-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(crate::layout::FILE);
    let (mut one, mut two) = (window("ja"), window("ja"));
    one.layout_file = Some(file.clone());
    two.layout_file = Some(file.clone());
    let now = chrono::Local::now().timestamp();
    let made = Automation {
        name: "依存の確認".into(),
        folder: one.folder_key(),
        prompt: "古くなった依存を調べて".into(),
        every: Every::Hours { hours: 1 },
        since: now - 7200,
        ..Automation::default()
    };
    let _ = one.set(|p| {
        p.more.automations.insert("a1".into(), made.clone());
        p.more.automations.insert("a2".into(), made);
    });
    two.read_others();
    assert!(two.prefs.more.automations["a1"].on && two.prefs.more.automations["a2"].on);
    let parked = two.parked.len();

    // Stopped in one and deleted in one: the other, which had both on and due, runs neither.
    let _ = one.update(Msg::Auto(AutoMsg::Toggle("a1".into())));
    let _ = one.update(Msg::Auto(AutoMsg::Delete("a2".into())));
    assert!(two.automations_here().iter().all(|(_, a)| a.on && a.due(now)));
    let _ = two.look_at_automations(now);
    assert_eq!(two.parked.len(), parked);
    // Nor does it write back what it had, when it writes again.
    let _ = two.set(|p| p.notify = false);
    let held = crate::layout::Prefs::read(&file).more.automations;
    assert_eq!(held.keys().collect::<Vec<_>>(), ["a1"]);
    assert!(!held["a1"].on);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An automation in plan mode runs as a plan run does: asked, with what writes denied.
#[test]
fn an_automation_in_plan_mode_has_its_writes_denied() {
    use crate::automations::{AutoMsg, Automation, Every};
    let mut app = window("ja");
    let now = chrono::Local::now().timestamp();
    app.prefs.more.automations.insert(
        "a1".into(),
        Automation {
            name: "依存の確認".into(),
            folder: app.folder_key(),
            prompt: "古くなった依存を調べて".into(),
            every: Every::Hours { hours: 24 },
            since: now - 2 * 86_400,
            ..Automation::default()
        },
    );
    let _ = app.update(Msg::Auto(AutoMsg::RunNow("a1".into())));
    let key = app.parked.last().unwrap().key;
    let (tx, mut written) = tokio::sync::mpsc::unbounded_channel();
    let _ = app.update(Msg::Run(key, crate::runner::RunUpdate::Started(tx)));
    let event = sasacode_protocol::parse_event(
        &serde_json::json!({"type": "approval_request", "id": "ap-1", "callId": "c-1", "tool": "write",
            "args": {"path": "Cargo.toml"}, "reason": "", "cwd": "/Users/me/src/orders-api"})
        .to_string(),
    )
    .unwrap();
    let _ = app.update(Msg::Run(key, crate::runner::RunUpdate::Event(Box::new(event))));
    assert!(matches!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            decision: Decision::Deny,
            ..
        })
    ));
    // The window's own mode is not changed by it.
    assert_eq!(app.permission, None);
    assert!(app.parked.last().unwrap().plan_run());
    // What the user sends after in that conversation goes in the mode the window shows.
    let _ = app.start_run(key, "では直して".into(), false);
    assert!(!app.parked.iter().find(|c| c.key == key).unwrap().plan_run());
}

/// Sharing as a secret gist is asked each time, saying where it goes and that its URL is enough
/// to read it; made, its URL is said and copied. Nothing said yet, there is nothing to share.
#[test]
fn a_secret_gist_is_asked_for_each_time_and_its_url_is_said() {
    let mut app = window("ja");
    let _ = app.update(Msg::Command(Command::Gist));
    assert_eq!(app.overlay, None);
    assert_eq!(app.toast.as_deref(), Some("共有する会話がありません"));

    markdown_reply(&mut app);
    let _ = app.update(Msg::Command(Command::Gist));
    assert_eq!(app.overlay, Some(crate::overlay::Overlay::ConfirmGist));
    assert!(says(&app, "secret gistで共有") && says(&app, "secret gistを作る"));
    let gist = app.gist().unwrap();
    assert_eq!(gist.file, "sasacode-0a1b2c3d.md");
    assert!(gist.markdown.contains("指数バックオフ"));
    // Put away, nothing is made.
    let _ = app.update(Msg::Show(None));
    assert!(!app.gisting);

    let _ = app.update(Msg::GistAsk);
    let _ = app.update(Msg::GistCreate);
    assert_eq!(app.overlay, None);
    assert!(app.gisting);
    assert_eq!(app.toast.as_deref(), Some("gistを作っています…"));
    let _ = app.update(Msg::GistMade(Ok("https://gist.github.com/someone/0123abcd".into())));
    assert!(!app.gisting);
    assert_eq!(
        app.toast.as_deref(),
        Some("secret gistを作り、URLをコピーしました：https://gist.github.com/someone/0123abcd")
    );
    let _ = app.update(Msg::GistMade(Err("gh auth login".into())));
    assert_eq!(app.toast.as_deref(), Some("gistを作れませんでした：gh auth login"));
}

#[test]
fn what_a_review_asked_for_finds_is_shown_on_the_lines_of_the_diff_until_put_away() {
    let mut app = window("ja");
    let w = app.s.w();
    let dir = std::env::temp_dir().join(format!("sasacode-gui-review-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "a\nb\nc\nd\ne\nf\ng\nh\nd\n").unwrap();
    app.cwd = dir.clone();
    changed_repo(&mut app);
    // The review asked for, sent, and answered.
    let _ = app.update(Msg::AskReview);
    let _ = app.update(Msg::Send);
    let _ = app.pretend_running();
    let key = app.chat.key;
    let reply =
        "1. a.txt:9 で`d`に変えた理由が分かりません。\n2. a.txt:3 の行は差分の外です。\n3. 全体にテストが足りません。";
    for line in [
        serde_json::json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":reply}],"provider":"fake","model":"m","usage":{"input":10,"output":10},"stopReason":"stop"}}),
        serde_json::json!({"type":"agent_end","cause":"done"}),
    ] {
        let event = sasacode_protocol::parse_event(&line.to_string()).unwrap();
        let _ = app.update(Msg::Run(key, crate::runner::RunUpdate::Event(Box::new(event))));
    }
    assert_eq!(app.chat.review.len(), 2);
    // On its line in the diff, and the one off the diff above it with where it is.
    assert!(says(&app, &crate::i18n::fill(w.review_found, "n", 2)));
    assert!(says(&app, "a.txt:9 で`d`に変えた理由が分かりません。"));
    assert!(says(&app, "@a.txt:3") && says(&app, "a.txt:3 の行は差分の外です。"));
    // One goes with the next message, as a comment on its line; the other is put away.
    let _ = app.update(Msg::ReviewTake(0));
    assert_eq!(app.chat.comments.len(), 1);
    assert_eq!(app.chat.comments[0].line, 9);
    let _ = app.update(Msg::ReviewDrop(0));
    assert!(app.chat.review.is_empty() && !says(&app, &crate::i18n::fill(w.review_found, "n", 1)));
    // All put away at once.
    app.chat.review = crate::review::findings("a.txt:2 です", &app.cwd);
    let _ = app.update(Msg::ReviewClear);
    assert!(app.chat.review.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
