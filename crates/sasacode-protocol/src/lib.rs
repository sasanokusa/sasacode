//! Types for talking to sasacode from Rust: the events `sasacode -p … --output jsonl` writes, the
//! messages `--control stdio` reads, and the session logs under `$SASACODE_HOME/sessions`.
//!
//! The TypeScript CLI is the source of truth for now. `tests/fixtures` holds a real run and the log
//! it wrote, and the tests also read the TypeScript sources, so a change there that this crate does
//! not follow fails `cargo test`. A future Rust core is meant to use these same types.

pub mod control;
pub mod event;
pub mod message;
pub mod session;

pub use control::{ControlMessage, Decision};
pub use event::{Event, StopCause, StreamDelta, ToolResult, parse_event};
pub use message::{AssistantMessage, Content, Message, ToolCall, ToolResultMessage, Usage, text_of};
pub use session::{SessionEntry, SessionSummary, list_sessions, read_entries, restore, sasacode_home, session_dir};
