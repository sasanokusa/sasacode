//! config.json: the global one (~/.sasacode/config.json) and a project's (.sasacode/config.json).
//!
//! A config is kept as the JSON object it was read as: checks drop bad keys, merging works on the
//! JSON, and [`Settings`] gives typed access to the result. That keeps unknown-but-valid shapes
//! (provider entries, MCP server entries, plugin settings) exactly as the user wrote them.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Map, Value};

use sasacode_agent::tool_search::ToolSearchConfig;
use sasacode_ai::{ModelInfo, ThinkingLevel};
use sasacode_host::{builtin_providers, t};
use sasacode_plugin_api::{PermissionMode, PermissionRules};

use crate::json::read_json;

/// A sanitized config object (keys in the order they were written).
pub type Config = Map<String, Value>;

pub use sasacode_plugin_api::sasacode_home;

const THINKING: [&str; 6] = ["off", "low", "medium", "high", "xhigh", "max"];
const MODES: [&str; 4] = ["auto", "agent", "ask", "edits"];

fn is_string(v: &Value) -> bool {
    v.is_string()
}
fn is_count(v: &Value, max: u64) -> bool {
    // Number.isInteger: 3.0 counts, 3.5 does not.
    match v {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                u <= max
            } else if let Some(f) = n.as_f64() {
                f.fract() == 0.0 && f >= 0.0 && f <= max as f64
            } else {
                false
            }
        }
        _ => false,
    }
}
fn is_strings(v: &Value) -> bool {
    v.as_array().is_some_and(|a| a.iter().all(is_string))
}
fn is_object_of_objects(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.values().all(Value::is_object))
}
fn absent_or(o: &Map<String, Value>, k: &str, f: impl Fn(&Value) -> bool) -> bool {
    o.get(k).is_none_or(&f)
}

/// Shape checks per key; a value that fails is dropped with a warning instead of silently merging.
fn check(key: &str, v: &Value) -> Option<bool> {
    Some(match key {
        "model" | "judgeModel" | "instructions" => is_string(v),
        "lang" => v == "ja" || v == "en",
        "models" | "trustedProjects" => is_strings(v),
        "thinking" => v.as_str().is_some_and(|s| THINKING.contains(&s)),
        "providers" | "modelOverrides" | "mcpServers" => is_object_of_objects(v),
        "permissions" => v.as_object().is_some_and(|o| {
            ["allow", "ask", "deny", "softDeny"]
                .iter()
                .all(|k| absent_or(o, k, is_strings))
                && absent_or(o, "mode", |m| m.as_str().is_some_and(|m| MODES.contains(&m)))
        }),
        "maxTurns" => is_count(v, 100_000),
        "maxRetries" => is_count(v, 20),
        "updateCheck" => v.is_boolean(),
        // `enabled` (ADR-011) is new; the TS check ignored unknown keys here, so old versions read it.
        "plugins" => v.as_object().is_some_and(|o| {
            absent_or(o, "disabled", is_strings)
                && absent_or(o, "enabled", is_strings)
                && absent_or(o, "settings", is_object_of_objects)
        }),
        "tools" => v.as_object().is_some_and(|o| absent_or(o, "disabled", is_strings)),
        "toolSearch" => v.is_object(),
        "tui" => v
            .as_object()
            .is_some_and(|o| ["altScreen", "bell"].iter().all(|k| absent_or(o, k, Value::is_boolean))),
        _ => return None,
    })
}

pub fn sanitize_config(raw: &Value, source: &str, warnings: &mut Vec<String>) -> Config {
    let Some(raw) = raw.as_object() else {
        warnings.push(t!("{source}: not a JSON object; ignored", source = source));
        return Config::new();
    };
    let mut out = Config::new();
    for (k, v) in raw {
        match check(k, v) {
            None => warnings.push(t!("{source}: unknown key \"{key}\" ignored", source = source, key = k)),
            Some(false) => warnings.push(t!(
                "{source}: \"{key}\" has an invalid value; ignored",
                source = source,
                key = k
            )),
            Some(true) => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

/// Lists that both levels add to. Anything else named "disabled" (mcpServers.<name>.disabled) is a
/// plain value.
const ACCUMULATE: [&str; 7] = [
    "permissions.allow",
    "permissions.ask",
    "permissions.deny",
    "permissions.softDeny",
    "tools.disabled",
    "plugins.disabled",
    "plugins.enabled",
];

/// Later values win; objects merge recursively. Permission rules and disabled lists accumulate
/// (never replace), so a project cannot drop the user's deny rules or re-enable what they turned
/// off.
pub fn merge_config(base: &Config, over: &Config) -> Config {
    fn merge(a: &Value, b: &Value, path: &str) -> Value {
        if ACCUMULATE.contains(&path)
            && let (Value::Array(x), Value::Array(y)) = (a, b)
        {
            let mut out: Vec<Value> = Vec::new();
            for v in x.iter().chain(y) {
                if !out.contains(v) {
                    out.push(v.clone());
                }
            }
            return Value::Array(out);
        }
        if let (Value::Object(x), Value::Object(y)) = (a, b) {
            let mut out = x.clone();
            for (k, v) in y {
                let merged = match x.get(k) {
                    Some(old) => merge(
                        old,
                        v,
                        &if path.is_empty() {
                            k.clone()
                        } else {
                            format!("{path}.{k}")
                        },
                    ),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            return Value::Object(out);
        }
        b.clone()
    }
    match merge(&Value::Object(base.clone()), &Value::Object(over.clone()), "") {
        Value::Object(o) => o,
        _ => Config::new(),
    }
}

/// Turns per run when none is configured: long tasks fit, a runaway loop stops. 0 means no limit.
pub const DEFAULT_MAX_TURNS: u64 = 200;
/// The SDKs' retry count when none is configured.
pub const DEFAULT_RETRIES: u64 = 8;

/// How much a mode restricts: a project may only move to an equal or stricter one without trust.
fn mode_strength(m: &str) -> u8 {
    match m {
        "auto" => 0,
        "agent" => 1,
        "edits" => 2,
        _ => 3,
    }
}

/// Routers reach many companies with one key, so which model they serve decides where the
/// conversation and code go.
const ROUTERS: [&str; 1] = ["openrouter"];

fn num(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64)
}

/// Split a project's config into what it may set on its own and what needs the user's trust.
/// Without trust a project can only tighten things: it cannot add endpoints (even keyless ones
/// would receive your code), change model metadata (baseUrl could reroute keys), configure or
/// disable plugins, start MCP servers, add allow rules, loosen the permission mode, or raise the
/// turn and retry limits.
pub fn split_project_config(project: &Config, global: &Config) -> (Config, Config) {
    let mut safe = Config::new();
    let mut elevated = Config::new();
    let global_providers = global.get("providers").and_then(Value::as_object);
    let known_provider = |spec: &str| {
        // spec.slice(0, spec.indexOf("/")): with no slash, indexOf is -1 and the slice drops the last char.
        let name = match spec.find('/') {
            Some(i) => &spec[..i],
            None => {
                let mut cut = spec.char_indices().map(|(i, _)| i).collect::<Vec<_>>();
                let end = cut.pop().unwrap_or(0);
                &spec[..end]
            }
        };
        if ROUTERS.contains(&name) {
            return false;
        }
        builtin_providers().contains_key(name) || global_providers.is_some_and(|p| p.contains_key(name))
    };
    // tools.disabled only adds to the user's list (see merge_config).
    for key in ["thinking", "instructions", "toolSearch", "tools", "tui"] {
        if let Some(v) = project.get(key) {
            safe.insert(key.into(), v.clone());
        }
    }
    // 0 turns means no limit, so it is the loosest value.
    let turns = |n: Option<f64>| match n {
        Some(n) if n != 0.0 => n,
        _ => f64::INFINITY,
    };
    if let Some(v) = project.get("maxTurns") {
        let limit = num(global.get("maxTurns")).unwrap_or(DEFAULT_MAX_TURNS as f64);
        let target = if turns(num(Some(v))) <= turns(Some(limit)) {
            &mut safe
        } else {
            &mut elevated
        };
        target.insert("maxTurns".into(), v.clone());
    }
    if let Some(v) = project.get("maxRetries") {
        let limit = num(global.get("maxRetries")).unwrap_or(DEFAULT_RETRIES as f64);
        let target = if num(Some(v)).unwrap_or(0.0) <= limit {
            &mut safe
        } else {
            &mut elevated
        };
        target.insert("maxRetries".into(), v.clone());
    }
    for key in ["model", "judgeModel"] {
        if let Some(v) = project.get(key) {
            let target = if v.as_str().is_some_and(known_provider) {
                &mut safe
            } else {
                &mut elevated
            };
            target.insert(key.into(), v.clone());
        }
    }
    if let Some(models) = project.get("models").and_then(Value::as_array) {
        let (ok, rest): (Vec<Value>, Vec<Value>) = models
            .iter()
            .cloned()
            .partition(|m| m.as_str().is_some_and(known_provider));
        if !ok.is_empty() {
            safe.insert("models".into(), Value::Array(ok));
        }
        if !rest.is_empty() {
            elevated.insert("models".into(), Value::Array(rest));
        }
    }
    if let Some(p) = project.get("permissions").and_then(Value::as_object) {
        let mut sp = Map::new();
        for k in ["ask", "deny", "softDeny"] {
            if let Some(v) = p.get(k) {
                sp.insert(k.into(), v.clone());
            }
        }
        let mut ep = Map::new();
        if let Some(allow) = p.get("allow").filter(|a| a.as_array().is_some_and(|a| !a.is_empty())) {
            ep.insert("allow".into(), allow.clone());
        }
        if let Some(mode) = p.get("mode").and_then(Value::as_str).filter(|m| !m.is_empty()) {
            let floor = global
                .get("permissions")
                .and_then(|p| p.get("mode"))
                .and_then(Value::as_str)
                .unwrap_or("edits");
            if mode_strength(mode) >= mode_strength(floor) {
                sp.insert("mode".into(), mode.into());
            } else {
                ep.insert("mode".into(), mode.into());
            }
        }
        safe.insert("permissions".into(), Value::Object(sp));
        if !ep.is_empty() {
            elevated.insert("permissions".into(), Value::Object(ep));
        }
    }
    for key in ["providers", "modelOverrides", "plugins", "mcpServers"] {
        if let Some(v) = project.get(key) {
            elevated.insert(key.into(), v.clone());
        }
    }
    (safe, elevated)
}

fn keys(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}
fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// One line per thing the trust prompt covers.
pub fn describe_elevated(e: &Config) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(providers) = e.get("providers").and_then(Value::as_object) {
        for (n, p) in providers {
            let url = p
                .get("baseUrl")
                .and_then(Value::as_str)
                .or_else(|| p.get("api").and_then(Value::as_str))
                .unwrap_or("?");
            out.push(t!("endpoint {name} ({url})", name = n, url = url));
        }
    }
    if let Some(m) = e.get("model").and_then(Value::as_str).filter(|m| !m.is_empty()) {
        out.push(t!("model {model}", model = m));
    }
    let models = strings(e.get("models"));
    if !models.is_empty() {
        out.push(t!("models {models}", models = models.join(", ")));
    }
    // It decides which tool calls run in agent mode: a project must not pick it unseen.
    if let Some(m) = e.get("judgeModel").and_then(Value::as_str).filter(|m| !m.is_empty()) {
        out.push(t!("judge model {model}", model = m));
    }
    if e.contains_key("modelOverrides") {
        out.push(t!(
            "model overrides for {models}",
            models = keys(e.get("modelOverrides")).join(", ")
        ));
    }
    for n in keys(e.get("mcpServers")) {
        out.push(t!("MCP server {name}", name = n));
    }
    let plugins = e.get("plugins");
    let disabled = strings(plugins.and_then(|p| p.get("disabled")));
    if !disabled.is_empty() {
        out.push(t!("disables plugins {names}", names = disabled.join(", ")));
    }
    let enabled = strings(plugins.and_then(|p| p.get("enabled")));
    if !enabled.is_empty() {
        out.push(t!("enables plugins {names}", names = enabled.join(", ")));
    }
    if let Some(s) = plugins.and_then(|p| p.get("settings")) {
        out.push(t!("settings for plugins {names}", names = keys(Some(s)).join(", ")));
    }
    let permissions = e.get("permissions");
    let allow = strings(permissions.and_then(|p| p.get("allow")));
    if !allow.is_empty() {
        out.push(t!("allow rules {rules}", rules = allow.join(", ")));
    }
    if let Some(m) = permissions
        .and_then(|p| p.get("mode"))
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
    {
        out.push(t!("permission mode {mode}", mode = m));
    }
    if let Some(v) = e.get("maxTurns") {
        let n = v.as_f64().unwrap_or(0.0);
        out.push(if n != 0.0 {
            format!("maxTurns {}", sasacode_ai::json::js_f64_string(n))
        } else {
            t!("maxTurns unlimited")
        });
    }
    if let Some(v) = e.get("maxRetries") {
        out.push(format!(
            "maxRetries {}",
            sasacode_ai::json::js_f64_string(v.as_f64().unwrap_or(0.0))
        ));
    }
    out
}

pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<String>,
    /// Project settings that need trust (applied only when `trusted`).
    pub elevated: Config,
    pub global: Config,
}

/// The two config files as read, before trust decides what of the project's applies.
#[derive(Clone, Default)]
pub struct ConfigFiles {
    pub global: Config,
    pub project: Config,
    pub warnings: Vec<String>,
}

pub fn global_config_path() -> PathBuf {
    sasacode_home().join("config.json")
}

pub fn read_config_files(cwd: &Path) -> anyhow::Result<ConfigFiles> {
    read_config_files_from(&global_config_path(), cwd)
}

/// [`read_config_files`] with the global file given (tests; SASACODE_HOME is process-wide).
pub fn read_config_files_from(global_path: &Path, cwd: &Path) -> anyhow::Result<ConfigFiles> {
    let mut warnings = Vec::new();
    let global = sanitize_config(
        &read_json(global_path)?,
        &global_path.display().to_string(),
        &mut warnings,
    );
    let mut project = sanitize_config(
        &read_json(&cwd.join(".sasacode").join("config.json"))?,
        ".sasacode/config.json",
        &mut warnings,
    );
    if project.contains_key("trustedProjects") {
        warnings.push(t!(
            ".sasacode/config.json: trustedProjects is only read from the global config; ignored"
        ));
        project.shift_remove("trustedProjects");
    }
    Ok(ConfigFiles {
        global,
        project,
        warnings,
    })
}

/// `files`: already read (startup reads them once for the trust check and for setup).
pub fn load_config(cwd: &Path, trusted: bool, files: Option<ConfigFiles>) -> anyhow::Result<LoadedConfig> {
    let files = match files {
        Some(f) => f,
        None => read_config_files(cwd)?,
    };
    let ConfigFiles {
        global,
        project,
        warnings,
    } = files;
    let mut warnings = warnings;
    let (safe, elevated) = split_project_config(&project, &global);
    let items = describe_elevated(&elevated);
    // Allowed without trust, but the user should know the project, not they, picked the model.
    if !trusted
        && let Some(m) = safe.get("model").and_then(Value::as_str).filter(|m| !m.is_empty())
        && global.get("model").and_then(Value::as_str) != Some(m)
    {
        warnings.push(t!(
            ".sasacode/config.json: this project selects the model {model}",
            model = m
        ));
    }
    if !trusted && !items.is_empty() {
        warnings.push(t!(
            ".sasacode/config.json: not applied until you trust this project ({items})",
            items = items.join("; ")
        ));
    }
    let config = merge_config(&global, if trusted { &project } else { &safe });
    Ok(LoadedConfig {
        config,
        warnings,
        elevated,
        global,
    })
}

/// Typed view of a sanitized config. Every field passed its check, so reading never fails; a value
/// that still does not fit (a provider entry with a wrong field type) reads as absent.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub model: Option<String>,
    pub judge_model: Option<String>,
    pub lang: Option<String>,
    pub models: Option<Vec<String>>,
    pub thinking: Option<ThinkingLevel>,
    pub providers: Option<Map<String, Value>>,
    pub model_overrides: Option<Map<String, Value>>,
    pub permissions: Option<PermissionsConfig>,
    pub instructions: Option<String>,
    pub max_turns: Option<f64>,
    pub max_retries: Option<f64>,
    pub update_check: Option<bool>,
    pub trusted_projects: Option<Vec<String>>,
    pub plugins: Option<PluginsConfig>,
    pub tools: Option<ToolsConfig>,
    pub mcp_servers: Option<Value>,
    pub tool_search: Option<Value>,
    pub tui: Option<TuiConfig>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PermissionsConfig {
    #[serde(flatten)]
    pub rules: PermissionRules,
    pub mode: Option<PermissionMode>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct PluginsConfig {
    pub disabled: Vec<String>,
    pub enabled: Vec<String>,
    pub settings: Map<String, Value>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub disabled: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TuiConfig {
    pub alt_screen: Option<bool>,
    pub bell: Option<bool>,
}

impl Settings {
    pub fn of(config: &Config) -> Settings {
        let mut s = Settings::default();
        macro_rules! field {
            ($key:literal, $f:ident) => {
                if let Some(v) = config.get($key) {
                    s.$f = serde_json::from_value(v.clone()).ok();
                }
            };
        }
        field!("model", model);
        field!("judgeModel", judge_model);
        field!("lang", lang);
        field!("models", models);
        field!("thinking", thinking);
        field!("providers", providers);
        field!("modelOverrides", model_overrides);
        field!("permissions", permissions);
        field!("instructions", instructions);
        field!("maxTurns", max_turns);
        field!("maxRetries", max_retries);
        field!("updateCheck", update_check);
        field!("trustedProjects", trusted_projects);
        field!("plugins", plugins);
        field!("tools", tools);
        field!("mcpServers", mcp_servers);
        field!("toolSearch", tool_search);
        field!("tui", tui);
        s
    }

    pub fn tool_search_config(&self) -> Option<ToolSearchConfig> {
        self.tool_search
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    pub fn model_override(&self, spec: &str) -> Option<Value> {
        self.model_overrides.as_ref().and_then(|o| o.get(spec)).cloned()
    }

    pub fn disabled_plugins(&self) -> Vec<String> {
        self.plugins.as_ref().map(|p| p.disabled.clone()).unwrap_or_default()
    }

    pub fn enabled_plugins(&self) -> Vec<String> {
        self.plugins.as_ref().map(|p| p.enabled.clone()).unwrap_or_default()
    }

    pub fn disabled_tools(&self) -> Vec<String> {
        self.tools.as_ref().map(|p| p.disabled.clone()).unwrap_or_default()
    }

    pub fn plugin_settings(&self, name: &str) -> Value {
        self.plugins
            .as_ref()
            .and_then(|p| p.settings.get(name))
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()))
    }
}

/// `ModelInfo` overrides are applied by `sasacode_host::models`; re-exported for setup.
pub type Model = ModelInfo;

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
