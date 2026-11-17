//! The interactive terminal UI (`packages/tui` and the pi-tui library it is built on).

pub mod activity;
pub mod ambiguous;
pub mod app;
pub mod clipboard;
pub mod dialogs;
pub mod jsfmt;
pub mod keys;
pub mod links;
pub mod summary;
pub mod term;
pub mod testing;
pub mod text;
pub mod text_viewer;
pub mod theme;
pub mod ui;
pub mod views;

pub use app::{ListedModel, TuiConfig, TuiHost, run_tui, run_tui_with};
