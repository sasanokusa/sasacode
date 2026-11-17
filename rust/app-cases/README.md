# Recorded CLI cases

`rust-anything app record` ran the TypeScript sasacode (0.15.2, the last TS version) on the offline cases in
`inputs.jsonl`, twice each, in a clean environment with `SASACODE_HOME` inside each case's sandbox. No case sends a
prompt, so none reaches a model or the keychain. Headless runs against a fake model are covered by
`crates/sasacode-testing/tests/cli_e2e.rs`.

Replay a build against them (the original in `rust-anything.json`, the TS, is gone, so always pass
`--build`, as an absolute path: the cases run in their own sandbox):

```sh
cargo build -p sasacode-cli
rust-anything app replay . --build "$PWD/target/debug/sasacode"
```

Changed by hand after recording, because 0.20 changes them on purpose (rust/DECISIONS.md):

- `version`, `version-short`: 0.20.0 (ADR-007).
- `help`, `help-ja`: the four `sasacode plugin …` lines are gone (ADR-002).
- `plugin-list`: `sasacode plugin` now fails with the message that it was removed (ADR-002).
- `help`, `help-ja`: `--trust-project` no longer names project plugins, which 0.20 does not load (ADR-002).
- `update-bogus`: a source checkout is updated with `cargo build --release`, not `bun install` (ADR-009).
