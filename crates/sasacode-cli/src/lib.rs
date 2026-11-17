//! The sasacode command (`packages/cli`): configuration, setup, the headless and control modes,
//! and the subcommands. `main.rs` only parses arguments and calls in here.

pub mod catalog;
pub mod config;
pub mod detect;
pub mod endpoints;
pub mod json;
pub mod list_models;
pub mod update;
pub mod args;
pub mod env;
pub mod keys;
pub mod stdin;
pub mod trust;
pub mod control;
pub mod headless;
pub mod setup;
pub mod subcommands;
pub mod tui;
