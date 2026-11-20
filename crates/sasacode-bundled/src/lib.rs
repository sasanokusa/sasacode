//! The plugins shipped with sasacode (`packages/bundled`, `plugins/*`, ADR-002).
//!
//! Each one registers through `sasacode-plugin-api` exactly like a third-party plugin did, and
//! each can be turned off with `plugins.disabled`. The ones that were separate packages before the
//! rewrite (the official plugins, jev-guard, free-search) are off until `plugins.enabled` names them,
//! except ask-user, which is on (ADR-011).

use sasacode_plugin_api::PluginFn;

pub mod action_nudge;
pub mod agent_judge;
pub mod agents_md;
pub mod auto_reconnect;
pub mod background_sessions;
pub mod browsr;
pub mod checkpoints;
pub mod compaction;
pub mod context_slim;
pub mod diagnostics;
pub mod error_hints;
pub mod git_args;
pub mod goal;
pub mod gui;
pub mod image_attach;
pub mod image_slim;
pub mod loop_guard;
pub(crate) mod net_guard;
pub(crate) mod net_util;
pub mod openai_codex;
pub mod permission_presets;
pub mod progress_notes;
pub(crate) mod project_bin;
pub mod project_trust;
pub mod regular_file;
pub mod repair;
pub mod repetition_guard;
pub mod rustxng_install;
pub mod search_tools;
pub mod subagent;
pub mod syntax_check;
pub mod text_tool_calls;
pub mod thinking_budget;
pub mod todo;
pub mod tool_repair;
pub mod usage;
pub mod web_fetch;
pub mod web_search;
pub mod write_guard;

// Off by default (`plugins.enabled` turns them on), except ask-user.
pub mod ask_user;
pub mod clean_retry;
pub mod free_search;
pub mod git_context;
pub mod hashline_edit;
pub mod jev_guard;
pub mod project_checks;
pub mod slim_profile;
pub mod workflows;

/// One shipped plugin.
#[derive(Clone, Copy)]
pub struct Bundled {
    pub name: &'static str,
    pub plugin: PluginFn,
    /// False for the plugins that load only when `plugins.enabled` names them (and
    /// `plugins.disabled` does not).
    pub on_by_default: bool,
}

const fn on(name: &'static str, plugin: PluginFn) -> Bundled {
    Bundled {
        name,
        plugin,
        on_by_default: true,
    }
}

const fn off(name: &'static str, plugin: PluginFn) -> Bundled {
    Bundled {
        name,
        plugin,
        on_by_default: false,
    }
}

/// Every shipped plugin, in load order. Hooks run in load order, so the order matters.
pub const PLUGINS: &[Bundled] = &[
    on("tool-repair", tool_repair::plugin),
    // After tool-repair: hooks run in load order, and tool-repair mends the calls this one makes.
    on("text-tool-calls", text_tool_calls::plugin),
    on("repetition-guard", repetition_guard::plugin),
    on("thinking-budget", thinking_budget::plugin),
    // Before hashline-edit: it cuts `read` output, and hashline-edit anchors only the lines left.
    off("slim-profile", slim_profile::plugin),
    // After tool-repair and before write-guard: strips N:hh| prefixes before write-guard inspects content.
    off("hashline-edit", hashline_edit::plugin),
    on("error-hints", error_hints::plugin),
    on("loop-guard", loop_guard::plugin),
    // After tool-repair: it strips line numbers from the repaired write.
    on("write-guard", write_guard::plugin),
    on("search-tools", search_tools::plugin),
    on("syntax-check", syntax_check::plugin),
    on("action-nudge", action_nudge::plugin),
    on("agents-md", agents_md::plugin),
    on("compaction", compaction::plugin),
    on("diagnostics", diagnostics::plugin),
    on("subagent", subagent::plugin),
    on("todo", todo::plugin),
    on("progress-notes", progress_notes::plugin),
    on("web-fetch", web_fetch::plugin),
    on("web-search", web_search::plugin),
    on("permission-presets", permission_presets::plugin),
    on("agent-judge", agent_judge::plugin),
    on("browsr", browsr::plugin),
    on("openai-codex", openai_codex::plugin),
    on("usage", usage::plugin),
    on("goal", goal::plugin),
    on("background-sessions", background_sessions::plugin),
    on("auto-reconnect", auto_reconnect::plugin),
    on("checkpoints", checkpoints::plugin),
    on("gui", gui::plugin),
    on("image-attach", image_attach::plugin),
    // After image-attach: it shrinks the images that one adds.
    on("image-slim", image_slim::plugin),
    // After image-slim: masks oversized old tool outputs once images are accounted for.
    on("context-slim", context_slim::plugin),
    // After the built-in ones, in the order a user would have installed them. Of these, only
    // ask-user loads without `plugins.enabled`.
    on("ask-user", ask_user::plugin),
    off("git-context", git_context::plugin),
    off("project-checks", project_checks::plugin),
    off("workflows", workflows::plugin),
    off("jev-guard", jev_guard::plugin),
    off("free-search", free_search::plugin),
    // After syntax-check & hashline-edit: observes syntax errors and snapshots any edit tool.
    off("clean-retry", clean_retry::plugin),
];

/// The shipped plugin called `name`.
pub fn find(name: &str) -> Option<&'static Bundled> {
    PLUGINS.iter().find(|p| p.name == name)
}

/// Whether `name` loads, given `plugins.enabled` and `plugins.disabled`. Disabled always wins.
pub fn wanted(plugin: &Bundled, enabled: &[String], disabled: &[String]) -> bool {
    if disabled.iter().any(|d| d == plugin.name) {
        return false;
    }
    plugin.on_by_default || enabled.iter().any(|e| e == plugin.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_order_keeps_the_hook_dependencies() {
        let names: Vec<&str> = PLUGINS.iter().map(|p| p.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
        let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(at("tool-repair") < at("text-tool-calls"));
        assert!(at("tool-repair") < at("hashline-edit"));
        assert!(at("hashline-edit") < at("write-guard"));
        assert!(at("slim-profile") < at("hashline-edit"));
        assert!(at("tool-repair") < at("write-guard"));
        assert!(at("image-attach") < at("image-slim"));
        assert!(at("image-slim") < at("context-slim"));
        assert!(at("syntax-check") < at("clean-retry"));
    }

    #[test]
    fn formerly_separate_plugins_are_off_until_enabled() {
        let p = find("git-context").unwrap();
        assert!(!wanted(p, &[], &[]));
        assert!(wanted(p, &["git-context".into()], &[]));
        assert!(!wanted(p, &["git-context".into()], &["git-context".into()]));
        assert!(wanted(find("todo").unwrap(), &[], &[]));
        assert!(!wanted(find("todo").unwrap(), &[], &["todo".into()]));
    }

    #[test]
    fn ask_user_is_the_one_formerly_separate_plugin_that_is_on() {
        let p = find("ask-user").unwrap();
        assert!(wanted(p, &[], &[]));
        assert!(!wanted(p, &[], &["ask-user".into()]));
        for name in ["git-context", "project-checks", "workflows", "jev-guard", "free-search"] {
            assert!(!find(name).unwrap().on_by_default, "{name}");
        }
    }
}
