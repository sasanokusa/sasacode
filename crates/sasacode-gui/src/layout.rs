//! How the window was laid out, kept from one launch to the next: its size and the widths its side
//! panels were given. It is kept in `gui.json` in sasacode's home, written when a panel's edge is
//! let go and when the window has stopped being resized. What it holds is brought within what the
//! window allows when it is read, so a file left by an edit, a crash or an older version cannot
//! open a window too small to use.

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
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

impl Layout {
    /// What `file` holds, within bounds. What it lacks, or holds that is not a number, is what the
    /// window opens at the first time; a file that is not there or cannot be read is all of it.
    pub fn read(file: &Path) -> Layout {
        let held: Value = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
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

    /// Writes it to `file`, whole or not at all: a window closed while it writes leaves the last
    /// one there.
    pub fn write(&self, file: &Path) -> io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let whole = |n: f32| n.round() as i64;
        let text = serde_json::to_string_pretty(&json!({
            "window": { "width": whole(self.size.width), "height": whole(self.size.height) },
            "side": whole(self.side),
            "inspector": whole(self.inspector),
        }))?;
        let part = file.with_extension("json.part");
        std::fs::write(&part, text + "\n")?;
        std::fs::rename(&part, file)
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
    fn a_window_kept_from_a_bigger_screen_is_fitted_to_this_one() {
        let screen = Size::new(1440.0, 900.0);
        assert_eq!(fit(Size::new(1280.0, 800.0), screen), None);
        assert_eq!(fit(Size::new(2560.0, 1440.0), screen), Some(screen));
        assert_eq!(fit(Size::new(2560.0, 700.0), screen), Some(Size::new(1440.0, 700.0)));
        // Never under the smallest the window can be made.
        assert_eq!(fit(Size::new(2000.0, 1000.0), Size::new(600.0, 400.0)), Some(MIN_SIZE));
    }
}
