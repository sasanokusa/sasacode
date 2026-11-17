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
        default_font: crate::view::UI,
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
                feedback: None
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
            feedback: Some("それは不要".into())
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
            feedback: Some("先にテストを書いて".into())
        })
    );
}

/// Whether the window as it is drawn has `label` in it.
fn says(app: &App, label: &str) -> bool {
    let settings = Settings {
        default_font: crate::view::UI,
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
        default_font: crate::view::UI,
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
    assert_eq!(written.try_recv().ok(), Some(ControlMessage::Abort));
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
            default_font: crate::view::UI,
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
    assert_eq!(written.try_recv().ok(), Some(ControlMessage::Abort));
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
            default_font: crate::view::UI,
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
            default_font: crate::view::UI,
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
