//! Types for talking to sasacode from Rust: the events `sasacode -p … --output jsonl` writes, the
//! messages `--control stdio` reads, and the session logs under `$SASACODE_HOME/sessions`.
//!
//! The CLI is the source of truth. `tests/fixtures` holds real runs and the logs they wrote
//! (`cargo run -p sasacode-testing --example protocol-fixtures` makes them again), and the tests also
//! read the CLI's sources (`sasacode-agent`'s events and session, `sasacode-cli`'s control and
//! headless output), so a change there that this crate does not follow fails `cargo test`.

pub mod control;
pub mod event;
pub mod message;
pub mod session;

pub use control::{ControlMessage, Decision};
pub use event::{Event, StopCause, StreamDelta, ToolResult, parse_event};
pub use message::{AssistantMessage, Content, Message, ToolCall, ToolResultMessage, Usage, text_of};
pub use session::{SessionEntry, SessionSummary, list_sessions, read_entries, restore, sasacode_home, session_dir};
