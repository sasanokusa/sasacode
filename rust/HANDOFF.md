# Handoff

Where the Rust rewrite stands, for whoever resumes it. Pointers, not copies.

- Branch `rust/rewrite`, worktree `.claude/worktrees/rust-rewrite`. Target version 0.20.0 (ADR-007);
  tagging and release only with the maintainer's approval.
- Progress per unit and the tests ported or dropped: [MAP.md](MAP.md).
- Decisions: [DECISIONS.md](DECISIONS.md). Rules for porting: [PORTING.md](PORTING.md). Names:
  [GLOSSARY.md](GLOSSARY.md). Research notes: [research/](research/).
- Oracles:
  - the Rust `cli_e2e` (`SASACODE_E2E_BIN=<abs path> cargo test -p sasacode-testing --test cli_e2e`);
  - the recorded app cases ([app-cases/README.md](app-cases/README.md)):
    `rust-anything app replay . --build <abs path to target/debug/sasacode>`.
- Repository checks: `cargo xtask checks` (also a test in `cargo test -p xtask`); numbers:
  `cargo xtask improve`.
- The TS is removed (the last commit of the rewrite); read it at tag v0.15.2. The GUI and protocol
  tests read the Rust sources, and the protocol fixtures are made by the Rust CLI
  (`cargo run -p sasacode-testing --example protocol-fixtures`, see docs/gui.md).
- Real-model checks (ADR-008): DeepSeek V4.1 Flash for the M0 task and the TUI; Gemma4 only in short
  runs, after checking `pmset -g batt` (not while the battery is draining).
