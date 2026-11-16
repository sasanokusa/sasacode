//! What the window keeps from one launch to the next: how it was laid out (its size and the widths
//! its side panels were given), the choices made in its settings, and the names, pins and archive
//! given to conversations. It is kept in `gui.json` in sasacode's home, written when a panel's edge
//! is let go, when the window has stopped being resized, and when a setting changes. What it holds
//! is brought within what the window allows when it is read, so a file left by an edit, a crash or
//! an older version cannot open a window too small to use.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use iced::Size;
use serde_json::{Value, json};

use crate::app::{INSP_MAX, INSP_MIN, INSP_W, SIDE_MAX, SIDE_MIN, SIDE_W};

/// The file in sasacode's home.
pub const FILE: &str = "gui.json";
/// The size the window opens at the first time.
pub const SIZE: Size = Size::new(1280.0, 800.0);
/// The smallest the window can be made: the conversation and one side panel at its narrowest,
/// and the input under a few lines of the conversation.
pub const MIN_SIZE: Size = Size::new(760.0, 520.0);

/// How far the window's contents can be zoomed out and in.
pub const ZOOM_MIN: f32 = 0.7;
pub const ZOOM_MAX: f32 = 1.6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// The window's size as the system measures it, whatever the zoom.
    pub size: Size,
    pub side: f32,
    pub inspector: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            size: SIZE,
            side: SIDE_W,
            inspector: INSP_W,
        }
    }
}

/// What `file` holds, or nothing when it is not there or cannot be read.
fn held(file: &Path) -> Value {
    std::fs::read_to_string(file)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

impl Layout {
    /// What `file` holds, within bounds. What it lacks, or holds that is not a number, is what the
    /// window opens at the first time; a file that is not there or cannot be read is all of it.
    pub fn read(file: &Path) -> Layout {
        let held = held(file);
        let number = |value: &Value| value.as_f64().map(|n| n as f32).filter(|n| n.is_finite());
        let start = Layout::default();
        Layout {
            size: Size::new(
                number(&held["window"]["width"]).unwrap_or(start.size.width),
                number(&held["window"]["height"]).unwrap_or(start.size.height),
            ),
            side: number(&held["side"]).unwrap_or(start.side),
            inspector: number(&held["inspector"]).unwrap_or(start.inspector),
        }
        .within()
    }

    /// No smaller than the window can be made, and the panels as wide as a drag of their edges can
    /// make them.
    fn within(self) -> Layout {
        Layout {
            size: Size::new(
                self.size.width.max(MIN_SIZE.width),
                self.size.height.max(MIN_SIZE.height),
            ),
            side: self.side.clamp(SIDE_MIN, SIDE_MAX),
            inspector: self.inspector.clamp(INSP_MIN, INSP_MAX),
        }
    }
}

/// The choices made in the window's settings, and what the user said about conversations.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    /// Enter sends and Shift+Enter breaks the line; else Enter breaks it and the chord sends.
    pub enter_sends: bool,
    /// Light or dark; `None` follows the system.
    pub dark: Option<bool>,
    /// How large the window's contents are drawn.
    pub zoom: f32,
    /// The system says so when a run ends or waits for an answer while the window is not in front.
    pub notify: bool,
    /// The computer does not sleep while a run goes on.
    pub awake: bool,
    /// What a run did between its words (calls, notes) is shown, not only what was said.
    pub steps: bool,
    /// Names given to conversations, by session id.
    pub names: BTreeMap<String, String>,
    /// Conversations kept at the top of the list, the first pinned first.
    pub pinned: Vec<String>,
    /// Conversations taken out of the list.
    pub archived: BTreeSet<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            enter_sends: true,
            dark: None,
            zoom: 1.0,
            notify: true,
            awake: true,
            steps: true,
            names: BTreeMap::new(),
            pinned: Vec::new(),
            archived: BTreeSet::new(),
        }
    }
}

impl Prefs {
    /// What `file` holds; what it lacks or holds wrong is as it is the first time.
    pub fn read(file: &Path) -> Prefs {
        let held = held(file);
        let start = Prefs::default();
        let flag = |key: &str, or: bool| held[key].as_bool().unwrap_or(or);
        let ids = |key: &str| -> Vec<String> {
            held[key]
                .as_array()
                .map(|all| all.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
                .unwrap_or_default()
        };
        let mut pinned = ids("pinned");
        pinned.dedup();
        Prefs {
            enter_sends: flag("enterSends", start.enter_sends),
            dark: match held["theme"].as_str() {
                Some("light") => Some(false),
                Some("dark") => Some(true),
                _ => None,
            },
            zoom: held["zoom"]
                .as_f64()
                .map(|z| z as f32)
                .filter(|z| z.is_finite())
                .map_or(start.zoom, |z| z.clamp(ZOOM_MIN, ZOOM_MAX)),
            notify: flag("notify", start.notify),
            awake: flag("awake", start.awake),
            steps: flag("steps", start.steps),
            names: held["names"]
                .as_object()
                .map(|all| {
                    all.iter()
                        .filter_map(|(id, name)| Some((id.clone(), name.as_str()?.trim().to_owned())))
                        .filter(|(_, name)| !name.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            pinned,
            archived: ids("archived").into_iter().collect(),
        }
    }

    /// This window's choices laid over `disk`, what the file holds now, which another window may
    /// have written since this one read or wrote `kept`: what this window changed since then is
    /// taken from it, the rest from the file. Names, pins and the archive go entry by entry, so
    /// two windows naming two conversations keep both names.
    pub fn over(&self, kept: &Prefs, disk: &Prefs) -> Prefs {
        fn pick<T: PartialEq + Clone>(mine: &T, kept: &T, disk: &T) -> T {
            if mine != kept { mine.clone() } else { disk.clone() }
        }
        let mut names = disk.names.clone();
        for id in kept.names.keys().chain(self.names.keys()) {
            match self.names.get(id) {
                mine if mine == kept.names.get(id) => {}
                Some(name) => {
                    names.insert(id.clone(), name.clone());
                }
                None => {
                    names.remove(id);
                }
            }
        }
        let mut archived = disk.archived.clone();
        archived.extend(self.archived.difference(&kept.archived).cloned());
        for id in kept.archived.difference(&self.archived) {
            archived.remove(id);
        }
        let unpinned = |id: &String| kept.pinned.contains(id) && !self.pinned.contains(id);
        let mut pinned: Vec<String> = disk.pinned.iter().filter(|id| !unpinned(id)).cloned().collect();
        for id in &self.pinned {
            if !kept.pinned.contains(id) && !pinned.contains(id) {
                pinned.push(id.clone());
            }
        }
        Prefs {
            enter_sends: pick(&self.enter_sends, &kept.enter_sends, &disk.enter_sends),
            dark: pick(&self.dark, &kept.dark, &disk.dark),
            zoom: pick(&self.zoom, &kept.zoom, &disk.zoom),
            notify: pick(&self.notify, &kept.notify, &disk.notify),
            awake: pick(&self.awake, &kept.awake, &disk.awake),
            steps: pick(&self.steps, &kept.steps, &disk.steps),
            names,
            pinned,
            archived,
        }
    }
}

/// Writes how the window is laid out and what was chosen to `file`, whole or not at all: a window
/// closed while it writes leaves the last one there.
pub fn write(file: &Path, layout: &Layout, prefs: &Prefs) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let whole = |n: f32| n.round() as i64;
    let theme = match prefs.dark {
        None => "system",
        Some(false) => "light",
        Some(true) => "dark",
    };
    let text = serde_json::to_string_pretty(&json!({
        "window": { "width": whole(layout.size.width), "height": whole(layout.size.height) },
        "side": whole(layout.side),
        "inspector": whole(layout.inspector),
        "enterSends": prefs.enter_sends,
        "theme": theme,
        // Rounded as the number it is written as, so that 1.4 is not 1.399999976158142.
        "zoom": (f64::from(prefs.zoom) * 100.0).round() / 100.0,
        "notify": prefs.notify,
        "awake": prefs.awake,
        "steps": prefs.steps,
        "names": prefs.names,
        "pinned": prefs.pinned,
        "archived": prefs.archived,
    }))?;
    let part = file.with_extension(format!("json.{}.part", std::process::id()));
    std::fs::write(&part, text + "\n")?;
    std::fs::rename(&part, file)
}

/// The window's size made no larger than the screen it is on, when it is larger: a size kept from a
/// bigger screen would put the window's edges where they cannot be taken.
pub fn fit(size: Size, screen: Size) -> Option<Size> {
    let fit = Size::new(
        size.width.min(screen.width).max(MIN_SIZE.width),
        size.height.min(screen.height).max(MIN_SIZE.height),
    );
    (fit != size).then_some(fit)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn two_windows_keep_what_each_said_about_conversations() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("gui.json");
        let mut first = Prefs::default();
        first.names.insert("a".into(), "前からの名前".into());
        first.pinned.push("p".into());
        write(&file, &Layout::default(), &first).unwrap();

        // Both windows open on what the file holds.
        let kept = Prefs::read(&file);
        let (mut one, mut two) = (kept.clone(), kept.clone());
        // One names a conversation and archives another, and is written.
        one.names.insert("b".into(), "一つ目の窓".into());
        one.archived.insert("x".into());
        let written = one.over(&kept, &Prefs::read(&file));
        write(&file, &Layout::default(), &written).unwrap();
        // The other, which still has neither, unpins one, names one, and zooms.
        two.pinned.clear();
        two.names.insert("c".into(), "二つ目の窓".into());
        two.zoom = 1.2;
        let written = two.over(&kept, &Prefs::read(&file));
        write(&file, &Layout::default(), &written).unwrap();

        let now = Prefs::read(&file);
        assert_eq!(
            now.names.keys().map(String::as_str).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert!(now.archived.contains("x"));
        assert!(now.pinned.is_empty());
        assert_eq!(now.zoom, 1.2);
        // What neither changed stays as the file had it; a name taken away is gone.
        let mut three = now.clone();
        three.names.remove("a");
        let after = three.over(&now, &now);
        assert!(!after.names.contains_key("a") && after.zoom == 1.2);
        std::fs::remove_dir_all(dir).unwrap();
    }
    use crate::app::{App, Edge, Msg, Options};
    use crate::i18n::Strings;

    fn home(name: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("sasacode-gui-layout-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        home
    }

    fn layout(width: f32, height: f32, side: f32, inspector: f32) -> Layout {
        Layout {
            size: Size::new(width, height),
            side,
            inspector,
        }
    }

    #[test]
    fn a_kept_layout_is_read_within_what_the_window_allows() {
        let home = home("read");
        std::fs::create_dir_all(&home).unwrap();
        let file = home.join(FILE);
        // None kept yet, or nothing that can be read: the window opens as the first time.
        assert_eq!(Layout::read(&file), Layout::default());
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(Layout::read(&file), Layout::default());

        // What fits is taken as it is.
        std::fs::write(
            &file,
            r#"{"window":{"width":1600,"height":1000},"side":300,"inspector":500}"#,
        )
        .unwrap();
        assert_eq!(Layout::read(&file), layout(1600.0, 1000.0, 300.0, 500.0));

        // Too small a window, a panel narrower or wider than a drag makes it: the nearest that is
        // allowed. What is not a number is what the window opens at.
        std::fs::write(
            &file,
            r#"{"window":{"width":-5,"height":"tall"},"side":20,"inspector":5000}"#,
        )
        .unwrap();
        assert_eq!(
            Layout::read(&file),
            layout(MIN_SIZE.width, SIZE.height, SIDE_MIN, INSP_MAX)
        );
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_layout_is_kept_when_an_edge_is_let_go_and_when_the_window_rests() {
        let home = home("keep");
        let file = home.join(FILE);
        let options = Options {
            program: "sasacode".into(),
            cwd: "/tmp".into(),
            session: None,
            layout: layout(1440.0, 900.0, 300.0, 420.0),
            prefs: Prefs::default(),
            layout_file: Some(file.clone()),
        };
        let mut app = App::blank(options, Strings::get("ja"));
        assert_eq!((app.width, app.side_w, app.insp_w), (1440.0, 300.0, 420.0));

        // Nothing is written while the edge is held, and the home is made when it is let go.
        let _ = app.update(Msg::Grab(Edge::Side));
        let _ = app.update(Msg::Drag(320.0));
        assert!(!file.exists());
        let _ = app.update(Msg::Drop);
        assert_eq!(Layout::read(&file), layout(1440.0, 900.0, 320.0, 420.0));
        let _ = app.update(Msg::ResetWidth(Edge::Inspector));
        assert_eq!(Layout::read(&file).inspector, INSP_W);

        // A resize is kept once it has rested: only the last size of it.
        let _ = app.update(Msg::Resized(Size::new(1500.0, 950.0)));
        let _ = app.update(Msg::Resized(Size::new(1600.0, 1000.0)));
        let _ = app.update(Msg::Rested(1));
        assert_eq!(Layout::read(&file).size, Size::new(1440.0, 900.0));
        let _ = app.update(Msg::Rested(2));
        assert_eq!(Layout::read(&file).size, Size::new(1600.0, 1000.0));

        // What has not changed is not written again. A panel folded by a drag keeps the width it
        // had when its edge was taken, not the narrowest it passed through.
        std::fs::remove_file(&file).unwrap();
        let _ = app.update(Msg::Rested(2));
        let _ = app.update(Msg::Grab(Edge::Side));
        for x in [250.0, 180.0, 130.0, 60.0] {
            let _ = app.update(Msg::Drag(x));
        }
        assert!(!app.side);
        let _ = app.update(Msg::Drop);
        assert!(!file.exists());
        assert_eq!(app.side_w, 320.0);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn settings_are_kept_with_the_layout_and_read_within_bounds() {
        let home = home("prefs");
        let file = home.join(FILE);
        let prefs = Prefs {
            enter_sends: false,
            dark: Some(true),
            zoom: 1.2,
            notify: false,
            awake: false,
            steps: false,
            names: [("aaaa".to_owned(), "名前".to_owned())].into(),
            pinned: vec!["bbbb".into(), "aaaa".into()],
            archived: ["cccc".to_owned()].into(),
        };
        write(&file, &Layout::default(), &prefs).unwrap();
        assert_eq!(Prefs::read(&file), prefs);
        assert_eq!(Layout::read(&file), Layout::default());
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"zoom\": 1.2\n"));

        // An older file has none of it; a zoom too large is the largest; a blank name is none.
        std::fs::write(
            &file,
            r#"{"zoom":9,"theme":"blue","names":{"a":"  ","b":"B"},"pinned":"x"}"#,
        )
        .unwrap();
        let read = Prefs::read(&file);
        assert_eq!(read.zoom, ZOOM_MAX);
        assert_eq!(read.dark, None);
        assert_eq!(read.names.len(), 1);
        assert!(read.pinned.is_empty() && read.enter_sends && read.steps);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_window_kept_from_a_bigger_screen_is_fitted_to_this_one() {
        let screen = Size::new(1440.0, 900.0);
        assert_eq!(fit(Size::new(1280.0, 800.0), screen), None);
        assert_eq!(fit(Size::new(2560.0, 1440.0), screen), Some(screen));
        assert_eq!(fit(Size::new(2560.0, 700.0), screen), Some(Size::new(1440.0, 700.0)));
        // Never under the smallest the window can be made.
        assert_eq!(fit(Size::new(2000.0, 1000.0), Size::new(600.0, 400.0)), Some(MIN_SIZE));
    }
}
