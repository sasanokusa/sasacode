//! `sasacode` (`packages/cli/src/main.ts`): arguments, the environment, the trust check, then the
//! TUI or a headless run.

use std::io::IsTerminal;
use std::path::Path;

use sasacode_agent::permission::{PERMISSION_MODES, PermissionPolicy};
use sasacode_cli::args::{OptionSpec, boolean, parse_args, string, strings};
use sasacode_cli::config::{read_config_files, sasacode_home};
use sasacode_cli::control::control_mode;
use sasacode_cli::env::{blank_keys, ensure_env_file, parse_env_file};
use sasacode_cli::headless::{HeadlessOptions, OutputFormat, run_headless};
use sasacode_cli::setup::{SetupOptions, read_prompt_file, setup};
use sasacode_cli::stdin::read_piped_stdin;
use sasacode_cli::trust::{ask_trust, assess_project, is_trusted, save_trust};
use sasacode_cli::{subcommands, tui};
use sasacode_host::{builtin_providers, resolve_lang, set_lang, t};
use sasacode_plugin_api::{PermissionMode, PermissionRules, hide_from_children};

const HELP_TEXT: &str = "sasacode — a small, pluggable coding agent

Usage:
  sasacode [prompt]              interactive session (optional first prompt)
  sasacode -p \"<prompt>\"         run headless and print the answer

Options:
  -p, --print <prompt>           headless mode (stdin is appended when piped)
      --stdin                    with -p: wait for piped input however long it takes
      --output <text|jsonl>      headless output format (default text)
      --control stdio            with -p --output jsonl: the parent answers approvals, sends messages and can abort
                                 over stdin as JSON Lines (stdin is then not appended to the prompt)
  -m, --model <provider/model>   e.g. anthropic/claude-opus-5, openai/gpt-5.5, ollama/gemma4:e4b
                                 (the TUI's /model lists what your providers offer)
      --permission <mode>        {modes} (default edits)
      --thinking <level>         off | low | medium | high | xhigh | max
      --max-turns <n>            turns per run before stopping (default 200; 0 = no limit)
  -c, --continue                 resume the most recent session in this directory
  -r, --resume <id>              resume a session by id
      --no-session               do not save this session
      --append-system-prompt-file <path>
                                 add the file's text to the system prompt, after the config's instructions
      --trust-project            trust this project's endpoints, MCP servers, plugin settings and
                                 permission settings without asking
      --allow <rule>             allow calls matching a permission rule for this run, as answering \"always\" does
                                 (repeatable; e.g. \"bash(npm test)\", the rules of approval_request)
      --slash                    with -p: a prompt that starts with /<command> runs that plugin command

Subcommands:
  sasacode endpoint add <name> <url> [--key-env VAR] [--project]   add a server (format detected)
  sasacode endpoint remove <name> [--project]
  sasacode endpoint list
  sasacode list models|commands|skills [--json]   what a run here can use (the GUI asks with --json)
  sasacode trust [--yes] [--json]   is this folder trusted, and what would trust enable; --yes trusts it
  sasacode plugin list --json       the bundled plugins and whether each is on (the GUI asks)
  sasacode login [status|logout]    use a ChatGPT plan instead of an API key (openai-codex/… models)
  sasacode update [--check] [tag]   download and install the newest release (checksum-verified)
  sasacode web-search install       install RustXNG for the web_search tool (checksum-verified)
  sasacode gui [folder]             open the desktop GUI on a folder (sasacode gui install puts it in)
  sasacode gui install [--path <dir>]   build the desktop GUI on this machine with Rust (docs/gui.md)
  -h, --help
  -v, --version";

/// Translated when shown, not when this loads: main sets the language first.
fn help() -> String {
    let modes = PERMISSION_MODES
        .iter()
        .map(|m| m.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    t!(HELP_TEXT, modes = modes)
}

const OPTIONS: &[OptionSpec] = &[
    string("print", Some('p')),
    OptionSpec {
        name: "output",
        kind: sasacode_cli::args::Kind::String,
        short: None,
        default: Some("text"),
    },
    string("control", None),
    string("append-system-prompt-file", None),
    string("model", Some('m')),
    string("permission", None),
    string("thinking", None),
    string("max-turns", None),
    boolean("continue", Some('c')),
    string("resume", Some('r')),
    boolean("no-session", None),
    boolean("trust-project", None),
    strings("allow", None),
    boolean("slash", None),
    boolean("stdin", None),
    boolean("help", Some('h')),
    boolean("version", Some('v')),
];

fn main() {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".into());
    // One language everywhere: SASACODE_LANG, else the `lang` setting, else the system locale.
    let lang = read_config_files(Path::new(&cwd))
        .ok()
        .and_then(|f| f.global.get("lang").and_then(|v| v.as_str()).map(str::to_string));
    set_lang(resolve_lang(lang.as_deref()));

    // The environment is changed here, before any other thread exists, and nowhere else: setting
    // it is only sound while the process is single-threaded (rust/DECISIONS.md ADR-012).
    let providers = builtin_providers();
    // An empty key variable (exported blank by a shell profile) must not shadow a real one.
    for name in blank_keys(providers, |n| std::env::var(n).ok()) {
        // SAFETY: no other thread runs yet (the runtime starts below), so nothing reads the
        // environment concurrently.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var(&name)
        };
    }
    let env_path = sasacode_home().join(".env");
    let created = ensure_env_file(&env_path, providers).unwrap_or(false);
    // Keys set only in ~/.sasacode/.env are for sasacode: the commands it runs do not get them.
    let loaded = parse_env_file(&env_path, |n| std::env::var_os(n).is_some_and(|v| !v.is_empty()));
    for (name, value) in &loaded {
        // SAFETY: as above, still single-threaded.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(name, value)
        };
    }
    hide_from_children(loaded.into_iter().map(|(n, _)| n));
    // An Anthropic request without any key falls back on the `ant auth login` profile, as the SDK did.
    sasacode_ai::transport::set_default_credentials(Some(sasacode_cli::anthropic_profile::credentials()));
    if created && std::io::stderr().is_terminal() {
        eprintln!(
            "{}",
            t!("created {path}: add your API key there", path = env_path.display())
        );
    }

    sasacode_ai::http::init_tls();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", t!("sasacode: {error}", error = e));
            std::process::exit(1);
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = runtime.block_on(async move {
        match run(args, cwd).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!(
                    "{}",
                    t!("sasacode: {error}", error = sasacode_host::plain(&format!("{e:#}")))
                );
                1
            }
        }
    });
    std::process::exit(code);
}

async fn run(args: Vec<String>, cwd: String) -> anyhow::Result<i32> {
    match args.first().map(String::as_str) {
        // All that is left of `sasacode plugin`: the bundled plugins, for the GUI.
        Some("plugin") if args[1..] == ["list", "--json"] => {
            let listed = sasacode_cli::listings::plugin_listing(Path::new(&cwd))?;
            println!("{}", serde_json::to_string(&listed)?);
            return Ok(0);
        }
        Some("plugin") => anyhow::bail!(t!(
            "`sasacode plugin` was removed in 0.20: the plugins now ship with sasacode (turn the optional ones on with plugins.enabled in config.json); add tools with MCP servers or Skills"
        )),
        Some("endpoint") => return sasacode_cli::endpoints::endpoint_command(&args[1..], Path::new(&cwd)).await,
        Some("login") => return subcommands::login(args.get(1).map(String::as_str)).await,
        Some("update") => return sasacode_cli::update::update_command(&args[1..]).await,
        Some("web-search") => return subcommands::web_search(&args[1..]).await,
        Some("gui") => return subcommands::gui(&args[1..]).await,
        Some("list") => return sasacode_cli::listings::list_command(&args[1..], Path::new(&cwd)).await,
        Some("trust") => return sasacode_cli::listings::trust_command(&args[1..], Path::new(&cwd)),
        _ => {}
    }
    let values = parse_args(&args, OPTIONS).map_err(anyhow::Error::msg)?;
    if values.flag("help") {
        println!("{}", help());
        return Ok(0);
    }
    if values.flag("version") {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    let print = values.string("print").map(str::to_string);
    let output = values.string("output").map(str::to_string);
    // Checked before anything is loaded: a wrong combination should fail at once, not after the setup.
    let control = control_mode(values.string("control"), print.as_deref(), output.as_deref())?;
    if values.flag("slash") && print.is_none() {
        anyhow::bail!(t!("--slash needs -p"));
    }
    let allow = values.strings("allow").to_vec();
    for rule in &allow {
        let rules = PermissionRules {
            allow: vec![rule.clone()],
            ..Default::default()
        };
        if PermissionPolicy::new(PermissionMode::Edits, &rules).is_err() {
            anyhow::bail!(t!(
                "--allow \"{rule}\" is not a permission rule (like bash, bash(npm test) or edit(src/**))",
                rule = rule
            ));
        }
    }
    let append_system_prompt = values
        .string("append-system-prompt-file")
        .map(read_prompt_file)
        .transpose()?;
    // One trust decision covers the parts of .sasacode/config.json that could run code or send
    // data elsewhere. Asked here, before anything from the project is used.
    let project = assess_project(Path::new(&cwd))?;
    let mut trusted = values.flag("trust-project") || is_trusted(&cwd, &project);
    let interactive = print.is_none() && std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if !trusted && interactive {
        trusted = ask_trust(&cwd, &project).await;
        if trusted {
            save_trust(&cwd, &project)?;
        }
    }
    let max_turns = values
        .string("max-turns")
        .filter(|v| !v.is_empty())
        .map(turns_arg)
        .transpose()?;
    let mut opts = SetupOptions::new(cwd.clone());
    opts.model = values.string("model").map(str::to_string);
    opts.permission = values.string("permission").map(str::to_string);
    opts.thinking = values.string("thinking").map(str::to_string);
    opts.max_turns = max_turns;
    opts.resume = if values.flag("continue") {
        Some("last".into())
    } else {
        values.string("resume").map(str::to_string)
    };
    opts.no_session = values.flag("no-session");
    opts.append_system_prompt = append_system_prompt;
    opts.trust_project = trusted;
    opts.preloaded = Some((project.files, project.warnings));
    let harness = setup(opts).await?;
    for w in harness.warnings() {
        eprintln!("{}", t!("warning: {warning}", warning = sasacode_host::plain(&w)));
    }
    // As if "always" had been answered for each: a window that starts a process per message keeps
    // its grants this way.
    if !allow.is_empty() {
        let rules = PermissionRules {
            allow,
            ..Default::default()
        };
        harness.agent.permissions().add_rules(&rules)?;
    }
    // A mistyped -m should stop here, not start a session that fails at the first request.
    if let Some(model) = values.string("model") {
        harness.check_model(model).await?;
    }

    if let Some(print) = print {
        let mut prompt = std::iter::once(print)
            .chain(values.positionals.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        // With --control, stdin is the control channel: it is not part of the prompt.
        let piped = if control {
            String::new()
        } else {
            read_piped_stdin(values.flag("stdin")).await
        };
        if !piped.trim().is_empty() {
            prompt = format!("{prompt}\n\n{piped}");
        }
        let format = match output.as_deref() {
            Some("text") => OutputFormat::Text,
            Some("jsonl") => OutputFormat::Jsonl,
            _ => anyhow::bail!(t!("--output must be text or jsonl")),
        };
        let input = control.then(tokio::io::stdin);
        let opts = HeadlessOptions {
            slash: values.flag("slash"),
        };
        return Ok(run_headless(&harness, prompt, format, input, opts).await);
    }
    tui::run(harness, values.positionals.join(" ")).await
}

/// `Number(v)` that must be a whole number, at least 0.
fn turns_arg(v: &str) -> anyhow::Result<u64> {
    let n = js_number(v);
    if !(n.is_finite() && n.fract() == 0.0 && n >= 0.0) {
        anyhow::bail!(t!(
            "--max-turns needs a whole number (0 = no limit), got \"{value}\"",
            value = v
        ));
    }
    Ok(n as u64)
}

/// JS `Number(string)`: trimmed, empty is 0, hex/octal/binary prefixes, otherwise a decimal.
fn js_number(v: &str) -> f64 {
    let s = v.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
    if s.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0o", 8), ("0O", 8), ("0b", 2), ("0B", 2)] {
        if let Some(digits) = s.strip_prefix(prefix) {
            return u64::from_str_radix(digits, radix).map(|n| n as f64).unwrap_or(f64::NAN);
        }
    }
    match s {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if s.chars().any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E') {
        return f64::NAN;
    }
    s.parse::<f64>().unwrap_or(f64::NAN)
}
