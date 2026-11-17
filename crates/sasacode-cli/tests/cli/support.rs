//! What the tests share: the sasacode binary in a throwaway home, a throwaway home for the tests
//! that call `setup` in this process, and configs that keep every provider on a local server.
//!
//! No test reaches a real model API or the keychain. Every provider a test uses points at a local
//! fake server and names a key variable that is set, so its key never comes from the keychain; and
//! the keychain commands (`security`, `secret-tool`) are stand-ins that fail and leave a mark, which
//! fails the test.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value, json};

use sasacode_testing::harness::{JsonlOutput, RunOptions, RunOutput, Sandbox, parse_lines};

use crate::test_server::{Res, TestServer};

/// The key variable every test provider names, and its value.
pub const KEY_VAR: &str = "SASACODE_TEST_KEY";
pub const KEY: &str = "test-key";

/// The sasacode binary under test.
pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_sasacode")
}

/// `{ api, baseUrl, apiKeyEnv: KEY_VAR }`.
pub fn provider(api: &str, base_url: &str) -> Value {
    json!({ "api": api, "baseUrl": base_url, "apiKeyEnv": KEY_VAR })
}

/// `{ api: "replay", apiKeyEnv: KEY_VAR }`: a provider answered in this process by a replay provider.
pub fn replay_provider() -> Value {
    json!({ "api": "replay", "apiKeyEnv": KEY_VAR })
}

/// Config for one OpenAI-compatible server named "fake" (`fake/m`, no retries); `extra`'s
/// top-level keys replace these.
pub fn fake_config(url: &str, extra: Value) -> Value {
    merged(
        json!({
            "providers": { "fake": provider("openai-chat", url) },
            "model": "fake/m",
            "maxRetries": 0,
        }),
        extra,
    )
}

/// `{ ...base, ...extra }`.
pub fn merged(mut base: Value, extra: Value) -> Value {
    if let (Value::Object(b), Value::Object(e)) = (&mut base, extra) {
        for (k, v) in e {
            b.insert(k, v);
        }
    }
    base
}

/// A server that answers 404 to everything: where the built-in providers go in a test.
pub fn sink() -> TestServer {
    TestServer::start(|_| Res::text(404, ""))
}

/// `providers` with every built-in provider sent to `sink` under the test key, first: listing every
/// provider (the TUI's /model) then asks no real service and never the keychain. The sink must be
/// another host:port than the servers under test, or they would be listed as one service.
pub fn with_quiet_builtins(sink: &str, providers: Value) -> Value {
    let mut out = Map::new();
    for name in sasacode_host::builtin_providers().keys() {
        out.insert(name.clone(), provider_override(sink));
    }
    if let Value::Object(p) = providers {
        for (k, v) in p {
            out.insert(k, v);
        }
    }
    Value::Object(out)
}

fn provider_override(sink: &str) -> Value {
    json!({ "baseUrl": format!("{sink}/v1"), "apiKeyEnv": KEY_VAR })
}

/// The built-in providers' key variables: never passed on to a run.
fn builtin_key_vars() -> Vec<String> {
    let mut vars: Vec<String> = sasacode_host::builtin_providers()
        .values()
        .filter_map(|p| p.api_key_env.clone())
        .collect();
    vars.sort();
    vars.dedup();
    vars
}

/// Stand-ins for the keychain commands in `<dir>/keychain-stub`: they fail, and note in
/// `<dir>/keychain-asked` that they were called. Returns PATH with them first.
fn keychain_stub(dir: &Path) -> OsString {
    let stub = dir.join("keychain-stub");
    std::fs::create_dir_all(&stub).expect("the keychain stub folder");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mark = dir.join("keychain-asked");
        for name in ["security", "secret-tool"] {
            let path = stub.join(name);
            let script = format!("#!/bin/sh\necho \"$0 $*\" >> '{}'\nexit 1\n", mark.display());
            std::fs::write(&path, script).expect("a keychain stand-in");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(stub).chain(std::env::split_paths(&path))).expect("PATH")
}

fn assert_keychain_untouched(dir: &Path) {
    let mark = dir.join("keychain-asked");
    let asked = std::fs::read_to_string(&mark).unwrap_or_default();
    assert!(asked.is_empty(), "the keychain was asked:\n{asked}");
}

/// The sasacode binary in a sandbox (a throwaway home with `config` and an empty `.env`, and a
/// project folder it runs in), with the test key set and the keychain stubbed out.
pub struct Bin {
    pub sb: Sandbox,
    env: Vec<(String, String)>,
}

impl Bin {
    pub fn new(config: &Value) -> Bin {
        let sb = Sandbox::with_program(config, bin());
        let path = keychain_stub(sb.root());
        let env = vec![
            ("PATH".to_string(), path.to_string_lossy().into_owned()),
            (KEY_VAR.to_string(), KEY.to_string()),
        ];
        Bin { sb, env }
    }

    /// A variable this sets for every run (PATH, the test key).
    pub fn var(&self, name: &str) -> &str {
        self.env
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or_default()
    }

    /// The project folder as the run sees it (`process.cwd()`): with symbolic links resolved
    /// (/var is /private/var on macOS); Windows keeps the path it was given (no `\\?\` form).
    pub fn cwd(&self) -> PathBuf {
        if cfg!(windows) {
            return self.sb.project.clone();
        }
        self.sb.project.canonicalize().expect("the project folder")
    }

    pub fn run<I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(&self, args: I) -> RunOutput {
        self.run_with(args, None, &[])
    }

    /// With `stdin` written and closed (none: /dev/null), and `env` added.
    pub fn run_with<I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(
        &self,
        args: I,
        stdin: Option<&str>,
        env: &[(&str, &str)],
    ) -> RunOutput {
        let mut all = self.env.clone();
        all.extend(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        self.sb.run_with(
            args,
            RunOptions {
                stdin: stdin.map(str::to_string),
                env: all,
            },
        )
    }

    /// `sasacode -p <prompt> --output jsonl <extra>`, the events parsed.
    pub fn jsonl(&self, prompt: &str, extra: &[&str]) -> JsonlOutput {
        let mut args = vec!["-p", prompt, "--output", "jsonl"];
        args.extend_from_slice(extra);
        let r = self.run(args);
        JsonlOutput {
            code: r.code,
            events: parse_lines(&r.stdout),
            stderr: r.stderr,
        }
    }

    /// The command, not started: the caller decides how its pipes are read.
    pub fn command<I: IntoIterator<Item = S>, S: AsRef<std::ffi::OsStr>>(&self, args: I) -> Command {
        let mut cmd = Command::new(bin());
        cmd.args(args)
            .current_dir(&self.sb.project)
            .env_clear()
            .envs(self.sb.env())
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        cmd
    }
}

impl Drop for Bin {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            assert_keychain_untouched(self.sb.root());
        }
    }
}

/// Set in a test's child process: the root of its throwaway home.
const CHILD_ROOT: &str = "SASACODE_CLI_TEST_ROOT";

/// A throwaway home (`SASACODE_HOME`) and project, set for the whole process.
pub struct Home {
    pub home: PathBuf,
    pub proj: PathBuf,
}

impl Home {
    /// `~/.sasacode/config.json`.
    pub fn config(&self, config: &Value) {
        std::fs::write(self.home.join("config.json"), config.to_string()).expect("config.json");
    }

    /// `.sasacode/config.json` in the project.
    pub fn project_config(&self, config: &Value) {
        let dir = self.proj.join(".sasacode");
        std::fs::create_dir_all(&dir).expect(".sasacode");
        std::fs::write(dir.join("config.json"), config.to_string()).expect(".sasacode/config.json");
    }

    pub fn cwd(&self) -> String {
        self.proj.to_string_lossy().into_owned()
    }
}

/// What the child process of an [`isolated`] test wrote to stderr.
pub struct ChildOutput {
    pub stderr: String,
}

pub enum Role {
    /// This process: the test ran in a child, which passed.
    Parent(ChildOutput),
    /// The child: run the test here.
    Child(Home),
}

/// `setup` reads SASACODE_HOME, which is process-wide, and setting it needs `unsafe` (edition 2024).
/// So a test that calls it runs again in a child test process (this test alone) with SASACODE_HOME,
/// HOME and the test key set, no built-in provider's key, and the keychain stubbed out. In the
/// parent this waits for the child and checks that it passed; its output is there for tests that
/// check what the code printed (stderr is the code's own: the test harness writes to stdout).
pub fn isolated() -> Role {
    isolated_with(&[])
}

/// [`isolated`] with `env` added to the child's environment.
pub fn isolated_with(env: &[(&str, &str)]) -> Role {
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        sasacode_host::set_lang(sasacode_plugin_api::Lang::En); // these assertions quote the English strings
        let root = PathBuf::from(root);
        return Role::Child(Home {
            home: root.join("home"),
            proj: root.join("proj"),
        });
    }
    // The test harness names each test's thread after the test.
    let current = std::thread::current();
    let name = current
        .name()
        .filter(|n| *n != "main")
        .expect("a test thread named after its test");
    let dir = tempfile::Builder::new()
        .prefix("sasacode-cli-")
        .tempdir()
        .expect("a temporary directory");
    let root = dir.path().canonicalize().expect("the temporary directory");
    for d in ["home", "proj"] {
        std::fs::create_dir_all(root.join(d)).expect("the test folders");
    }
    let mut cmd = Command::new(std::env::current_exe().expect("the test binary"));
    cmd.args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_ROOT, &root)
        .env("SASACODE_HOME", root.join("home"))
        .env("HOME", &root)
        .env("USERPROFILE", &root)
        // The `ant auth login` profile is looked up here, never in the user's own config.
        .env("ANTHROPIC_CONFIG_DIR", root.join("anthropic"))
        .env_remove("ANTHROPIC_PROFILE")
        .env("SASACODE_LANG", "en")
        .env("NO_COLOR", "1")
        .env(KEY_VAR, KEY)
        .env("PATH", keychain_stub(&root))
        .env_remove("SASACODE_BIN")
        .stdin(Stdio::null());
    for var in builtin_key_vars() {
        cmd.env_remove(var);
    }
    cmd.envs(env.iter().copied());
    let out = cmd.output().expect("the test's child process");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{name} in its own home:\n{stdout}\n{stderr}"
    );
    assert_keychain_untouched(&root);
    Role::Parent(ChildOutput { stderr })
}

/// [`isolated`] for a test that checks nothing in the parent: the home in the child, None in the parent.
pub fn child() -> Option<Home> {
    child_with(&[])
}

/// [`child`] with `env` added to the child's environment.
pub fn child_with(env: &[(&str, &str)]) -> Option<Home> {
    match isolated_with(env) {
        Role::Child(home) => Some(home),
        Role::Parent(_) => None,
    }
}

/// One Chat Completions stream chunk from `model`: `delta`, and `finish` as its finish_reason.
pub fn chunk(model: &str, delta: Value, finish: Option<&str>) -> Value {
    json!({
        "id": "c", "object": "chat.completion.chunk", "created": 0, "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
    })
}

/// `events` as an SSE body (OpenAI's), ending with `[DONE]`.
pub fn sse_body(events: &[Value]) -> String {
    let mut body: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    body.push_str("data: [DONE]\n\n");
    body
}

/// The streamed Chat Completions answer `text` (OpenAI's SSE), to the model `model`.
pub fn sse_answer(model: &str, text: &str) -> String {
    let usage = json!({
        "id": "c", "object": "chat.completion.chunk", "created": 0, "model": model, "choices": [],
        "usage": { "prompt_tokens": 50, "completion_tokens": 1, "total_tokens": 51 },
    });
    sse_body(&[
        chunk(model, json!({ "content": text }), None),
        chunk(model, json!({}), Some("stop")),
        usage,
    ])
}

/// A response with an SSE body.
pub fn sse(body: String) -> Res {
    Res {
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        body: body.into_bytes(),
    }
}
