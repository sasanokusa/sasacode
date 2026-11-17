//! The language the user interface speaks: one language everywhere, chosen by the user
//! (`packages/host/src/i18n.ts`).
//!
//! Strings are written in one language at the call site and carried to the other by the tables in
//! messages_en.rs / messages_ja.rs (gettext style): a string nobody has translated is shown as
//! written, and no call site is blocked on a translation existing. Model-facing text (tool results,
//! prompts) is not translated: it belongs to the conversation, not to the interface.
//!
//! Call sites pass the text as a literal first argument, `t("エラー: {error}", &[("error", &e)])` or
//! `t!("エラー: {error}", error = e)`, so the scan in tests/i18n.rs finds every string shown.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

pub use sasacode_plugin_api::Lang;

use crate::messages_en::TO_EN;
use crate::messages_ja::TO_JA;

/// 0: ja (the default), 1: en. Switched at runtime by /language.
static LANG: AtomicU8 = AtomicU8::new(0);

/// SASACODE_LANG, then the `lang` setting, then the system locale; "ja" when nothing says.
pub fn resolve_lang(setting: Option<&str>) -> Lang {
    resolve_lang_with(setting, |name| {
        std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
    })
}

/// [`resolve_lang`] with the environment read through `env` (tests; a child's environment).
pub fn resolve_lang_with(setting: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Lang {
    match env("SASACODE_LANG").unwrap_or_default().to_lowercase().as_str() {
        "ja" => return Lang::Ja,
        "en" => return Lang::En,
        _ => {}
    }
    match setting {
        Some("ja") => return Lang::Ja,
        Some("en") => return Lang::En,
        _ => {}
    }
    // An empty one is unset, as gettext reads them: the next one says.
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(&env)
        .find(|v| !v.is_empty())
        .unwrap_or_default();
    let lower = locale.to_ascii_lowercase();
    if lower.starts_with("ja") {
        return Lang::Ja;
    }
    // Nothing said (empty, C or POSIX): the product's own language.
    let plain = |name: &str| {
        lower
            .strip_prefix(name)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
    };
    if locale.is_empty() || plain("c") || plain("posix") {
        return Lang::Ja;
    }
    Lang::En
}

/// Set at startup (before the first message is shown), and by /language.
pub fn set_lang(next: Lang) {
    LANG.store(if next == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn current_lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Ja
    }
}

fn table(t: &'static [(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
    // Like an object literal: a later entry for the same key wins.
    t.iter().copied().collect()
}

fn to_en() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    M.get_or_init(|| table(TO_EN))
}

fn to_ja() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    M.get_or_init(|| table(TO_JA))
}

/// English → Japanese from the Japanese-first table: a later entry with the same English wins.
fn from_english() -> &'static HashMap<&'static str, &'static str> {
    static M: OnceLock<HashMap<&str, &str>> = OnceLock::new();
    M.get_or_init(|| TO_EN.iter().map(|&(ja, en)| (en, ja)).collect())
}

/// A string already shown in the other language, in the current one: what /language does to text
/// made before the switch (command descriptions). Text written in either language is found.
pub fn retranslate(text: &str) -> String {
    let found = match current_lang() {
        Lang::En => to_en().get(text),
        Lang::Ja => to_ja().get(text).or_else(|| from_english().get(text)),
    };
    found.map_or(text, |s| s).to_string()
}

/// The text in the current language, with `{name}` placeholders filled from `params`. Call sites
/// write dynamic parts as placeholders (`t("エラー: {error}", &[("error", &e)])`), so the whole
/// sentence is what the translation table matches on. A placeholder without a param stays as written.
pub fn t(text: &str, params: &[(&str, &dyn Display)]) -> String {
    let table = match current_lang() {
        Lang::Ja => to_ja(),
        Lang::En => to_en(),
    };
    let out = table.get(text).copied().unwrap_or(text);
    if params.is_empty() {
        return out.to_string();
    }
    fill(out, params)
}

/// `out.replace(/\{(\w+)\}/g, …)`: `\w` is ASCII here, as in a JS regex without the u flag.
fn fill(text: &str, params: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        if name_len > 0 && after.as_bytes().get(name_len) == Some(&b'}') {
            let name = &after[..name_len];
            match params.iter().find(|(k, _)| *k == name) {
                Some((_, v)) => out.push_str(&v.to_string()),
                None => out.push_str(&rest[open..open + name_len + 2]),
            }
            rest = &after[name_len + 1..];
        } else {
            // Not a placeholder: the brace is text, and matching goes on right after it.
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `t!(TEXT)`, `t!(TEXT, name = value, …)`: [`t`] with named params filling `{name}`.
#[macro_export]
macro_rules! t {
    ($text:expr $(,)?) => {
        $crate::i18n::t($text, &[])
    };
    ($text:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::t($text, &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_matches_the_js_replace() {
        let n = 3;
        let p: &[(&str, &dyn Display)] = &[("a", &"x"), ("n", &n)];
        assert_eq!(fill("{a} {n} {b} {{a}} {a-b} {} {", p), "x 3 {b} {x} {a-b} {} {");
        assert_eq!(fill("日本語{a}の{n}", p), "日本語xの3");
        assert_eq!(fill("{é}", p), "{é}");
    }
}
