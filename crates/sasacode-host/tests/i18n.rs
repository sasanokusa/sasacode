// packages/host/test/i18n.test.ts
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use regex::Regex;
use sasacode_host::messages_en::TO_EN;
use sasacode_host::messages_ja::TO_JA;
use sasacode_host::{Lang, resolve_lang_with, retranslate, set_lang, t};

/// The language is global: tests that switch it run one at a time and switch back.
static LANG_LOCK: Mutex<()> = Mutex::new(());

fn with_lang<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    let _guard = LANG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_lang(lang);
    let out = f();
    set_lang(Lang::Ja);
    out
}

/// `/[ぁ-んァ-ヶ一-龠]/`
fn is_japanese(s: &str) -> bool {
    s.chars().any(|c| matches!(c, 'ぁ'..='ん' | 'ァ'..='ヶ' | '一'..='龠'))
}

fn placeholders(s: &str) -> Vec<String> {
    let re = Regex::new(r"\{([A-Za-z0-9_]+)\}").unwrap();
    let mut v: Vec<String> = re.captures_iter(s).map(|c| c[1].to_string()).collect();
    v.sort();
    v
}

fn files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            files(&p, ext, out);
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn table(text: &str) -> Option<&'static str> {
    let t: &[(&str, &str)] = if is_japanese(text) { TO_EN } else { TO_JA };
    t.iter()
        .rev()
        .find(|(k, _)| *k == text)
        .map(|(_, v)| *v)
        .filter(|v| !v.is_empty())
}

/// Every t("…") / t('…') literal in the TS interface's sources, as the TS test collected them.
/// Kept while packages/ exists (the reference for the tables); None once it is gone.
fn ts_literals() -> Option<Vec<(String, String)>> {
    let root = repo().join("packages");
    if !root.is_dir() {
        return None;
    }
    let call_const = Regex::new(r"(?:^|[^A-Za-z0-9_])t\(([A-Z_a-z][A-Za-z0-9_]*)[,)]").unwrap();
    let record = Regex::new(r"const ([A-Za-z0-9_]+): Partial<Record<[A-Za-z0-9_]+, string>> = \{([^}]*)\}").unwrap();
    let record_value = Regex::new(r#":\s*("(?:[^"\\]|\\.)*")"#).unwrap();
    let call_lit = Regex::new(r#"(?:^|[^A-Za-z0-9_])t\(\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')"#).unwrap();
    let mut out = Vec::new();
    for pkg in ["tui", "cli", "bundled", "agent", "host"] {
        let dir = root.join(pkg).join("src");
        let mut list = Vec::new();
        files(&dir, "ts", &mut list);
        for f in list {
            let rel = f.strip_prefix(&dir).unwrap().to_string_lossy().into_owned();
            if rel.starts_with("messages-") {
                continue;
            }
            let file = format!("{pkg}/{rel}");
            let src = std::fs::read_to_string(&f).unwrap();
            // Constants shown through t(NAME): `const NAME = "…"` or a template without substitutions.
            for m in call_const.captures_iter(&src) {
                let def = Regex::new(&format!(
                    r#"const {} = (`[^`$]*`|"(?:[^"\\]|\\.)*")"#,
                    regex::escape(&m[1])
                ))
                .unwrap()
                .captures(&src)
                .map(|c| c[1].to_string());
                if let Some(def) = def {
                    let text = if def.starts_with('`') {
                        def[1..def.len() - 1].to_string()
                    } else {
                        serde_json::from_str(&def).unwrap()
                    };
                    out.push((file.clone(), text));
                }
            }
            // Tables of messages looked up and then translated (headless.ts's STOP_HINT).
            for m in record.captures_iter(&src) {
                for v in record_value.captures_iter(&m[2]) {
                    out.push((file.clone(), serde_json::from_str(&v[1]).unwrap()));
                }
            }
            for m in call_lit.captures_iter(&src) {
                let raw = &m[1];
                let text: String = if raw.starts_with('"') {
                    serde_json::from_str(raw).unwrap()
                } else {
                    let inner = raw[1..raw.len() - 1].replace("\\'", "'").replace('"', "\\\"");
                    serde_json::from_str(&format!("\"{inner}\"")).unwrap()
                };
                out.push((file.clone(), text));
            }
        }
    }
    Some(out)
}

/// A Rust string literal (`"…"` with escapes, or `r#"…"#`) at the start of `s`.
fn rust_literal(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix('r') {
        let hashes = rest.bytes().take_while(|b| *b == b'#').count();
        let body = rest[hashes..].strip_prefix('"')?;
        let close = format!("\"{}", "#".repeat(hashes));
        return body.find(&close).map(|i| body[..i].to_string());
    }
    let mut chars = s.strip_prefix('"')?.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                'x' => {
                    let h: String = [chars.next()?, chars.next()?].iter().collect();
                    out.push(char::from(u8::from_str_radix(&h, 16).ok()?));
                }
                'u' => {
                    chars.next()?; // {
                    let h: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    out.push(char::from_u32(u32::from_str_radix(&h, 16).ok()?)?);
                }
                '\n' => {
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                }
                _ => return None,
            },
            c => out.push(c),
        }
    }
    None
}

/// Every `t("…")` / `t!("…")` (and `t(NAME)` of a `const NAME: &str`) in the Rust interface crates.
fn rust_literals() -> Vec<(String, String)> {
    let call_lit = Regex::new(r#"(?m)(?:^|[^A-Za-z0-9_.])t!?\(\s*(r#*"|")"#).unwrap();
    let call_const = Regex::new(r"(?m)(?:^|[^A-Za-z0-9_.])t!?\(\s*([A-Za-z_][A-Za-z0-9_]*)\s*[,)]").unwrap();
    let mut out = Vec::new();
    for krate in ["tui", "cli", "bundled", "agent", "host"] {
        let dir = repo().join("crates").join(format!("sasacode-{krate}")).join("src");
        let mut list = Vec::new();
        files(&dir, "rs", &mut list);
        for f in list {
            let rel = f.strip_prefix(&dir).unwrap().to_string_lossy().into_owned();
            if rel.starts_with("messages_") {
                continue;
            }
            let file = format!("{krate}/{rel}");
            let src = std::fs::read_to_string(&f).unwrap();
            for m in call_lit.captures_iter(&src) {
                let at = m.get(1).unwrap().start();
                let text = rust_literal(&src[at..]).unwrap_or_else(|| panic!("{file}: unreadable literal at {at}"));
                out.push((file.clone(), text));
            }
            for m in call_const.captures_iter(&src) {
                let def = Regex::new(&format!(
                    r#"(?:const|static)\s+{}\s*:\s*&(?:'static\s+)?str\s*=\s*"#,
                    regex::escape(&m[1])
                ))
                .unwrap()
                .find(&src)
                .and_then(|d| rust_literal(&src[d.end()..]));
                if let Some(text) = def {
                    out.push((file.clone(), text));
                }
            }
        }
    }
    out
}

#[test]
fn every_string_shown_through_t_has_a_translation_with_the_same_placeholders() {
    let missing = |found: &[(String, String)]| -> Vec<String> {
        found
            .iter()
            .filter(|(_, text)| table(text).is_none())
            .map(|(f, text)| format!("{f}: {text}"))
            .collect()
    };
    if let Some(found) = ts_literals() {
        assert!(found.len() > 100, "only {} literals in the TS sources", found.len());
        assert_eq!(missing(&found), Vec::<String>::new());
    }
    // The Rust call sites. The TS test's floor (> 100 literals) waits for the tui and cli ports.
    let found = rust_literals();
    assert_eq!(missing(&found), Vec::<String>::new());
    for table in [TO_EN, TO_JA] {
        for (from, to) in table {
            assert_eq!(placeholders(to), placeholders(from), "{from}");
        }
    }
}

#[test]
fn the_tables_hold_every_ts_entry() {
    // Counted from Object.keys of the TS tables when they were generated.
    assert_eq!(TO_EN.len(), 273);
    assert_eq!(TO_JA.len(), 158);
}

#[test]
fn t_follows_the_language_and_fills_placeholders() {
    with_lang(Lang::Ja, || {
        assert_eq!(t("エラー: {error}", &[("error", &"x")]), "エラー: x");
        assert_eq!(sasacode_host::t!("エラー: {error}", error = "x"), "エラー: x");
    });
    with_lang(Lang::En, || {
        assert_eq!(t("エラー: {error}", &[("error", &"x")]), "Error: x");
        assert_eq!(sasacode_host::t!("エラー: {error}", error = 'x'), "Error: x");
        assert_eq!(t("not in any table", &[]), "not in any table");
        assert_eq!(sasacode_host::t!("not in any table"), "not in any table");
    });
}

#[test]
fn retranslate_finds_text_written_in_either_language() {
    let (ja, en) = TO_EN[0];
    let (en2, ja2) = TO_JA[0];
    with_lang(Lang::En, || {
        assert_eq!(retranslate(ja), en);
        assert_eq!(retranslate("unknown"), "unknown");
    });
    with_lang(Lang::Ja, || {
        assert_eq!(retranslate(en), ja);
        assert_eq!(retranslate(en2), ja2);
    });
}

#[test]
fn resolve_lang_sasacode_lang_then_the_setting_then_the_locale() {
    let mut env: HashMap<&str, &str> = HashMap::new();
    let resolve = |env: &HashMap<&str, &str>, setting: Option<&str>| {
        resolve_lang_with(setting, |k| env.get(k).map(|v| v.to_string()))
    };
    env.insert("SASACODE_LANG", "en");
    assert_eq!(resolve(&env, Some("ja")), Lang::En);
    env.remove("SASACODE_LANG");
    assert_eq!(resolve(&env, Some("en")), Lang::En);
    env.insert("LANG", "en_US.UTF-8");
    assert_eq!(resolve(&env, None), Lang::En);
    env.insert("LANG", "ja_JP.UTF-8");
    assert_eq!(resolve(&env, None), Lang::Ja);
    // An empty LC_ALL or LC_MESSAGES is unset: LANG says (#46).
    env.insert("LANG", "en_US.UTF-8");
    env.insert("LC_ALL", "");
    assert_eq!(resolve(&env, None), Lang::En);
    env.remove("LC_ALL");
    env.insert("LC_MESSAGES", "");
    assert_eq!(resolve(&env, None), Lang::En);
    env.insert("LC_ALL", "en_US");
    env.insert("LANG", "ja_JP.UTF-8");
    assert_eq!(resolve(&env, None), Lang::En);
    // Nothing said, C or POSIX: the product's own language.
    for locale in ["", "C", "C.UTF-8", "POSIX", "posix"] {
        assert_eq!(resolve(&HashMap::from([("LANG", locale)]), None), Lang::Ja, "{locale}");
    }
    assert_eq!(resolve(&HashMap::from([("LANG", "Ja")]), None), Lang::Ja);
    assert_eq!(resolve(&HashMap::from([("LANG", "CA_es")]), None), Lang::En);
    assert_eq!(resolve(&HashMap::from([("SASACODE_LANG", "EN")]), Some("ja")), Lang::En);
}
