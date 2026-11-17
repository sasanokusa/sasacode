//! The repository's own checks, plus tests of the checks themselves: a check that silently stops
//! working is worse than no check. The repository-wide case is the one that fails CI.
//!
//! Ported from scripts/checks.test.ts. Dropped with the `api-version` check (rust/DECISIONS.md
//! ADR-002): "the API versions are compared as numbers, not as strings" and "a README that never
//! states the API version is reported".

use super::*;

/// A throwaway repository root holding `files`, keyed by path relative to it.
fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("sasacode-check-")
        .tempdir()
        .expect("a temporary directory");
    for (path, body) in files {
        let path = root.path().join(path);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the fixture's directories");
        fs::write(path, body).expect("a fixture file");
    }
    root
}

fn problems_of(root: &Path, check: &str) -> Vec<String> {
    run_checks(root)
        .into_iter()
        .find(|c| c.name == check)
        .map(|c| c.problems)
        .unwrap_or_else(|| panic!("no check named {check}"))
}

#[test]
fn every_check_passes_in_this_repository() {
    let problems: Vec<String> = run_checks(&root())
        .into_iter()
        .flat_map(|check| {
            check
                .problems
                .into_iter()
                .map(move |problem| format!("{}: {problem}", check.name))
        })
        .collect();
    assert_eq!(problems, Vec::<String>::new());
}

#[test]
fn a_check_whose_source_moved_fails_instead_of_passing_quietly() {
    let root = fixture(&[("README.md", "# a\n"), ("README.en.md", "# a\n")]);
    let nothing = |check: &str| {
        problems_of(root.path(), check)
            .first()
            .is_some_and(|p| p.starts_with("read nothing"))
    };
    assert!(nothing("core-line-budget"));
    assert!(nothing("dependency-record"));
    assert!(nothing("readme-cli"));
    assert!(nothing("readme-plugins"));
    // A main.rs whose table was renamed reads as nothing too, not as "every option documented".
    let renamed = fixture(&[(
        "crates/sasacode-cli/src/main.rs",
        "const FLAGS: &[OptionSpec] = &[string(\"print\", None)];\n",
    )]);
    assert!(problems_of(renamed.path(), "readme-cli")[0].starts_with("read nothing"));
}

#[test]
fn count_code_lines_counts_like_wc_and_leaves_out_inline_tests() {
    assert_eq!(count_code_lines(""), 0);
    assert_eq!(count_code_lines("a\n"), 1);
    assert_eq!(count_code_lines("a\nb"), 2);
    assert_eq!(count_code_lines("a\nb\n"), 2);
    let with_tests =
        "fn a() {}\n\n#[cfg(test)]\n#[allow(unused)]\nmod tests {\n    #[test]\n    fn t() {}\n}\nfn b() {}\n";
    assert_eq!(count_code_lines(with_tests), 3);
    // A test module kept in its own file is declared in one line, which counts.
    assert_eq!(
        count_code_lines("#[cfg(test)]\n#[path = \"x_tests.rs\"]\nmod tests;\n"),
        3
    );
}

#[test]
fn wide_core_lines_reports_code_and_comments_past_120_columns_but_not_a_line_of_mostly_one_string() {
    let long = "x".repeat(130);
    let lines = [
        // Code packed around a short literal.
        format!(
            "let a = \"{}\"; let total = values.iter().map(|value| value * 2).filter(|value| value % 3 == 0).sum::<i64>();",
            "x".repeat(40)
        ),
        format!("// {long}"),
        format!("        \"{long}\","),
        format!("    return r#\"{long}\"#;"),
        format!("        description: \"{long}\".into(),"),
        // A long string in a comment is still a comment.
        format!("    let s = \"short\"; // \"{long}\""),
        // Many short literals are not one long one.
        concat!(
            r#"    json!({ "id": id, "type": "function", "function": { "name": name, "description": description, "#,
            r#""arguments": arguments } })"#
        )
        .to_string(),
        // The inside of a literal that runs over several lines.
        "    let text = \"first line".to_string(),
        long.clone(),
        "\";".to_string(),
        // 63 characters, 183 bytes: columns are characters.
        format!("// {}", "日本語".repeat(20)),
        "let short = 1;".to_string(),
    ];
    let source = lines.join("\n");
    let root = fixture(&[
        ("crates/sasacode-ai/src/a.rs", &source),
        ("crates/sasacode-ai/tests/t.rs", &format!("let a = {long};\n")),
        ("crates/sasacode-cli/src/not_core.rs", &format!("let a = {long};\n")),
    ]);
    let at = |i: usize| {
        format!(
            "crates/sasacode-ai/src/a.rs:{} ({} columns)",
            i + 1,
            lines[i].chars().count()
        )
    };
    assert_eq!(wide_core_lines(root.path()), vec![at(0), at(1), at(5), at(6)]);
    assert!(
        lines[6].chars().count() > CORE_LINE_WIDTH,
        "the json! line must be over the width"
    );
}

#[test]
fn core_lines_adds_up_src_rs_of_the_core_crates_subdirectories_included() {
    let root = fixture(&[
        ("crates/sasacode-ai/src/a.rs", "one\ntwo\nthree\n"),
        ("crates/sasacode-ai/src/nested/b.rs", "one\ntwo"), // no trailing newline: still two lines
        ("crates/sasacode-agent/src/c.rs", "one\n"),
        ("crates/sasacode-tools/src/d.rs", "one\ntwo\nthree\nfour\n"),
        ("crates/sasacode-ai/tests/a.rs", "one\ntwo\nthree\nfour\nfive\n"), // tests/ is not core
        ("crates/sasacode-ai/src/notes.md", "one\n"),
        ("crates/sasacode-cli/src/main.rs", "one\n"),
    ]);
    assert_eq!(
        core_lines(root.path()),
        CoreLines {
            total: 10,
            by_crate: vec![
                ("sasacode-ai", 5),
                ("sasacode-agent", 1),
                ("sasacode-plugin-api", 0),
                ("sasacode-tools", 4)
            ],
        }
    );
}

#[test]
fn runtime_dependencies_follow_the_shipped_crates_and_skip_the_ones_on_each_other() {
    let cli = r#"[package]
name = "sasacode-cli"
version.workspace = true

[dependencies]
sasacode-ai.workspace = true
clap.workspace = true
serde = { workspace = true, features = ["derive"] } # a comment
# commented = "1"

[target.'cfg(unix)'.dependencies]
nix = { version = "0.31", default-features = false }

[target.'cfg(windows)'.dependencies.windows-sys]
version = "0.59"

[dev-dependencies]
tempfile.workspace = true

[target.'cfg(unix)'.dev-dependencies]
rustix = "1"

[build-dependencies]
cc = "1"

[[bin]]
name = "sasacode"
path = "src/main.rs"
"#;
    let ai = "[dependencies]\nreqwest.workspace = true\n\"hyper\" = \"1\"\nserde.workspace = true\n\n\
              [dependencies.sasacode-plugin-api]\nworkspace = true\n";
    let root = fixture(&[
        ("crates/sasacode-cli/Cargo.toml", cli),
        ("crates/sasacode-ai/Cargo.toml", ai),
        (
            "crates/sasacode-plugin-api/Cargo.toml",
            "[dependencies]\ntokio.workspace = true\n",
        ),
        // Not reachable from sasacode-cli: the GUI is not part of the release.
        ("crates/sasacode-gui/Cargo.toml", "[dependencies]\niced = \"0.14\"\n"),
    ]);
    assert_eq!(
        runtime_dependencies(root.path()),
        ["clap", "hyper", "nix", "reqwest", "serde", "tokio", "windows-sys"]
    );
    let doc = "| `serde` | where | why | instead |\n| --- | --- | --- | --- |\n| plain | not code, not counted | | |\n";
    assert_eq!(
        documented_dependencies(fixture(&[("docs/dependencies.md", doc)]).path()),
        ["serde"]
    );
}

#[test]
fn the_dependency_record_reports_both_directions() {
    let root = fixture(&[
        (
            "crates/sasacode-cli/Cargo.toml",
            "[dependencies]\nclap = \"4\"\nregex = \"1\"\n",
        ),
        (
            "docs/dependencies.md",
            "| `clap` | cli | why | - |\n| `openai` | ai | why | - |\n",
        ),
    ]);
    assert_eq!(
        problems_of(root.path(), "dependency-record"),
        [
            "regex is used but has no row in docs/dependencies.md",
            "openai has a row in docs/dependencies.md but nothing depends on it",
        ]
    );
}

#[test]
fn a_readme_that_misses_a_flag_is_reported() {
    let main = r#"const HELP_TEXT: &str = "sasacode
Options:
  -p, --print <prompt>           headless (\"quoted\")
      --max-turns <n>            turns
  -h, --help";

const OPTIONS: &[OptionSpec] = &[
    string("print", Some('p')),
    OptionSpec { name: "max-turns", kind: Kind::String, short: None, default: Some("200") },
    boolean("help", Some('h')),
    // boolean("commented-out", None),
    boolean("trust-project", None),
];
"#;
    let root = fixture(&[
        ("crates/sasacode-cli/src/main.rs", main),
        (
            "README.md",
            "| `-p <prompt>` | headless |\n| `--max-turns <n>` | turns |\n",
        ),
        (
            "README.en.md",
            "| `-p <prompt>` | headless |\n| `--max-turns` | turns |\n| `-h` | help |\n",
        ),
    ]);
    let options = cli_options(root.path());
    let mut names: Vec<&str> = options.iter().map(|o| o.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["help", "max-turns", "print", "trust-project"]);
    assert_eq!(
        options.iter().find(|o| o.name == "print").and_then(|o| o.short),
        Some('p')
    );
    assert_eq!(
        problems_of(root.path(), "readme-cli"),
        [
            "README.md never mentions --help",
            "README.md never mentions --trust-project",
            "README.en.md never mentions --trust-project",
        ]
    );
}

#[test]
fn cli_options_add_the_flags_only_the_help_lists() {
    let main = "const HELP_TEXT: &str = \"Options:\n  -x, --extra <v>   only in the help\n      --print \\\n\
                \x20 continued\n  sasacode endpoint add <name> [--project]\";\n\
                const OPTIONS: &[OptionSpec] = &[string(\"print\", Some('p'))];\n";
    let root = fixture(&[("crates/sasacode-cli/src/main.rs", main)]);
    assert_eq!(
        cli_options(root.path()),
        [
            CliOption {
                name: "print".into(),
                short: Some('p')
            },
            CliOption {
                name: "extra".into(),
                short: Some('x')
            },
        ]
    );
}

#[test]
fn the_bundled_plugins_are_read_from_the_plugins_table_on_or_off() {
    let lib = r#"pub const PLUGINS: &[Bundled] = &[
    on("tool-repair", tool_repair::plugin),
    // After tool-repair: on("not-a-plugin", x)
    off("ask-user", ask_user::plugin),
    Bundled { name: "custom", on_by_default: true, init: custom::plugin },
];

pub fn find(name: &str) -> Option<&'static Bundled> { on("elsewhere", f) }
"#;
    let table = "| 名前 | 内容 |\n| --- | --- |\n| `tool-repair` | a |\n| `ask-user` | b |\n| `custom` | c |\n\
                 | `skills` / `mcp` | adapters |\n";
    let readme = format!("### 同梱プラグイン（`plugins.disabled` で外せる）\n\n{table}\n## Next\n| `after` | x |\n");
    let root = fixture(&[
        ("crates/sasacode-bundled/src/lib.rs", lib),
        ("README.md", &readme),
        (
            "README.en.md",
            "### Bundled plugins\n\n| Name | What |\n| --- | --- |\n| `tool-repair` | a |\n| `gone` | b |\n",
        ),
    ]);
    assert_eq!(bundled_plugins(root.path()), ["tool-repair", "ask-user", "custom"]);
    assert_eq!(
        problems_of(root.path(), "readme-plugins"),
        [
            "README.en.md does not list the bundled plugin ask-user",
            "README.en.md does not list the bundled plugin custom",
            "README.en.md lists gone, which ships with nobody",
        ]
    );
}

#[test]
fn the_bundled_plugin_table_reads_names_including_a_slash_b_rows() {
    let text = [
        "### Bundled plugins",
        "",
        "| Name | What |",
        "| --- | --- |",
        "| `todo` | plan |",
        "| `skills` / `mcp` | adapters |",
        "",
        "### MCP",
        "| `other` | x |",
    ]
    .join("\n");
    assert_eq!(table_names(&text, "### Bundled plugins"), ["todo", "skills", "mcp"]);
}

#[test]
fn readme_structure_sees_headings_and_opening_fences_and_ignores_both_inside_a_block() {
    let text = [
        "# Title",
        "## One",
        "```bash",
        "# not a heading",
        "```",
        "### Two",
        "```",
        "```",
        "#### Three",
    ]
    .join("\n");
    assert_eq!(readme_structure(&text), ["h1", "h2", "code:bash", "h3", "code:", "h4"]);
}

#[test]
fn a_readme_that_drifts_shape_from_the_other_is_reported() {
    let ja = ("README.md", "# a\n## b\n```sh\nx\n```\n");
    assert_eq!(
        readme_shape_problem(fixture(&[ja, ("README.en.md", "# A\n## B\n```sh\ny\n```\n")]).path()),
        Vec::<String>::new()
    );
    assert_eq!(
        readme_shape_problem(fixture(&[ja, ("README.en.md", "# A\n```sh\ny\n```\n")]).path()),
        ["README.md has 3 headings and code blocks, README.en.md has 2"]
    );
    assert_eq!(
        readme_shape_problem(fixture(&[ja, ("README.en.md", "# A\n### B\n```sh\ny\n```\n")]).path()),
        ["the two READMEs part ways at block 2: h2 vs h3"]
    );
}

#[test]
fn a_broken_relative_link_and_an_unlinked_doc_are_reported() {
    let root = fixture(&[
        (
            "README.md",
            "[a](docs/here.md) [b](docs/gone.md) [c](https://example.com/missing) [d](#anchor) [e](docs/here.md#part) \
             [f](notes/with%20space.md) `[h](in-a-code-span.md)` [`i`](docs/here.md)\n```\n[g](docs/in-a-fence.md)\n```\n",
        ),
        ("README.en.md", "[a](docs/here.md)\n"),
        ("docs/here.md", "[up](../README.md)\n"),
        ("notes/with space.md", "[up](../README.md)\n"),
        ("docs/orphan.md", "# nothing links to me\n"),
        // cargo's output is not part of the repository a reader sees.
        ("target/doc/x.md", "[broken](nowhere.md)\n"),
    ]);
    assert_eq!(broken_links(root.path()), ["README.md: docs/gone.md"]);
    assert_eq!(unreachable_docs(root.path()), ["docs/orphan.md"]);
}

#[test]
fn an_option_with_a_short_form_counts_as_documented_when_the_readme_uses_the_short_form() {
    let option = |name: &str, short: Option<char>| CliOption {
        name: name.into(),
        short,
    };
    let options = [
        option("continue", Some('c')),
        option("resume", Some('r')),
        option("no-session", None),
    ];
    assert_eq!(
        undocumented_options("| `-c` / `-r <id>` / `--no-session` |", &options),
        Vec::<String>::new()
    );
    let options = [option("version", Some('v')), option("help", Some('h'))];
    assert_eq!(undocumented_options("sasacode --version", &options), ["--help"]);
    // `-h` inside a word or a longer flag is not the option.
    assert_eq!(
        undocumented_options("a-h or -hx or --h-thing", &[option("help", Some('h'))]),
        ["--help"]
    );
}
