//! The sasacode command (`packages/cli`): configuration, setup, the headless and control modes,
//! and the subcommands. `main.rs` only parses arguments and calls in here.

pub mod anthropic_profile;
pub mod args;
pub mod catalog;
pub mod config;
pub mod control;
pub mod detect;
pub mod endpoints;
pub mod env;
pub mod headless;
pub mod json;
pub mod keys;
pub mod list_models;
pub mod listings;
pub mod setup;
pub mod stdin;
pub mod subcommands;
pub mod trust;
pub mod tui;
pub mod update;
