# Porting rules (read before touching a unit)

Every unit of the rewrite follows these. Decisions are in [DECISIONS.md](DECISIONS.md), state in
[MAP.md](MAP.md), names in [GLOSSARY.md](GLOSSARY.md).

## Where things are

- TS original (the reference; do not edit): `packages/<pkg>/src`, tests in `packages/<pkg>/test`.
- Rust: `crates/sasacode-<pkg>/src`. One TS file → one Rust module of the same name (`-` → `_`).
- Shared contract (written first, change only with the coordinator): `crates/sasacode-ai/src/{types,provider,json,http}.rs`
  and `crates/sasacode-plugin-api/src/{api,hooks,tool,host}.rs`.
- Research notes with the semantic traps of a dependency: `rust/research/*.md`. Read the one for your unit.

## Working agreement for a unit

1. Own only your crate (or the files you were given). Never edit the root `Cargo.toml`, another
   crate, or the shared contract. If the contract is missing something, say so in your report
   (with the exact signature you need) and work around it locally meanwhile.
2. Dependencies: use `<dep>.workspace = true` for anything in the root `[workspace.dependencies]`.
   A new external crate may be added to your own crate's `Cargo.toml` with an explicit version;
   name it in your report.
3. Build and test only your crate, with your own target dir so parallel builds do not block:
   `CARGO_TARGET_DIR=target/<your-unit> cargo test -p <crate>` and
   `CARGO_TARGET_DIR=target/<your-unit> cargo clippy -p <crate> --all-targets -- -D warnings`,
   then `cargo fmt -p <crate>` (rustfmt `max_width = 120`).
4. Do not commit. Report: files written, tests ported (TS test name → Rust test), tests dropped and
   why, behaviour differences, contract requests.

## How to translate

- Faithful first: same structure, same names (snake_case), same error texts, same order of
  operations. Comments that explain *why* come along (translated to Rust context).
- Port every TS test in `packages/<pkg>/test` for your files into Rust tests (`#[cfg(test)]` in the
  module, or `tests/` in your crate). Same inputs, same expectations. A test that cannot apply any
  more (it tests the TS plugin loader, npm, Bun specifics) is dropped and listed with its reason.
- Errors that only become messages: `anyhow`. Where the TS threw an `Error` with a message that
  reaches the user or the model, keep the message text exactly.
- No `unsafe`. No `unwrap`/`expect`/`panic!` on paths where the TS handled or threw an error;
  `// INFALLIBLE: <why>` on the rare one that cannot fail.
- Async: tokio. Cancellation (`AbortSignal`) is `tokio_util::sync::CancellationToken`. Never hold
  a `std::sync::Mutex` guard across `.await`.
- HTTP: always build clients through `sasacode_ai::http::client()` / `sasacode_ai::http::builder()`
  (they install the TLS provider; a bare `reqwest::Client::new()` panics without it).
- Child processes: `tokio::process::Command`, environment from `sasacode_plugin_api::apply_child_env`
  so secrets loaded from ~/.sasacode/.env do not leak.

## Semantic traps (each has bitten ports before)

- **Strings**: JS `length`, `slice`, `indexOf`, `charAt` count UTF-16 code units; Rust `str` indices
  are UTF-8 bytes and slicing inside a character panics. Japanese and emoji text are normal input
  here. When the TS counts characters for a limit the user sees (truncation at N chars), decide
  whether it meant UTF-16 units (`s.encode_utf16().count()`) or code points (`[...s].length` =
  `s.chars().count()`), and truncate on a char boundary. Never `&s[..n]` with a computed n
  unless n came from `char_indices`/`find`.
- **toWellFormed / lone surrogates**: Rust strings cannot hold lone surrogates; JSON input that has
  them must be decoded leniently (serde_json rejects `"\ud800"` alone — handle that error path
  where the TS relied on toWellFormed).
- **JSON output**: key order is insertion order (serde_json `preserve_order` is on: build objects
  in the TS order). Absent optional fields are omitted, not `null`, unless the TS wrote null.
  Integral floats print without `.0` in JS: use `sasacode_ai::json::js_number` for f64 values that
  reach JSON. Timestamps are `i64` milliseconds.
- **`undefined` vs `null` vs missing**: `Option<T>` + `skip_serializing_if = "Option::is_none"` for
  optional fields; `Option<Option<T>>` (`sasacode_ai::json::double_option`) only where null and
  missing both occur.
- **Regex**: JS regex ≠ Rust `regex`. No lookaround/backreferences in `regex` (use `fancy-regex`).
  JS `\w`, `\d`, `\s`, `\b` without the `u` flag are ASCII-only for `\w\d\b` but `\s` includes
  Unicode spaces; Rust's are Unicode by default — write `(?-u:\w)` or explicit classes where the
  TS was ASCII. JS `.` does not match `\n` (same as Rust) but also not `\r`, `\u2028`, `\u2029`.
  `replace` with a string replaces the first match only; `replaceAll`/`/g` all of them.
- **Numbers**: JS numbers are f64. Integer division and `%` with negatives differ from Rust
  (`%` keeps the dividend's sign in both, but `Math.floor(a/b)` ≠ `a/b` for negatives).
  `parseInt("12px")` is 12; Rust `parse` fails.
- **Sorting**: JS `sort()` without a comparator sorts by UTF-16 string order; with one, it is
  stable. Rust `sort_by` is stable too; string comparison in Rust is by bytes (= code points), which
  differs from UTF-16 order only for characters above U+FFFF vs U+E000–U+FFFF.
- **Object iteration order**: integer-like keys come first in ascending order in JS objects.
  Rare in this code base; watch for it in maps keyed by numbers.
- **Paths**: Node `path.resolve`/`join` normalise `..`; Rust `Path::join` does not. Realpath with
  `std::fs::canonicalize` (on Windows prefer `dunce` semantics: no `\\?\` prefix in user-visible
  text). Permission matching uses NFC/NFD and case rules — port the tests first and make them pass.
- **Time**: `Date.now()` → `sasacode_ai::now_ms()`. ISO strings: `chrono::Utc::now().to_rfc3339_opts(Millis, true)`
  matches `toISOString()` (`2026-11-18T00:00:00.000Z`).
- **Keychain**: do exactly what `packages/cli/src/keys.ts` does (it shells out to `security` on
  macOS). A different access path can raise a macOS prompt that blocks an unattended run.
- **Process exit and output**: flush stdout before exit; a closed pipe (EPIPE) is not a crash.
