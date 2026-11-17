//! The improvement loop's dashboard: `cargo xtask improve` (scripts/improve.ts before the port).
//! Prints what one round of docs/improvement.md starts from — the checks, the core line budget and
//! the size of the repository — and exits 1 when a check fails, so it works as a gate as well.

use std::fs;
use std::path::Path;
use std::process::ExitCode;

use crate::checks::{CORE_LINE_BUDGET, CheckResult, READMES, core_lines, root, run_checks, walk};

/// `12045 / 12500 (455 left)   sasacode-ai 3887 · sasacode-agent 4501 · …`.
fn core_line(root: &Path) -> (String, i64) {
    let lines = core_lines(root);
    let per_crate: Vec<String> = lines
        .by_crate
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect();
    let left = CORE_LINE_BUDGET as i64 - lines.total as i64;
    let text = format!(
        "{} / {CORE_LINE_BUDGET} ({left} left)   {}",
        lines.total,
        per_crate.join(" · ")
    );
    (text, left)
}

/// `14 crates · 8 docs · 250 Rust files, 40 of them tests`. A test file is one under a tests/
/// directory or a `*_tests.rs` module; unit tests inside a source file are not counted apart.
fn repository(root: &Path) -> String {
    let rust = walk(root, &|name| name.ends_with(".rs"));
    let tests = rust
        .iter()
        .filter(|path| {
            let name = path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default();
            name.ends_with("_tests.rs") || path.components().any(|c| c.as_os_str() == "tests")
        })
        .count();
    let count = |dir: &str, keep: &dyn Fn(&fs::DirEntry) -> bool| {
        fs::read_dir(root.join(dir))
            .map(|entries| entries.flatten().filter(|e| keep(e)).count())
            .unwrap_or(0)
    };
    let crates = count("crates", &|e| e.file_type().is_ok_and(|t| t.is_dir()));
    let docs = count("docs", &|e| e.file_name().to_string_lossy().ends_with(".md"));
    format!(
        "{crates} crates · {docs} docs · {} Rust files, {tests} of them tests",
        rust.len()
    )
}

/// The two READMEs are one document in two languages: a size difference is worth a look.
fn readmes(root: &Path) -> String {
    READMES
        .iter()
        .map(|name| {
            let text = fs::read_to_string(root.join(name)).unwrap_or_default();
            format!("{name} {} lines", text.split('\n').count() - 1)
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The checks, one line each, with what is wrong under a failing one.
pub fn print_checks(checks: &[CheckResult]) {
    for check in checks {
        let state = if check.problems.is_empty() { "ok  " } else { "FAIL" };
        println!("  {state}  {:<18} {}", check.name, check.promise);
        for problem in &check.problems {
            println!("          {problem}");
        }
    }
}

pub fn run() -> ExitCode {
    let root = root();
    let checks = run_checks(&root);
    let failed = checks.iter().filter(|check| !check.problems.is_empty()).count();
    let (core, left) = core_line(&root);

    println!("sasacode — improvement signals");
    println!();
    println!("checks");
    print_checks(&checks);
    println!();
    println!("core lines");
    println!("  {core}");
    println!();
    println!("repository");
    println!("  {}", repository(&root));
    println!("  {}", readmes(&root));
    println!();
    println!("next");
    if failed > 0 {
        let s = if failed == 1 { "" } else { "s" };
        println!("  fix the {failed} failing check{s} above, then run this again");
    } else {
        println!("  pick a lens in docs/improvement.md and make one small change");
    }
    if left < 100 {
        println!(
            "  the core has {left} lines left of its {CORE_LINE_BUDGET}: shrink it or raise the budget on purpose"
        );
    }
    println!("  confirm with: cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace");
    println!("  record it in: docs/improvement.md");

    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
