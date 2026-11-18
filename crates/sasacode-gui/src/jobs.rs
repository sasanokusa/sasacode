//! The background jobs the background-sessions plugin started (`bg_start`): read from where the
//! plugin keeps them, `$SASACODE_HOME/background-sessions/<id>/` (meta.json, output.log,
//! exit-code), with the end of what each wrote, and one still running stopped as the plugin's
//! `bg_stop` does: its process group sent SIGTERM, after making sure the pid is still the job's.

use std::path::{Path, PathBuf};

use iced::Task;
use serde::Deserialize;

use crate::app::{App, Msg};

/// How much of the end of a job's output is shown.
const TAIL_BYTES: u64 = 16 * 1024;
const TAIL_LINES: usize = 40;

/// A job as its meta.json says it, and whether it still runs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub pid: i64,
    /// Milliseconds since 1970.
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub killed: Option<bool>,
    #[serde(skip)]
    pub dir: PathBuf,
    /// Its exit code is not written and its pid is alive and started when the job did.
    #[serde(skip)]
    pub running: bool,
    /// The end of what it wrote.
    #[serde(skip)]
    pub tail: String,
}

/// The jobs kept under `home`, the newest first.
pub fn list(home: &Path) -> Vec<Job> {
    let Ok(dirs) = std::fs::read_dir(home.join("background-sessions")) else {
        return Vec::new();
    };
    let mut jobs: Vec<Job> = dirs
        .filter_map(Result::ok)
        .filter_map(|d| {
            let dir = d.path();
            let meta = std::fs::read_to_string(dir.join("meta.json")).ok()?;
            let mut job: Job = serde_json::from_str(&meta).ok()?;
            if job.exit_code.is_none() {
                job.exit_code = std::fs::read_to_string(dir.join("exit-code"))
                    .ok()
                    .and_then(|c| c.trim().parse().ok());
            }
            job.running = !dir.join("exit-code").exists() && same_process(job.pid, job.started_at);
            job.tail = tail(&dir.join("output.log"));
            job.dir = dir;
            Some(job)
        })
        .collect();
    jobs.sort_by_key(|j| std::cmp::Reverse(j.started_at));
    jobs
}

/// The last lines of `log`, read from its end.
fn tail(log: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(log) else {
        return String::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)));
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes);
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n")
}

/// Whether `pid` is alive and is the process that started at `started_ms`: a pid freed when the
/// job ended may be another program's now. Not known, it is not the job's.
#[cfg(unix)]
fn same_process(pid: i64, started_ms: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .output();
    let Ok(out) = out else {
        return false;
    };
    let said = String::from_utf8_lossy(&out.stdout);
    let words = said.split_whitespace().collect::<Vec<_>>().join(" ");
    let Ok(at) = chrono::NaiveDateTime::parse_from_str(&words, "%a %b %d %H:%M:%S %Y") else {
        return false;
    };
    let Some(at) = at.and_local_timezone(chrono::Local).earliest() else {
        return false;
    };
    // `ps` says the second; the plugin writes the time just after it started the process.
    (at.timestamp_millis() - started_ms).abs() < 10_000
}

#[cfg(not(unix))]
fn same_process(_pid: i64, _started_ms: i64) -> bool {
    false
}

/// Stops `job` as `bg_stop` does: marked killed in its meta, then its process group sent SIGTERM,
/// when its pid is still the job's.
#[cfg(unix)]
pub fn stop(job: &Job) -> Result<(), String> {
    if job.dir.join("exit-code").exists() || !same_process(job.pid, job.started_at) {
        return Err("ended".into());
    }
    let meta = job.dir.join("meta.json");
    if let Ok(text) = std::fs::read_to_string(&meta)
        && let Ok(serde_json::Value::Object(mut fields)) = serde_json::from_str(&text)
    {
        fields.insert("killed".into(), serde_json::Value::Bool(true));
        let _ = std::fs::write(&meta, serde_json::to_string_pretty(&fields).unwrap_or(text));
    }
    let kill = |target: String| {
        std::process::Command::new("kill")
            .args(["-TERM", "--", &target])
            .status()
            .is_ok_and(|s| s.success())
    };
    if kill(format!("-{}", job.pid)) || kill(job.pid.to_string()) {
        Ok(())
    } else {
        Err(format!("kill {}", job.pid))
    }
}

#[cfg(not(unix))]
pub fn stop(_job: &Job) -> Result<(), String> {
    Err("not on this system".into())
}

impl App {
    /// Reads the jobs again, off the window's thread.
    pub(crate) fn list_jobs(&self) -> Task<Msg> {
        let Some(home) = self.home.clone() else {
            return Task::none();
        };
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || list(&home))
                    .await
                    .unwrap_or_default()
            },
            Msg::Jobs,
        )
    }

    /// The jobs screen's messages; any other comes back.
    pub(crate) fn update_jobs(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let w = self.s.w();
        let task = match msg {
            Msg::Jobs(jobs) => {
                if self
                    .job_open
                    .as_ref()
                    .is_some_and(|id| !jobs.iter().any(|j| &j.id == id))
                {
                    self.job_open = None;
                }
                self.jobs = jobs;
                Task::none()
            }
            Msg::JobsRefresh => self.list_jobs(),
            Msg::JobOpen(id) => {
                self.job_open = if self.job_open.as_ref() == Some(&id) {
                    None
                } else {
                    Some(id)
                };
                Task::none()
            }
            Msg::JobStop(id) => {
                // The first press asks; the second stops it.
                if self.job_stopping.as_ref() != Some(&id) {
                    self.job_stopping = Some(id);
                    return Ok(Task::none());
                }
                self.job_stopping = None;
                let Some(job) = self.jobs.iter().find(|j| j.id == id).cloned() else {
                    return Ok(Task::none());
                };
                self.toast = Some(match stop(&job) {
                    Ok(()) => crate::i18n::fill(w.jobs_stopped, "id", &job.id),
                    Err(why) if why == "ended" => w.jobs_gone.to_owned(),
                    Err(why) => crate::i18n::fill(w.jobs_not_stopped, "why", why),
                });
                self.list_jobs()
            }
            other => return Err(other),
        };
        Ok(task)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn job(home: &Path, id: &str, meta: serde_json::Value, log: &str, exit: Option<&str>) {
        let dir = home.join("background-sessions").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
        std::fs::write(dir.join("output.log"), log).unwrap();
        if let Some(code) = exit {
            std::fs::write(dir.join("exit-code"), code).unwrap();
        }
    }

    #[test]
    fn jobs_are_listed_newest_first_with_the_end_of_their_output_and_a_freed_pid_is_not_running() {
        let home = std::env::temp_dir().join(format!("sasacode-jobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let log: String = (1..=60).map(|i| format!("line {i}\n")).collect();
        job(
            &home,
            "bg-1",
            serde_json::json!({"id": "bg-1", "command": "cargo test", "cwd": "/w", "runCwd": "/w",
                "pid": 1, "startedAt": 1_000, "status": "done"}),
            &log,
            Some("0\n"),
        );
        // Its pid is alive (this test's own), but it started long after the job said.
        job(
            &home,
            "bg-2",
            serde_json::json!({"id": "bg-2", "command": "npm run build", "cwd": "/w", "runCwd": "/w",
                "pid": std::process::id(), "startedAt": 2_000, "status": "running"}),
            "",
            None,
        );
        let jobs = list(&home);
        assert_eq!(jobs.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(), ["bg-2", "bg-1"]);
        assert!(!jobs[0].running, "a pid that is another process's is not the job");
        assert_eq!(jobs[1].exit_code, Some(0));
        assert!(jobs[1].tail.starts_with("line 21\n") && jobs[1].tail.ends_with("line 60"));
        assert!(stop(&jobs[0]).is_err(), "never signalled");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_running_job_is_stopped_by_its_process_group_and_marked_killed() {
        let home = std::env::temp_dir().join(format!("sasacode-jobs-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        // Taken at once by `ps`, the start is within the second of now.
        let now = chrono::Local::now().timestamp_millis();
        job(
            &home,
            "bg-3",
            serde_json::json!({"id": "bg-3", "command": "sleep 30", "cwd": "/w", "runCwd": "/w",
                "pid": pid, "startedAt": now, "status": "running"}),
            "",
            None,
        );
        let jobs = list(&home);
        assert!(jobs[0].running);
        stop(&jobs[0]).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(jobs[0].dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!(meta["killed"], true);
        std::fs::remove_dir_all(&home).unwrap();
    }
}
