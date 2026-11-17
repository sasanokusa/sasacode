# Recorded CLI cases

`rust-anything app record` ran the TypeScript sasacode (0.15.2, the last TS version) on the offline cases in
`inputs.jsonl`, twice each, in a clean environment with `SASACODE_HOME` inside each case's sandbox. No case sends a
prompt, so none reaches a model or the keychain. Headless runs against a fake model are covered by
`crates/sasacode-testing/tests/cli_e2e.rs`.

Replay a build against them:

```sh
cargo build -p sasacode-cli
rust-anything app replay . --build target/debug/sasacode
```

Changed by hand after recording, because 0.20 changes them on purpose (rust/DECISIONS.md):

- `version`, `version-short`: 0.20.0 (ADR-007).
- `help`, `help-ja`: the four `sasacode plugin …` lines are gone (ADR-002).
- `plugin-list`: `sasacode plugin` now fails with the message that it was removed (ADR-002).
