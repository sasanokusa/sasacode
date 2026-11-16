//! One run of `sasacode -p … --output jsonl --control stdio` as a child process, turned into a
//! stream of messages for the window.

use std::path::PathBuf;
use std::process::Stdio;

use iced::futures::{SinkExt, Stream};
use sasacode_protocol::{ControlMessage, Event, parse_event};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// What to run.
#[derive(Debug, Clone)]
pub struct RunSpec {
    /// The sasacode executable.
    pub program: PathBuf,
    /// The project folder: the child's working directory.
    pub cwd: PathBuf,
    pub prompt: String,
    /// Continue this session (`-r`).
    pub resume: Option<String>,
    /// `provider/model` (`-m`).
    pub model: Option<String>,
    /// `--permission`.
    pub permission: Option<String>,
    /// `--thinking`.
    pub thinking: Option<String>,
}

impl RunSpec {
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            "-p".into(),
            self.prompt.clone(),
            "--output".into(),
            "jsonl".into(),
            "--control".into(),
            "stdio".into(),
        ];
        let mut opt = |flag: &str, value: &Option<String>| {
            if let Some(v) = value {
                args.push(flag.into());
                args.push(v.clone());
            }
        };
        opt("-r", &self.resume);
        opt("-m", &self.model);
        opt("--permission", &self.permission);
        opt("--thinking", &self.thinking);
        args
    }
}

/// What a run reports, in order: `Started` once (or `Failed`), then `Event`s and `Stderr` lines,
/// then `Exited`.
#[derive(Debug, Clone)]
pub enum RunUpdate {
    /// Control messages sent here reach the child's stdin.
    Started(mpsc::UnboundedSender<ControlMessage>),
    Event(Box<Event>),
    /// A line the child wrote to stderr, or a stdout line that was not an event.
    Stderr(String),
    Exited(Option<i32>),
    /// The child could not be started.
    Failed(String),
}

pub fn run(spec: RunSpec) -> impl Stream<Item = RunUpdate> {
    iced::stream::channel(
        256,
        async move |mut out: iced::futures::channel::mpsc::Sender<RunUpdate>| {
            let mut command = tokio::process::Command::new(&spec.program);
            // On Windows, a console program started from a window would open a console of its own.
            #[cfg(windows)]
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            let mut child = match command
                .args(spec.args())
                .current_dir(&spec.cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    let _ = out
                        .send(RunUpdate::Failed(format!("{}: {e}", spec.program.display())))
                        .await;
                    return;
                }
            };
            let (control, mut control_rx) = mpsc::unbounded_channel::<ControlMessage>();
            let _ = out.send(RunUpdate::Started(control)).await;

            let mut stdin = child.stdin.take();
            let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
            let mut stderr = BufReader::new(child.stderr.take().expect("stderr is piped")).lines();
            let (mut stdout_open, mut stderr_open) = (true, true);

            while stdout_open || stderr_open {
                tokio::select! {
                    line = stdout.next_line(), if stdout_open => match line {
                        Ok(Some(line)) if line.trim().is_empty() => {}
                        Ok(Some(line)) => {
                            let update = match parse_event(&line) {
                                Ok(event) => {
                                    // The run is over: nothing more will be asked, so close stdin as a
                                    // parent that is done answering does.
                                    if matches!(event, Event::AgentEnd { .. }) {
                                        stdin = None;
                                    }
                                    RunUpdate::Event(Box::new(event))
                                }
                                Err(_) => RunUpdate::Stderr(line),
                            };
                            let _ = out.send(update).await;
                        }
                        _ => stdout_open = false,
                    },
                    line = stderr.next_line(), if stderr_open => match line {
                        Ok(Some(line)) => {
                            let _ = out.send(RunUpdate::Stderr(line)).await;
                        }
                        _ => stderr_open = false,
                    },
                    Some(message) = control_rx.recv() => {
                        if let Some(pipe) = stdin.as_mut() {
                            let line = message.to_line();
                            if pipe.write_all(line.as_bytes()).await.is_err() || pipe.flush().await.is_err() {
                                stdin = None;
                            }
                        }
                    }
                }
            }
            drop(stdin);
            let code = child.wait().await.ok().and_then(|s| s.code());
            let _ = out.send(RunUpdate::Exited(code)).await;
        },
    )
}
