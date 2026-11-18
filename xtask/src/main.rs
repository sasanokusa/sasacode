//! `cargo xtask <task>`: the repository's own tasks, in Rust so that nothing but cargo is needed
//! (rust/DECISIONS.md ADR-009; the alias is in .cargo/config.toml). Ported from scripts/build.ts,
//! checks.ts and improve.ts; pack-plugin-api.ts and pack-plugins.ts are gone with the public plugin
//! API (ADR-002).

mod build;
mod checks;
mod improve;
mod rust_source;

use std::process::ExitCode;

const USAGE: &str = "usage: cargo xtask <task>

  build [--all | --targets <filter>] [--cross]
            release executables in dist/: sasacode for this machine, or sasacode-<target>
            for the release targets (all, or those whose name contains <filter>)
  checks    the repository's promises (core line budget, dependency record, READMEs, docs);
            exits 1 when one fails. `cargo test -p xtask` runs them too
  improve   the checks next to the core's headroom and the repository's size (docs/improvement.md)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("build") => build::run(&args[1..]),
        Some("checks") => Ok(checks_command()),
        Some("improve") => Ok(improve::run()),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        _ => {
            eprintln!("{USAGE}");
            Ok(ExitCode::from(2))
        }
    };
    result.unwrap_or_else(|error| {
        eprintln!("xtask: {error:#}");
        ExitCode::FAILURE
    })
}

/// `cargo xtask checks`: every check, and exit 1 when one fails.
fn checks_command() -> ExitCode {
    let checks = checks::run_checks(&checks::root());
    println!("checks");
    improve::print_checks(&checks);
    if checks.iter().any(|check| !check.problems.is_empty()) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
