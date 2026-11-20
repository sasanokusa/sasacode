//! Checks for promises this repository makes about itself that nothing else verifies: the core line
//! budget, the record of runtime dependencies, the READMEs staying in step with the CLI flags and the
//! bundled plugins, and the docs staying reachable and linked. `cargo test -p xtask` runs them
//! (`every_check_passes_in_this_repository`, so a change that breaks one fails CI), `cargo xtask
//! checks` prints them, and `cargo xtask improve` prints them next to the numbers the improvement
//! loop starts from (docs/improvement.md).
//!
//! Ported from scripts/checks.ts. The crates are read as text, never linked: a check needs no build
//! of what it looks at. The plugin API version check is gone with the public plugin API
//! (rust/DECISIONS.md ADR-002).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use crate::rust_source::{literal_spans, string_value, without_comments};

/// The core stays under this many lines (README,「開発」; rust/DECISIONS.md ADR-005). Re-measured
/// after the port on 2026-11-18: the code under src/ of the four core crates came to 12,045 lines
/// (sasacode-ai 3,887, sasacode-agent 4,501, sasacode-plugin-api 2,074, sasacode-tools 1,583; unit
/// tests, inline or in `*_tests.rs`, and tests/ do not count), rounded up to the next 500. The
/// TypeScript core's budget was 5,000. Raised to 12,720 on 2026-11-21 for stricter path and
/// permission checks (12,714 lines then).
pub const CORE_LINE_BUDGET: usize = 12_720;
/// A core line is at most this many characters wide, so the budget above measures size: 4,000 lines
/// were once kept by packing statements onto lines of 300 characters. A line that is mostly one
/// string literal may run past it (a tool description, a message for the model: prose that reads and
/// greps better whole, and that rustfmt cannot break either).
pub const CORE_LINE_WIDTH: usize = 120;
pub const CORE_CRATES: [&str; 4] = ["sasacode-ai", "sasacode-agent", "sasacode-plugin-api", "sasacode-tools"];
/// The crate of the `sasacode` binary: its dependencies, and those of every workspace crate it pulls
/// in, are what ships. The GUI (sasacode-gui, sasacode-protocol), the tests' crates and xtask are not.
pub const SHIPPED_FROM: &str = "sasacode-cli";
/// Both READMEs document the CLI and the bundled plugins; a change to one is a change to both.
pub const READMES: [&str; 2] = ["README.md", "README.en.md"];
/// Rows of the bundled-plugin table that are adapters, not entries of `sasacode_bundled::PLUGINS`.
const TABLE_ONLY: [&str; 2] = ["skills", "mcp"];
/// Directories that are not part of what a reader of the repository sees (bench/ stays local,
/// target/ is cargo's).
const SKIPPED: [&str; 6] = [".git", ".claude", "node_modules", "dist", "bench", "target"];

macro_rules! regex {
    ($pattern:expr) => {{
        static RE: LazyLock<Regex> = LazyLock::new(|| {
            // INFALLIBLE: the patterns are literals, and the tests run every one of them.
            Regex::new($pattern).expect("a valid pattern")
        });
        &*RE
    }};
}

/// The repository root (xtask's parent directory).
pub fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}

/// A file's text; an unreadable file reads as empty, invalid UTF-8 as U+FFFD.
fn read(path: &Path) -> String {
    fs::read(path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// `path` relative to `root`, with "/" on Windows too: the same place reads the same in every report.
fn relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every file under `dir` whose name `matches` accepts, following subdirectories, in name order.
pub fn walk(dir: &Path, matches: &dyn Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    let mut out = Vec::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            if !SKIPPED.contains(&name.as_str()) {
                out.extend(walk(&entry.path(), matches));
            }
        } else if matches(&name) {
            out.push(entry.path());
        }
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub struct CoreLines {
    pub total: usize,
    /// Per core crate, in the order of [`CORE_CRATES`].
    pub by_crate: Vec<(&'static str, usize)>,
}

/// A core crate's code: the `.rs` files under src/, without the unit-test files kept beside the code
/// (`*_tests.rs`, `tests.rs`).
fn core_sources(root: &Path, name: &str) -> Vec<PathBuf> {
    walk(&root.join("crates").join(name).join("src"), &|file| {
        file.ends_with(".rs") && !file.ends_with("_tests.rs") && file != "tests.rs"
    })
}

/// Lines as `wc -l` counts them (a last line without a newline still counts), without the unit tests
/// written inline: a `#[cfg(test)]` module at the top level (`#[cfg(test)]`, maybe more attributes,
/// `mod name {` … `}` at column 0) is not code the budget is about.
pub fn count_code_lines(text: &str) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    let (mut count, mut i) = (0, 0);
    while i < lines.len() {
        if lines[i] == "#[cfg(test)]" {
            let mut j = i + 1;
            while j < lines.len() && lines[j].starts_with("#[") {
                j += 1;
            }
            if j < lines.len() && lines[j].starts_with("mod ") && lines[j].ends_with('{') {
                let end = lines[j + 1..].iter().position(|l| *l == "}").map(|k| j + 1 + k);
                if let Some(end) = end {
                    i = end + 1;
                    continue;
                }
            }
        }
        count += 1;
        i += 1;
    }
    count
}

pub fn core_lines(root: &Path) -> CoreLines {
    let by_crate: Vec<(&'static str, usize)> = CORE_CRATES
        .iter()
        .map(|name| {
            (
                *name,
                core_sources(root, name)
                    .iter()
                    .map(|file| count_code_lines(&read(file)))
                    .sum(),
            )
        })
        .collect();
    CoreLines {
        total: by_crate.iter().map(|(_, n)| n).sum(),
        by_crate,
    }
}

/// A line that is mostly one string literal: its longest literal is longer than the rest of the line
/// (indentation aside). `spans` are the line's literals, from [`literal_spans`].
fn mostly_one_literal(line: &str, spans: &[(usize, usize)]) -> bool {
    let width = line.trim().chars().count();
    let longest = spans.iter().map(|(start, end)| end - start).max().unwrap_or(0);
    longest * 2 > width
}

/// Core lines wider than [`CORE_LINE_WIDTH`], as `crates/sasacode-ai/src/x.rs:12 (140 columns)`.
pub fn wide_core_lines(root: &Path) -> Vec<String> {
    let mut wide = Vec::new();
    for name in CORE_CRATES {
        for file in core_sources(root, name) {
            let text = read(&file);
            let spans = literal_spans(&text);
            for (i, line) in text.split('\n').enumerate() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                let columns = line.chars().count();
                if columns > CORE_LINE_WIDTH && !mostly_one_literal(line, spans.get(i).map_or(&[], Vec::as_slice)) {
                    wide.push(format!("{}:{} ({columns} columns)", relative(root, &file), i + 1));
                }
            }
        }
    }
    wide
}

/// The dependency names of a manifest's `[dependencies]` and `[target.….dependencies]` tables: what
/// the crate needs at run time (dev- and build-dependencies are not).
pub fn manifest_dependencies(manifest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_table = false;
    for raw in manifest.lines() {
        let line = raw.split(" #").next().unwrap_or(raw).trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            let header = header.trim_end_matches(']').trim();
            in_table = header == "dependencies" || (header.starts_with("target.") && header.ends_with(".dependencies"));
            // `[dependencies.foo]` and `[target.'cfg(unix)'.dependencies.foo]`: a table per dependency.
            let single = header.strip_prefix("dependencies.").or_else(|| {
                let rest = header.strip_prefix("target.")?;
                rest.split_once(".dependencies.").map(|(_, name)| name)
            });
            if let Some(name) = single {
                out.push(name.trim_matches(['"', '\'']).to_string());
            }
            continue;
        }
        if in_table && let Some((key, _)) = line.split_once('=') {
            // `foo = "1"`, `foo = { … }`, `foo.workspace = true`
            let name = key
                .trim()
                .split('.')
                .next()
                .unwrap_or_default()
                .trim_matches(['"', '\'']);
            if !name.is_empty() {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Runtime dependencies of the shipped crates (from [`SHIPPED_FROM`] through the workspace crates it
/// depends on), excluding the ones on each other.
pub fn runtime_dependencies(root: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut queue = vec![SHIPPED_FROM.to_string()];
    while let Some(krate) = queue.pop() {
        if !seen.insert(krate.clone()) {
            continue;
        }
        let manifest = root.join("crates").join(&krate).join("Cargo.toml");
        for name in manifest_dependencies(&read(&manifest)) {
            if name.starts_with("sasacode-") {
                queue.push(name);
            } else {
                names.insert(name);
            }
        }
    }
    names.into_iter().collect()
}

/// The first column of the tables in docs/dependencies.md, which record a reason for each one. Only a
/// name written as code counts (a table can opt out by writing its names plainly).
pub fn documented_dependencies(root: &Path) -> Vec<String> {
    let path = root.join("docs").join("dependencies.md");
    regex!(r"(?m)^\|\s*`([^`]+)`\s*\|")
        .captures_iter(&read(&path))
        .map(|m| m[1].to_string())
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOption {
    pub name: String,
    pub short: Option<char>,
}

/// The options the CLI offers: the ones `OPTIONS` in crates/sasacode-cli/src/main.rs parses, plus the
/// ones only `HELP_TEXT` lists. Read from the source so the check needs no build.
pub fn cli_options(root: &Path) -> Vec<CliOption> {
    let path = root.join("crates").join("sasacode-cli").join("src").join("main.rs");
    if !path.exists() {
        return Vec::new();
    }
    let source = without_comments(&read(&path));
    let mut options: Vec<CliOption> = Vec::new();
    let mut add = |name: &str, short: Option<char>| {
        if !options.iter().any(|o| o.name == name) {
            options.push(CliOption {
                name: name.to_string(),
                short,
            });
        }
    };
    let table = regex!(r"(?s)const\s+OPTIONS\s*:\s*&(?:'static\s+)?\[OptionSpec\]\s*=\s*&\[(.*?)\]\s*;");
    if let Some(block) = table.captures(&source) {
        // `string("print", Some('p'))`, `boolean("stdin", None)`, `OptionSpec { name: "output", … }`
        let entry = regex!(
            r#"\b(?:string|boolean)\(\s*"([a-z][a-z0-9-]*)"\s*,\s*(?:Some\('([A-Za-z0-9_])'\)|None)\s*\)|\bOptionSpec\s*\{([^}]*)\}"#
        );
        for m in entry.captures_iter(&block[1]) {
            if let Some(name) = m.get(1) {
                add(name.as_str(), first_char(m.get(2)));
            } else if let Some(fields) = m.get(3) {
                let fields = fields.as_str();
                if let Some(name) = regex!(r#"\bname\s*:\s*"([a-z][a-z0-9-]*)""#).captures(fields) {
                    let short = regex!(r"\bshort\s*:\s*Some\('([A-Za-z0-9_])'\)").captures(fields);
                    add(&name[1], first_char(short.and_then(|s| s.get(1))));
                }
            }
        }
    }
    let help = regex!(r"const\s+HELP_TEXT\s*:\s*&(?:'static\s+)?str\s*=\s*")
        .find(&source)
        .and_then(|m| string_value(&source[m.end()..]))
        .unwrap_or_default();
    for m in regex!(r"(?m)^\s+(?:-([A-Za-z0-9_]), )?--([a-z][a-z0-9-]*)").captures_iter(&help) {
        add(&m[2], first_char(m.get(1)));
    }
    options
}

fn first_char(m: Option<regex::Match<'_>>) -> Option<char> {
    m.and_then(|m| m.as_str().chars().next())
}

/// Whether `-x` stands in `text` as an option: after the start, a space or one of `` ` | ( / ``, and
/// not followed by a letter, digit, `_` or `-`.
fn mentions_short(text: &str, short: char) -> bool {
    let flag = format!("-{short}");
    text.match_indices(&flag).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + flag.len()..].chars().next();
        let starts = before.is_none_or(|c| c.is_whitespace() || "`|(/".contains(c));
        let ends = after.is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        starts && ends
    })
}

/// Options `text` never mentions, counting `-c` as documenting `--continue` (the tables use shorts).
pub fn undocumented_options(text: &str, options: &[CliOption]) -> Vec<String> {
    options
        .iter()
        .filter(|option| {
            if text.contains(&format!("--{}", option.name)) {
                return false;
            }
            !option.short.is_some_and(|short| mentions_short(text, short))
        })
        .map(|option| format!("--{}", option.name))
        .collect()
}

/// The names in `PLUGINS` of crates/sasacode-bundled/src/lib.rs (`on("todo", …)`, `off("ask-user",
/// …)`), in load order: every plugin that ships, on by default or not (ADR-011).
pub fn bundled_plugins(root: &Path) -> Vec<String> {
    let path = root.join("crates").join("sasacode-bundled").join("src").join("lib.rs");
    let source = without_comments(&read(&path));
    let table = regex!(r"(?s)const\s+PLUGINS\s*:\s*&(?:'static\s+)?\[Bundled\]\s*=\s*&\[(.*?)\]\s*;");
    let Some(block) = table.captures(&source) else {
        return Vec::new();
    };
    regex!(r#"\b(?:on|off)\(\s*"([^"]+)"|\bname\s*:\s*"([^"]+)""#)
        .captures_iter(&block[1])
        .filter_map(|m| m.get(1).or_else(|| m.get(2)).map(|n| n.as_str().to_string()))
        .collect()
}

/// Table names in `text` under `heading`, reading `` `a` / `b` `` rows as two names.
pub fn table_names(text: &str, heading: &str) -> Vec<String> {
    let Some(at) = text.find(heading) else {
        return Vec::new();
    };
    let rest = &text[at + heading.len()..];
    let section = regex!(r"(?m)^#{2,3} ")
        .find(rest)
        .map_or(rest, |end| &rest[..end.start()]);
    regex!(r"(?m)^\|\s*([^|]+?)\s*\|")
        .captures_iter(section)
        .map(|m| m[1].trim().to_string())
        .filter(|cell| !cell.is_empty() && !cell.chars().all(|c| c == '-') && cell != "名前" && cell != "Name")
        .flat_map(|cell| {
            cell.split('/')
                .map(|part| part.replace('`', "").trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The skeleton of a README: heading levels and opening code fences, in order.
pub fn readme_structure(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open = false;
    for line in text.split('\n') {
        if let Some(fence) = regex!(r"^```(\S*)\s*$").captures(line) {
            if !open {
                out.push(format!("code:{}", &fence[1]));
            }
            open = !open;
            continue;
        }
        if open {
            continue; // a heading-looking line inside a block is an example, not a heading
        }
        if let Some(heading) = regex!(r"^(#{1,6}) ").captures(line) {
            out.push(format!("h{}", heading[1].len()));
        }
    }
    out
}

fn read_if_exists(path: &Path) -> Option<String> {
    path.exists().then(|| read(path))
}

/// Where the two READMEs stop having the same shape, if they do.
pub fn readme_shape_problem(root: &Path) -> Vec<String> {
    let [ja, en] = READMES.map(|name| read_if_exists(&root.join(name)).filter(|t| !t.is_empty()));
    let (Some(ja), Some(en)) = (ja, en) else {
        return Vec::new();
    };
    let (a, b) = (readme_structure(&ja), readme_structure(&en));
    if a.len() != b.len() {
        return vec![format!(
            "README.md has {} headings and code blocks, README.en.md has {}",
            a.len(),
            b.len()
        )];
    }
    match a.iter().zip(&b).position(|(x, y)| x != y) {
        Some(at) => vec![format!(
            "the two READMEs part ways at block {}: {} vs {}",
            at + 1,
            a[at],
            b[at]
        )],
        None => Vec::new(),
    }
}

/// Relative links in the markdown that point at nothing.
pub fn broken_links(root: &Path) -> Vec<String> {
    let mut broken = Vec::new();
    for file in walk(root, &|name| name.ends_with(".md")) {
        // Fenced blocks and code spans are examples, not prose: a link-looking string inside one is
        // not a link.
        let text = read(&file);
        let text = regex!(r"(?s)```.*?```").replace_all(&text, "");
        let text = regex!(r"`[^`\n]*`").replace_all(&text, "").into_owned();
        let dir = file.parent().unwrap_or(root);
        for m in regex!(r"\[[^\]]*\]\(([^)\s]+)\)").captures_iter(&text) {
            let target = &m[1];
            if regex!(r"(?i)^[a-z][a-z0-9+.-]*:").is_match(target) || target.starts_with(['#', '/']) {
                continue;
            }
            let path = target.split('#').next().unwrap_or_default();
            let path = percent_encoding::percent_decode_str(path).decode_utf8_lossy();
            if !path.is_empty() && !dir.join(path.as_ref()).exists() {
                broken.push(format!("{}: {target}", relative(root, &file)));
            }
        }
    }
    broken
}

/// Docs in docs/ that not both READMEs link to: written down, but found by nobody.
pub fn unreachable_docs(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root.join("docs")) else {
        return Vec::new();
    };
    let readmes = READMES.map(|name| read(&root.join(name)));
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".md"))
        .collect();
    names.sort();
    names
        .into_iter()
        .filter(|name| !readmes.iter().all(|text| text.contains(name.as_str())))
        .map(|name| format!("docs/{name}"))
        .collect()
}

#[derive(Debug)]
pub struct CheckResult {
    /// Short name, shown by `cargo xtask improve`.
    pub name: &'static str,
    /// What the check keeps true, in one line.
    pub promise: String,
    /// What is wrong; empty means the check passed.
    pub problems: Vec<String>,
}

/// `problems`, unless the check read nothing to compare (`found` is false): a source it reads that
/// moved or changed its shape would otherwise pass it quietly.
fn unless_empty(found: bool, nothing: &str, problems: Vec<String>) -> Vec<String> {
    if found {
        problems
    } else {
        vec![format!(
            "read nothing from {nothing}: the check no longer sees its source"
        )]
    }
}

pub fn run_checks(root: &Path) -> Vec<CheckResult> {
    let lines = core_lines(root);
    let used = runtime_dependencies(root);
    let documented = documented_dependencies(root);
    let options = cli_options(root);
    let plugins = bundled_plugins(root);
    let readmes: Vec<(&str, String)> = READMES.iter().map(|name| (*name, read(&root.join(name)))).collect();

    vec![
        CheckResult {
            name: "core-line-budget",
            promise: format!(
                "the core ({}) stays under {CORE_LINE_BUDGET} lines",
                CORE_CRATES.join(", ")
            ),
            problems: unless_empty(
                lines.total > 0,
                "crates/<core crate>/src/*.rs",
                if lines.total > CORE_LINE_BUDGET {
                    vec![format!(
                        "the core is {} lines, {} over the budget of {CORE_LINE_BUDGET}",
                        lines.total,
                        lines.total - CORE_LINE_BUDGET
                    )]
                } else {
                    Vec::new()
                },
            ),
        },
        CheckResult {
            name: "core-line-width",
            promise: format!(
                "no core line is wider than {CORE_LINE_WIDTH} columns, a line that is mostly one string literal aside"
            ),
            problems: wide_core_lines(root)
                .into_iter()
                .map(|at| format!("{at} is over {CORE_LINE_WIDTH} columns"))
                .collect(),
        },
        CheckResult {
            name: "dependency-record",
            promise: "every runtime dependency of a shipped crate has a row with a reason in docs/dependencies.md"
                .into(),
            problems: unless_empty(
                !used.is_empty(),
                "crates/sasacode-cli/Cargo.toml and the crates it depends on",
                used.iter()
                    .filter(|name| !documented.contains(name))
                    .map(|name| format!("{name} is used but has no row in docs/dependencies.md"))
                    .chain(
                        documented
                            .iter()
                            .filter(|name| !used.contains(name))
                            .map(|name| format!("{name} has a row in docs/dependencies.md but nothing depends on it")),
                    )
                    .collect(),
            ),
        },
        CheckResult {
            name: "readme-cli",
            promise: "both READMEs mention every option the CLI offers".into(),
            problems: unless_empty(
                !options.is_empty(),
                "OPTIONS and HELP_TEXT in crates/sasacode-cli/src/main.rs",
                readmes
                    .iter()
                    .flat_map(|(name, text)| {
                        undocumented_options(text, &options)
                            .into_iter()
                            .map(move |option| format!("{name} never mentions {option}"))
                    })
                    .collect(),
            ),
        },
        CheckResult {
            name: "readme-plugins",
            promise: "both READMEs list exactly the plugins that ship with sasacode".into(),
            problems: unless_empty(
                !plugins.is_empty(),
                "PLUGINS in crates/sasacode-bundled/src/lib.rs",
                readmes
                    .iter()
                    .flat_map(|(name, text)| {
                        let heading = if *name == "README.md" {
                            "### 同梱プラグイン"
                        } else {
                            "### Bundled plugins"
                        };
                        let listed = table_names(text, heading);
                        let missing = plugins
                            .iter()
                            .filter(|plugin| !listed.contains(plugin))
                            .map(|plugin| format!("{name} does not list the bundled plugin {plugin}"));
                        let extra = listed
                            .iter()
                            .filter(|listed| !plugins.contains(listed) && !TABLE_ONLY.contains(&listed.as_str()))
                            .map(|listed| format!("{name} lists {listed}, which ships with nobody"));
                        missing.chain(extra).collect::<Vec<_>>()
                    })
                    .collect(),
            ),
        },
        CheckResult {
            name: "readme-shape",
            promise: "the two READMEs keep the same shape, heading by heading and block by block".into(),
            problems: readme_shape_problem(root),
        },
        CheckResult {
            name: "docs-links",
            promise: "relative links in the markdown resolve".into(),
            problems: broken_links(root)
                .into_iter()
                .map(|link| format!("broken link {link}"))
                .collect(),
        },
        CheckResult {
            name: "docs-reachable",
            promise: "both READMEs link every doc in docs/".into(),
            problems: unreachable_docs(root)
                .into_iter()
                .map(|doc| format!("{doc} is linked from no README"))
                .collect(),
        },
    ]
}

#[cfg(test)]
#[path = "checks_tests.rs"]
mod tests;
