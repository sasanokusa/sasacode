//! The window's shortcuts, and the keys the user gives them instead: each is ⌘ (Ctrl+ elsewhere)
//! with a key, so no bare key typed for the input ever does one, and none takes a key that editing
//! text or the system needs (copy, paste, undo, quit…). What the user chose is kept in the
//! settings as `cmd+shift+k`, by the shortcut's name.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use iced::keyboard::{Key, Modifiers};

use crate::app::{Msg, Overlay, Tab};
use crate::i18n::Strings;
use crate::input::Command;
use crate::view::MOD;

/// What a shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Act {
    New,
    Window,
    Side,
    Inspector,
    Prev,
    Next,
    Attention,
    Scripts,
    Outline,
    Split,
    Palette,
    Mention,
    FindIn,
    Settings,
    Keys,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Find,
    FocusInput,
    Tab(u8),
    Back,
    Forward,
    Rename,
    Pin,
    Ask,
    Model,
    Effort,
    Permission,
    OpenFolder,
}

/// Every shortcut, in the order the list of keys shows them.
pub const ACTS: [Act; 35] = [
    Act::New,
    Act::Window,
    Act::OpenFolder,
    Act::Back,
    Act::Forward,
    Act::Rename,
    Act::Pin,
    Act::Ask,
    Act::Model,
    Act::Effort,
    Act::Permission,
    Act::Find,
    Act::FindIn,
    Act::Mention,
    Act::Palette,
    Act::Prev,
    Act::Next,
    Act::Attention,
    Act::Scripts,
    Act::Outline,
    Act::Split,
    Act::FocusInput,
    Act::Side,
    Act::Inspector,
    Act::Tab(1),
    Act::Tab(2),
    Act::Tab(3),
    Act::Tab(4),
    Act::Tab(5),
    Act::Tab(6),
    Act::Settings,
    Act::Keys,
    Act::ZoomIn,
    Act::ZoomOut,
    Act::ZoomReset,
];

/// Keys no shortcut takes: editing text, and the system's own.
const RESERVED: [&str; 9] = ["a", "c", "v", "x", "z", "q", "w", "h", "m"];

/// A shortcut's key: ⌘ with it, and Shift or Option if they are held.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub shift: bool,
    pub alt: bool,
    /// The key, lower case, or a named key such as `enter`.
    pub key: String,
}

impl Chord {
    fn new(shift: bool, key: &str) -> Chord {
        Chord {
            shift,
            alt: false,
            key: key.to_owned(),
        }
    }

    /// The chord a key press is, if ⌘ is held.
    pub fn of(key: &Key, modifiers: Modifiers) -> Option<Chord> {
        if !modifiers.command() {
            return None;
        }
        let key = match key.as_ref() {
            Key::Character(c) => c.to_lowercase(),
            Key::Named(named) => format!("{named:?}").to_lowercase(),
            _ => return None,
        };
        // Shift held makes some keys others: = is + there.
        let key = if key == "+" { "=".to_owned() } else { key };
        Some(Chord {
            shift: modifiers.shift(),
            alt: modifiers.alt(),
            key,
        })
    }

    /// As the settings keep it: `cmd+shift+k`.
    pub fn parse(s: &str) -> Option<Chord> {
        let mut chord = Chord::new(false, "");
        let mut cmd = false;
        for part in s.split('+').map(str::trim) {
            match part.to_lowercase().as_str() {
                "cmd" | "ctrl" | "command" => cmd = true,
                "shift" => chord.shift = true,
                "alt" | "option" | "opt" => chord.alt = true,
                "" => {}
                key => chord.key = key.to_owned(),
            }
        }
        (cmd && !chord.key.is_empty()).then_some(chord)
    }

    pub fn store(&self) -> String {
        let mut s = "cmd+".to_owned();
        if self.shift {
            s.push_str("shift+");
        }
        if self.alt {
            s.push_str("alt+");
        }
        s.push_str(&self.key);
        s
    }

    /// As it is shown: `⌘⇧K`.
    pub fn show(&self) -> String {
        let mut s = MOD.to_owned();
        if self.shift {
            s.push('⇧');
        }
        if self.alt {
            s.push('⌥');
        }
        s.push_str(&self.key.to_uppercase());
        s
    }
}

impl Act {
    /// The name the settings keep it by.
    pub fn name(self) -> String {
        match self {
            Act::New => "new".into(),
            Act::Window => "window".into(),
            Act::Side => "sidebar".into(),
            Act::Inspector => "inspector".into(),
            Act::Prev => "previous".into(),
            Act::Next => "next".into(),
            Act::Attention => "attention".into(),
            Act::Scripts => "scripts".into(),
            Act::Outline => "outline".into(),
            Act::Split => "split".into(),
            Act::Palette => "palette".into(),
            Act::Mention => "mention".into(),
            Act::FindIn => "find-in-conversation".into(),
            Act::Settings => "settings".into(),
            Act::Keys => "keys".into(),
            Act::ZoomIn => "zoom-in".into(),
            Act::ZoomOut => "zoom-out".into(),
            Act::ZoomReset => "zoom-reset".into(),
            Act::Find => "find".into(),
            Act::FocusInput => "focus-input".into(),
            Act::Tab(n) => format!("tab-{n}"),
            Act::Back => "back".into(),
            Act::Forward => "forward".into(),
            Act::Rename => "rename".into(),
            Act::Pin => "pin".into(),
            Act::Ask => "side-question".into(),
            Act::Model => "model".into(),
            Act::Effort => "effort".into(),
            Act::Permission => "permission".into(),
            Act::OpenFolder => "open-folder".into(),
        }
    }

    pub fn default_chord(self) -> Chord {
        match self {
            Act::New => Chord::new(false, "n"),
            Act::Window => Chord::new(true, "n"),
            Act::Side => Chord::new(false, "b"),
            Act::Inspector => Chord::new(false, "i"),
            Act::Prev => Chord::new(true, "["),
            Act::Next => Chord::new(true, "]"),
            Act::Attention => Chord::new(true, "a"),
            Act::Scripts => Chord::new(true, "r"),
            Act::Outline => Chord::new(true, "o"),
            Act::Split => Chord::new(false, "d"),
            Act::Palette => Chord::new(true, "p"),
            Act::Mention => Chord::new(false, "p"),
            Act::FindIn => Chord::new(false, "f"),
            Act::Settings => Chord::new(false, ","),
            Act::Keys => Chord::new(false, "/"),
            Act::ZoomIn => Chord::new(false, "="),
            Act::ZoomOut => Chord::new(false, "-"),
            Act::ZoomReset => Chord::new(false, "0"),
            Act::Find => Chord::new(false, "k"),
            Act::FocusInput => Chord::new(false, "l"),
            Act::Tab(n) => Chord::new(false, &n.to_string()),
            Act::Back => Chord::new(false, "["),
            Act::Forward => Chord::new(false, "]"),
            Act::Rename => Chord::new(true, "e"),
            Act::Pin => Chord::new(true, "u"),
            Act::Ask => Chord::new(true, "b"),
            Act::Model => Chord::new(true, "m"),
            Act::Effort => Chord::new(true, "t"),
            Act::Permission => Chord::new(true, "l"),
            Act::OpenFolder => Chord::new(false, "o"),
        }
    }

    pub fn msg(self) -> Msg {
        match self {
            Act::New => Msg::NewSession,
            Act::Window => Msg::Command(Command::Window),
            Act::Side => Msg::ToggleSide,
            Act::Inspector => Msg::ToggleInspector,
            Act::Prev => Msg::Neighbour(-1),
            Act::Next => Msg::Neighbour(1),
            Act::Attention => Msg::Command(Command::Attention),
            Act::Scripts => Msg::Command(Command::Scripts),
            Act::Outline => Msg::Command(Command::Outline),
            Act::Split => Msg::Command(Command::Split),
            Act::Palette => Msg::Show(Some(Overlay::palette())),
            Act::Mention => Msg::Command(Command::Mention),
            Act::FindIn => Msg::Command(Command::Find),
            Act::Settings => Msg::Show(Some(Overlay::Settings)),
            Act::Keys => Msg::Show(Some(Overlay::Keys)),
            Act::ZoomIn => Msg::Zoom(1),
            Act::ZoomOut => Msg::Zoom(-1),
            Act::ZoomReset => Msg::Zoom(0),
            Act::Find => Msg::Find,
            Act::FocusInput => Msg::FocusInput,
            Act::Tab(n) => Msg::Tab(match n {
                1 => Tab::Changes,
                2 => Tab::Plan,
                3 => Tab::Commands,
                4 => Tab::Usage,
                5 => Tab::Git,
                _ => Tab::Files,
            }),
            Act::Back => Msg::Back,
            Act::Forward => Msg::Forward,
            Act::Rename => Msg::Command(Command::Rename),
            Act::Pin => Msg::Command(Command::Pin),
            Act::Ask => Msg::SideOpen,
            Act::Model => Msg::FocusModel,
            Act::Effort => Msg::CycleEffort,
            Act::Permission => Msg::CyclePermission,
            Act::OpenFolder => Msg::Command(Command::OpenFolder),
        }
    }

    /// What it is called in the list of keys.
    pub fn label(self, s: &Strings) -> &'static str {
        match self {
            Act::New => s.new_session,
            Act::Window => s.c_window,
            Act::Side => s.a_side,
            Act::Inspector => s.a_insp,
            Act::Prev => s.c_prev,
            Act::Next => s.c_next,
            Act::Attention => s.c_attention,
            Act::Scripts => s.w().c_scripts,
            Act::Outline => s.w().c_outline,
            Act::Split => s.w().c_split,
            Act::Palette => s.c_palette,
            Act::Mention => s.c_mention,
            Act::FindIn => s.c_find,
            Act::Settings => s.settings,
            Act::Keys => s.c_keys,
            Act::ZoomIn => s.c_zoom_in,
            Act::ZoomOut => s.c_zoom_out,
            Act::ZoomReset => s.c_zoom_reset,
            Act::Find => s.find,
            Act::FocusInput => s.k_input,
            Act::Tab(1) => s.tab_changes,
            Act::Tab(2) => s.tab_plan,
            Act::Tab(3) => s.tab_cmd,
            Act::Tab(4) => s.tab_usage,
            Act::Tab(5) => s.tab_git,
            Act::Tab(_) => s.w().tab_files,
            Act::Back => s.w().k_back,
            Act::Forward => s.w().k_forward,
            Act::Rename => s.rename,
            Act::Pin => s.w().k_pin,
            Act::Ask => s.w().k_ask,
            Act::Model => s.w().k_model,
            Act::Effort => s.w().k_effort,
            Act::Permission => s.w().k_permission,
            Act::OpenFolder => s.w().c_open_folder,
        }
    }
}

/// The keys as bound now: each shortcut's chord, or none for one the user unbound.
#[derive(Debug, Clone, Default)]
pub struct Table(Vec<(Act, Option<Chord>)>);

impl Table {
    /// The defaults, with what the settings say over them. A chord that is not one, is reserved,
    /// or is another's is left as the default: two shortcuts never share a key.
    pub fn from(keys: &BTreeMap<String, String>) -> Table {
        let mut table: Vec<(Act, Option<Chord>)> = ACTS.iter().map(|a| (*a, Some(a.default_chord()))).collect();
        for (name, chord) in keys {
            let Some(at) = ACTS.iter().position(|a| a.name() == *name) else {
                continue;
            };
            if chord.trim().is_empty() {
                table[at].1 = None;
                continue;
            }
            let Some(chord) = Chord::parse(chord) else {
                continue;
            };
            if reserved(&chord) {
                continue;
            }
            table[at].1 = Some(chord);
        }
        // Where two came to share a key, the one the user set keeps it, and the other loses it.
        for i in 0..table.len() {
            for j in 0..table.len() {
                if i != j && table[i].1.is_some() && table[i].1 == table[j].1 {
                    let user_set_i = keys.contains_key(&table[i].0.name());
                    let loser = if user_set_i { j } else { i };
                    table[loser].1 = None;
                }
            }
        }
        Table(table)
    }

    pub fn act(&self, chord: &Chord) -> Option<Act> {
        self.0.iter().find(|(_, c)| c.as_ref() == Some(chord)).map(|(a, _)| *a)
    }

    pub fn chord(&self, act: Act) -> Option<&Chord> {
        self.0.iter().find(|(a, _)| *a == act).and_then(|(_, c)| c.as_ref())
    }
}

/// Why a chord cannot be given to a shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clash {
    /// Editing text or the system needs it.
    Reserved,
    /// Another shortcut has it.
    Taken(Act),
}

/// Whether `chord` can be given to `act`, with the keys bound as `table` has them.
pub fn check(table: &Table, act: Act, chord: &Chord) -> Result<(), Clash> {
    if reserved(chord) {
        return Err(Clash::Reserved);
    }
    match table.act(chord) {
        Some(other) if other != act => Err(Clash::Taken(other)),
        _ => Ok(()),
    }
}

fn reserved(chord: &Chord) -> bool {
    (!chord.shift && !chord.alt && RESERVED.contains(&chord.key.as_str()))
        || (chord.key == "z")
        // ⌘G and ⌘⇧G step through what the search in the conversation finds.
        || (chord.key == "g" && !chord.alt)
        || chord.key == "enter"
        || chord.key == "escape"
        || chord.key == "tab"
}

impl crate::app::App {
    /// The list of keys' messages; any other comes back.
    pub(crate) fn update_keys(&mut self, msg: Msg) -> Result<iced::Task<Msg>, Msg> {
        let w = self.s.w();
        match msg {
            Msg::KeyCapture(act) => {
                self.key_capture = Some(act);
                self.key_note = Some(w.key_press.to_owned());
            }
            Msg::KeyCaptured(key, modifiers) => {
                let Some(act) = self.key_capture else {
                    return Ok(iced::Task::none());
                };
                if matches!(key, Key::Named(iced::keyboard::key::Named::Escape)) {
                    self.key_capture = None;
                    self.key_note = None;
                    return Ok(iced::Task::none());
                }
                let Some(chord) = Chord::of(&key, modifiers) else {
                    self.key_note = Some(crate::i18n::fill(w.key_needs_mod, "mod", MOD.trim_end_matches('+')));
                    return Ok(iced::Task::none());
                };
                match check(&bound(), act, &chord) {
                    Err(Clash::Reserved) => {
                        self.key_note = Some(crate::i18n::fill(w.key_reserved, "key", chord.show()));
                    }
                    Err(Clash::Taken(other)) => {
                        self.key_note = Some(crate::i18n::fill(
                            &crate::i18n::fill(w.key_taken, "key", chord.show()),
                            "other",
                            other.label(&self.s),
                        ));
                    }
                    Ok(()) => {
                        self.key_capture = None;
                        self.key_note = None;
                        let name = act.name();
                        let _ = self.set(|p| {
                            if chord == act.default_chord() {
                                p.keys.remove(&name);
                            } else {
                                p.keys.insert(name, chord.store());
                            }
                        });
                    }
                }
            }
            Msg::KeyReset(act) => {
                self.key_capture = None;
                self.key_note = None;
                let _ = self.set(|p| match act {
                    Some(act) => {
                        p.keys.remove(&act.name());
                    }
                    None => p.keys.clear(),
                });
            }
            other => return self.update_outline(other),
        }
        Ok(iced::Task::none())
    }
}

// Where the keys are looked up when a key is pressed. The window's listeners run where its
// runtime puts them, so the table is the process's; a test's window is its thread's.
#[cfg(not(test))]
static BOUND: RwLock<Option<Arc<Table>>> = RwLock::new(None);

#[cfg(not(test))]
pub fn bind(table: Table) {
    if let Ok(mut b) = BOUND.write() {
        *b = Some(Arc::new(table));
    }
}

#[cfg(not(test))]
pub fn bound() -> Arc<Table> {
    BOUND
        .read()
        .ok()
        .and_then(|b| b.clone())
        .unwrap_or_else(|| Arc::new(Table::from(&BTreeMap::new())))
}

#[cfg(test)]
thread_local! {
    static BOUND: RwLock<Option<Arc<Table>>> = const { RwLock::new(None) };
}

#[cfg(test)]
pub fn bind(table: Table) {
    BOUND.with(|b| {
        if let Ok(mut b) = b.write() {
            *b = Some(Arc::new(table));
        }
    });
}

#[cfg(test)]
pub fn bound() -> Arc<Table> {
    BOUND.with(|b| {
        b.read()
            .ok()
            .and_then(|b| b.clone())
            .unwrap_or_else(|| Arc::new(Table::from(&BTreeMap::new())))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_is_kept_and_shown_as_it_was_given() {
        let c = Chord::parse("cmd+shift+k").unwrap();
        assert_eq!((c.shift, c.alt, c.key.as_str()), (true, false, "k"));
        assert_eq!(c.store(), "cmd+shift+k");
        assert!(c.show().ends_with("⇧K"));
        assert_eq!(Chord::parse("shift+k"), None, "no bare key");
        assert_eq!(
            Chord::of(&Key::Character("+".into()), Modifiers::COMMAND | Modifiers::SHIFT),
            Some(Chord::new(true, "="))
        );
    }

    #[test]
    fn every_default_is_its_own_and_none_is_reserved() {
        let table = Table::from(&BTreeMap::new());
        for act in ACTS {
            let chord = table.chord(act).expect("bound");
            assert_eq!(table.act(chord), Some(act), "{act:?}");
            assert!(!reserved(chord), "{act:?}");
            assert_eq!(ACTS.iter().filter(|a| a.name() == act.name()).count(), 1);
        }
    }

    #[test]
    fn a_key_given_twice_or_reserved_is_refused_and_the_users_one_wins() {
        let table = Table::from(&BTreeMap::new());
        let k = Chord::new(false, "k");
        assert_eq!(check(&table, Act::Palette, &k), Err(Clash::Taken(Act::Find)));
        assert_eq!(
            check(&table, Act::Palette, &Chord::new(false, "c")),
            Err(Clash::Reserved)
        );
        assert_eq!(check(&table, Act::Palette, &Chord::new(true, "y")), Ok(()));
        // Written by hand into the settings: the palette takes ⌘K, the search loses it.
        let keys = [("palette".to_owned(), "cmd+k".to_owned())].into();
        let table = Table::from(&keys);
        assert_eq!(table.act(&k), Some(Act::Palette));
        assert_eq!(table.chord(Act::Find), None);
        // An empty one unbinds.
        let keys = [("zoom-in".to_owned(), String::new())].into();
        assert_eq!(Table::from(&keys).chord(Act::ZoomIn), None);
    }
}
