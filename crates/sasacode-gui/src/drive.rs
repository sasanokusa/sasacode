//! Uses the window without a display and without a process: clicks and keys go in the way a
//! user's do, and what the window would write to its run comes out of a channel.

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::text_editor;
use iced::{Settings, Size};
use iced_test::Simulator;
use sasacode_protocol::{ControlMessage, Decision, Event};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{App, INPUT, Msg, Tab, shortcut};
use crate::snapshots::{play, window};
use crate::transcript::Answer;

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
    let id = app.transcript.pending_approval().expect("an approval waits").id.clone();
    (app, written, id)
}

fn answer_of(app: &App, id: &str) -> Option<Answer> {
    app.transcript
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
        assert!(app.transcript.pending_approval().is_none());
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
    assert!(app.editor.text().trim().is_empty());
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
    app.editor = text_editor::Content::with_text("先にテストを書いて");
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
    assert_eq!(app.transcript.queued, ["やっぱり10秒でいい"]);
    // Still the same exchange: it becomes one of its own when the model is given it.
    assert_eq!(app.transcript.exchanges.len(), 1);

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
