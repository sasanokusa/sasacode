//! The folder's scripts: commands the user lists in the settings (a dev server, the tests, a
//! lint), run from the window in the folder by the user's shell, their output shown in the
//! inspector as it comes, and handed to the agent if asked.

use std::process::Stdio;
use std::time::Instant;

use iced::Task;
use iced::futures::{SinkExt, Stream};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::app::{App, Msg, Tab};
use crate::layout::Script;

/// How many runs are kept shown, the newest first.
const KEPT_RUNS: usize = 6;
/// How much of a run's output is kept: its end.
const KEPT_OUTPUT: usize = 64 * 1024;
/// How much of it goes to the agent when it is asked to look.
const TO_AGENT: usize = 6 * 1024;

/// A script run, or running.
#[derive(Debug)]
pub struct ScriptRun {
    pub id: u64,
    pub name: String,
    pub command: String,
    pub output: String,
    pub started: Instant,
    /// How long it ran, once it ended.
    pub took: Option<std::time::Duration>,
    /// How it ended: its exit code, or none for a signal; still running while `None`.
    pub ended: Option<Option<i32>>,
    /// Stops it when dropped.
    pub handle: Option<iced::task::Handle>,
}

impl ScriptRun {
    pub fn running(&self) -> bool {
        self.ended.is_none()
    }
}

/// What a script's run says.
#[derive(Debug, Clone)]
pub enum ScriptUpdate {
    Line(String),
    Exited(Option<i32>),
}

/// `command` run in `cwd` by the user's shell, as a login shell so it has the user's `PATH` as a
/// terminal would: a window started from the Dock has the system's alone.
fn shell(command: &str, cwd: &std::path::Path) -> tokio::process::Command {
    #[cfg(windows)]
    let mut c = {
        let mut c = tokio::process::Command::new("cmd");
        c.args(["/C", command]);
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let sh = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        let mut c = tokio::process::Command::new(sh);
        c.args(["-lc", command]);
        c
    };
    c.current_dir(cwd);
    c
}

fn run(command: String, cwd: std::path::PathBuf) -> impl Stream<Item = ScriptUpdate> {
    iced::stream::channel(
        256,
        async move |mut out: iced::futures::channel::mpsc::Sender<ScriptUpdate>| {
            let child = shell(&command, &cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn();
            let mut child = match child {
                Ok(child) => child,
                Err(e) => {
                    let _ = out.send(ScriptUpdate::Line(e.to_string())).await;
                    let _ = out.send(ScriptUpdate::Exited(None)).await;
                    return;
                }
            };
            let mut stdout = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
            let mut stderr = BufReader::new(child.stderr.take().expect("stderr is piped")).lines();
            let (mut stdout_open, mut stderr_open) = (true, true);
            while stdout_open || stderr_open {
                tokio::select! {
                    line = stdout.next_line(), if stdout_open => match line {
                        Ok(Some(line)) => { let _ = out.send(ScriptUpdate::Line(line)).await; }
                        _ => stdout_open = false,
                    },
                    line = stderr.next_line(), if stderr_open => match line {
                        Ok(Some(line)) => { let _ = out.send(ScriptUpdate::Line(line)).await; }
                        _ => stderr_open = false,
                    },
                }
            }
            let code = child.wait().await.ok().and_then(|s| s.code());
            let _ = out.send(ScriptUpdate::Exited(code)).await;
        },
    )
}

/// The end of `text`, at most `max` bytes of it, cut at a line.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    match text[start..].find('\n') {
        Some(i) => &text[start + i + 1..],
        None => &text[start..],
    }
}

impl App {
    /// This folder's scripts.
    pub(crate) fn scripts(&self) -> &[Script] {
        self.prefs
            .scripts
            .get(&self.cwd.to_string_lossy().into_owned())
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// The scripts' messages; any other comes back.
    pub(crate) fn update_scripts(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let key = self.cwd.to_string_lossy().into_owned();
        let task = match msg {
            Msg::RunScript(i) => {
                let Some(script) = self.scripts().get(i).cloned() else {
                    return Ok(Task::none());
                };
                if script.command.trim().is_empty() {
                    return Ok(Task::none());
                }
                self.script_runs_made += 1;
                let id = self.script_runs_made;
                let (task, handle) = Task::run(run(script.command.clone(), self.cwd.clone()), move |u| {
                    Msg::ScriptUpdate(id, u)
                })
                .abortable();
                self.script_runs.insert(
                    0,
                    ScriptRun {
                        id,
                        name: script.name.clone(),
                        command: script.command,
                        output: String::new(),
                        started: Instant::now(),
                        took: None,
                        ended: None,
                        handle: Some(handle.abort_on_drop()),
                    },
                );
                // The oldest that ended go; one that runs stays.
                while self.script_runs.len() > KEPT_RUNS {
                    match self.script_runs.iter().rposition(|r| !r.running()) {
                        Some(i) => {
                            self.script_runs.remove(i);
                        }
                        None => break,
                    }
                }
                self.tab = Tab::Commands;
                self.insp = true;
                task
            }
            Msg::ScriptUpdate(id, update) => {
                if let Some(run) = self.script_runs.iter_mut().find(|r| r.id == id) {
                    match update {
                        ScriptUpdate::Line(line) => {
                            run.output.push_str(&line);
                            run.output.push('\n');
                            if run.output.len() > KEPT_OUTPUT * 2 {
                                run.output = tail(&run.output, KEPT_OUTPUT).to_owned();
                            }
                        }
                        ScriptUpdate::Exited(code) => {
                            run.ended = Some(code);
                            run.took = Some(run.started.elapsed());
                            run.handle = None;
                        }
                    }
                }
                Task::none()
            }
            Msg::StopScript(id) => {
                if let Some(run) = self.script_runs.iter_mut().find(|r| r.id == id) {
                    // Dropped, the handle stops the stream, and with it the process.
                    run.handle = None;
                    run.ended = Some(None);
                    run.output.push_str(self.s.w().script_stopped);
                    run.output.push('\n');
                }
                Task::none()
            }
            Msg::ScriptToAgent(id) => {
                let Some(run) = self.script_runs.iter().find(|r| r.id == id) else {
                    return Ok(Task::none());
                };
                let prompt = crate::i18n::fill(
                    &crate::i18n::fill(self.s.w().script_prompt, "command", &run.command),
                    "output",
                    tail(run.output.trim_end(), TO_AGENT),
                );
                return Ok(self.put_in_input(&prompt));
            }
            Msg::ScriptAdd => {
                let _ = self.set(|p| {
                    p.scripts.entry(key).or_default().push(Script {
                        name: String::new(),
                        command: String::new(),
                    })
                });
                Task::none()
            }
            Msg::ScriptName(i, name) => {
                let _ = self.set(|p| {
                    if let Some(s) = p.scripts.get_mut(&key).and_then(|s| s.get_mut(i)) {
                        s.name = name;
                    }
                });
                Task::none()
            }
            Msg::ScriptCommand(i, command) => {
                let _ = self.set(|p| {
                    if let Some(s) = p.scripts.get_mut(&key).and_then(|s| s.get_mut(i)) {
                        s.command = command;
                    }
                });
                Task::none()
            }
            Msg::ScriptRemove(i) => {
                let _ = self.set(|p| {
                    if let Some(list) = p.scripts.get_mut(&key) {
                        if i < list.len() {
                            list.remove(i);
                        }
                        if list.is_empty() {
                            p.scripts.remove(&key);
                        }
                    }
                });
                Task::none()
            }
            other => return Err(other),
        };
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_output_keeps_its_end_from_a_line() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("one\ntwo\nthree\n", 9), "three\n");
        assert_eq!(tail("あいう\nえお", 7), "えお");
        assert_eq!(tail("あいう\nえお", 5), "お");
    }

    #[tokio::test]
    async fn a_script_runs_in_the_folder_and_says_its_lines_and_its_code() {
        use iced::futures::StreamExt;
        let dir = std::env::temp_dir();
        let said: Vec<ScriptUpdate> = run("echo one; echo two >&2; exit 3".into(), dir).collect().await;
        let lines: Vec<&str> = said
            .iter()
            .filter_map(|u| match u {
                ScriptUpdate::Line(l) => Some(l.as_str()),
                _ => None,
            })
            .collect();
        assert!(lines.contains(&"one") && lines.contains(&"two"));
        assert!(matches!(said.last(), Some(ScriptUpdate::Exited(Some(3)))));
    }
}
