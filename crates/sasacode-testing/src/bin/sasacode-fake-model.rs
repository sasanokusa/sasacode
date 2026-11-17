//! `sasacode-fake-model`: runs one command against the fake OpenAI-compatible model server, in a
//! throwaway sasacode home, for black-box recording of CLI cases (rust/DECISIONS.md ADR-004).

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;

use sasacode_testing::harness::{exit_code, fake_config, write_home};
use sasacode_testing::{FakeServer, Script};
use serde_json::Value;

const HELP: &str = r#"Usage: sasacode-fake-model --script <file.json> [options] -- <command> [args...]

Starts a fake OpenAI-compatible model server on 127.0.0.1 that answers chat requests with the
replies in the script, writes a sasacode config for it into a fresh temporary SASACODE_HOME, runs
the command with stdin, stdout and stderr passed through, and exits with its exit code.

Script: a JSON array of replies, or {"replies": [...], "models": [...]} (/models lists
[{"id": "m"}] unless "models" says otherwise). A reply has any of:
  reasoning       text, streamed as reasoning_content in three pieces
  text            text, streamed as content in three pieces
  toolCalls       [{"name": "write", "args": {...}}, ...]
  toolCallShape   "no-index" (no index, arguments in two halves) or "name-later" (id first, then
                  the name, then the arguments); default: one chunk per call
  cut             true: stop after the content (no finish_reason, no usage, no [DONE])
  status          answer with this HTTP status and an error body instead of a stream
  streamError     send this error as the only event of a 200 stream
Events are a millisecond apart. A request after the last reply gets a 500. Tool-call ids are
call_<i>_<request number in base 36>, the same on every run.

Config written to $SASACODE_HOME/config.json:
  {"providers": {"fake": {"api": "openai-chat", "baseUrl": "<server>/v1"}},
   "model": "fake/m", "maxRetries": 0}
$SASACODE_HOME/.env is empty, so no keys from this machine are loaded.

Environment of the command: this one, with HOME and USERPROFILE set to the temporary root,
SASACODE_HOME to <root>/home, SASACODE_LANG=en and NO_COLOR=1. It runs in the current
directory. The temporary root is removed when the command ends.

Options:
  --script <file>     the replies (required)
  --config <json>     a JSON object whose top-level keys replace the config's,
                      e.g. '{"permissions": {"mode": "edits"}}'
  --requests <file>   when the command ends, write the request bodies the server got there,
                      one JSON object per line
  -h, --help          print this help

Exit code: the command's (128 + the signal if a signal ended it); 2 for a usage error here, 1 when
the server or the home cannot be set up, 127 when the command cannot be started."#;

struct Args {
    script: PathBuf,
    config: Option<String>,
    requests: Option<PathBuf>,
    command: Vec<OsString>,
}

enum Parsed {
    Run(Args),
    Help,
}

fn parse_args(mut args: impl Iterator<Item = OsString>) -> Result<Parsed, String> {
    let mut script = None;
    let mut config = None;
    let mut requests = None;
    while let Some(arg) = args.next() {
        let Some(name) = arg.to_str() else {
            return Err(format!("unknown argument {}", arg.to_string_lossy()));
        };
        match name {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--" => {
                let command: Vec<OsString> = args.collect();
                if command.is_empty() {
                    return Err("no command after --".into());
                }
                let Some(script) = script else {
                    return Err("--script is required".into());
                };
                return Ok(Parsed::Run(Args {
                    script,
                    config,
                    requests,
                    command,
                }));
            }
            "--script" | "--config" | "--requests" => {
                let Some(value) = args.next() else {
                    return Err(format!("{name} needs a value"));
                };
                match name {
                    "--script" => script = Some(PathBuf::from(value)),
                    "--requests" => requests = Some(PathBuf::from(value)),
                    _ => config = Some(value.to_string_lossy().into_owned()),
                }
            }
            _ => return Err(format!("unknown argument {name}")),
        }
    }
    Err("no command: put it after --".into())
}

fn fail(code: i32, message: &str) -> ! {
    eprintln!("sasacode-fake-model: {message}");
    std::process::exit(code);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(Parsed::Run(args)) => args,
        Ok(Parsed::Help) => {
            println!("{HELP}");
            return;
        }
        Err(e) => fail(2, &format!("{e} (see --help)")),
    };
    let code = run(args).await.unwrap_or_else(|(code, message)| fail(code, &message));
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

async fn run(args: Args) -> Result<i32, (i32, String)> {
    let text = std::fs::read_to_string(&args.script)
        .map_err(|e| (1, format!("could not read {}: {e}", args.script.display())))?;
    let script = Script::parse(&text).map_err(|e| (1, format!("{}: {e}", args.script.display())))?;
    let extra = match &args.config {
        Some(json) => match serde_json::from_str::<Value>(json) {
            Ok(v @ Value::Object(_)) => v,
            _ => return Err((2, "--config must be a JSON object".into())),
        },
        None => Value::Object(Default::default()),
    };
    let mut server = FakeServer::from_script(script).map_err(|e| (1, format!("could not start the server: {e}")))?;
    let root = tempfile::Builder::new()
        .prefix("sasacode-fake-model-")
        .tempdir()
        .map_err(|e| (1, format!("could not make a temporary directory: {e}")))?;
    let home = root.path().join("home");
    write_home(&home, &fake_config(server.url(), extra))
        .map_err(|e| (1, format!("could not write {}: {e}", home.display())))?;

    let mut child = tokio::process::Command::new(&args.command[0])
        .args(&args.command[1..])
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("SASACODE_HOME", &home)
        .env("SASACODE_LANG", "en")
        .env("NO_COLOR", "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| (127, format!("could not run {}: {e}", args.command[0].to_string_lossy())))?;
    let status = loop {
        tokio::select! {
            status = child.wait() => break status,
            // Ctrl-C reached the command too; it decides when to stop, and the cleanup still runs.
            _ = tokio::signal::ctrl_c() => {}
        }
    };
    let code = match status {
        Ok(status) => exit_code(status),
        Err(e) => return Err((1, format!("could not wait for the command: {e}"))),
    };
    server.stop();
    if let Some(file) = &args.requests {
        let lines: String = server.requests().iter().map(|r| format!("{r}\n")).collect();
        std::fs::write(file, lines).map_err(|e| (1, format!("could not write {}: {e}", file.display())))?;
    }
    drop(root);
    Ok(code)
}
