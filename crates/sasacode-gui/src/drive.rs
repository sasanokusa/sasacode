//! Uses the window without a display and without a process: clicks and keys go in the way a
//! user's do, and what the window would write to its run comes out of a channel.

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::text_editor;
use iced::{Settings, Size};
use iced_test::Simulator;
use sasacode_protocol::{ControlMessage, Decision, Event};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{App, Clip, Edge, INPUT, INSP_W, Msg, SEARCH, SIDE_W, Tab, dragging, shortcut};
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
        let _ = app.update(Msg::Chord);
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
    let _ = app.update(Msg::Chord);
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
    let _ = app.update(Msg::Chord);
    assert_eq!(
        written.try_recv().ok(),
        Some(ControlMessage::Approval {
            id,
            decision: Decision::Deny,
            feedback: Some("先にテストを書いて".into())
        })
    );
}

#[test]
fn no_bare_key_is_a_shortcut() {
    let none = Modifiers::empty();
    assert!(matches!(
        shortcut(&Key::Named(Named::Enter), Modifiers::COMMAND),
        Some(Msg::Chord)
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

/// ⌘ and `key`, pressed and let go, as macOS sends them: the key comes with its letter as text.
fn command_chord(letter: &str) -> Vec<iced::Event> {
    use iced::keyboard::{Event as K, Location, key::NativeCode, key::Physical};
    let key = Key::Character(letter.into());
    let physical = Physical::Unidentified(NativeCode::Unidentified);
    vec![
        iced::Event::Keyboard(K::ModifiersChanged(Modifiers::COMMAND)),
        iced::Event::Keyboard(K::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: physical,
            location: Location::Standard,
            modifiers: Modifiers::COMMAND,
            text: Some(letter.into()),
            repeat: false,
        }),
        iced::Event::Keyboard(K::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key: physical,
            location: Location::Standard,
            modifiers: Modifiers::COMMAND,
        }),
        iced::Event::Keyboard(K::ModifiersChanged(Modifiers::empty())),
    ]
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
            diff: crate::diff::Diff::parse("@@ -1 +1 @@\n-a\n+b\n"),
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
