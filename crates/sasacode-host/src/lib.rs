//! What the sasacode app shares across its interface crates (tui, cli, the bundled plugins): the
//! language the interface speaks, the providers and models it knows, and the session catalog. Not
//! part of the core (ai, agent, plugin-api, tools), whose size is kept small (`packages/host`).

pub mod i18n;
pub mod messages_en;
pub mod messages_ja;
pub mod models;
pub mod notes;
pub mod sessions;
pub mod text;

pub use i18n::*;
pub use models::*;
pub use notes::*;
pub use sessions::*;
pub use text::{plain, plain_clipped, plain_line};
