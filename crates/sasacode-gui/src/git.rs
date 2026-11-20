//! The folder's git repository as the inspector's git tab shows it: the branch, what is changed
//! and not committed, each file's diff, and what can be done about it there (stage, unstage,
//! discard, commit, push, open a pull request with `gh`). Each is a run of `git` or `gh` in the
//! folder, away from the window.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::diff::{Diff, Sign};

/// A file bigger than this that git does not know yet is not read to be shown.
const UNTRACKED_MAX: u64 = 256 * 1024;
/// How many files get their diff read; the rest are listed without one. A folder with thousands of
/// files git does not know (a build left unignored) would take long to read whole.
const DIFFED: usize = 200;
/// How long a push or a pull request may take; one that waits for a password nobody can type is
/// stopped, rather than leaving the git tab busy for good.
const REMOTE_WAIT: Duration = Duration::from_secs(120);

/// Given to every git the window runs, before the subcommand: no file-system monitor started from
/// the repository's config, and no bare repository found in the folder unless one is named. The
/// same as `sasacode-bundled`'s `git_args::GUARD`; a test compares the two.
pub(crate) const GUARD: [&str; 4] = ["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit"];

/// After a subcommand that shows a diff: no external diff program and no textconv filter from the
/// repository's config. The same as `sasacode-bundled`'s `git_args::NO_DIFF_HELPERS`.
pub(crate) const NO_DIFF_HELPERS: [&str; 2] = ["--no-ext-diff", "--no-textconv"];

/// `git <sub>` (`diff` or `show`) uncolored, given [`NO_DIFF_HELPERS`], then `more`.
fn shown<'a>(sub: &'a str, more: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![sub, "--no-color"];
    args.extend(NO_DIFF_HELPERS);
    args.extend(more);
    args
}

/// `git`, given [`GUARD`].
pub(crate) fn command() -> Command {
    let mut c = Command::new("git");
    c.args(GUARD);
    c
}

/// [`command`], to be awaited.
pub(crate) fn async_command() -> tokio::process::Command {
    let mut c = tokio::process::Command::new("git");
    c.args(GUARD);
    c
}

/// The same as [`GUARD`], for the git that another program (`gh`) runs: as the environment says
/// it, when the environment does not already set config so.
fn guard_env(c: &mut Command) {
    if std::env::var_os("GIT_CONFIG_COUNT").is_some() {
        return;
    }
    let pairs: Vec<(&str, &str)> = GUARD.chunks(2).filter_map(|pair| pair[1].split_once('=')).collect();
    c.env("GIT_CONFIG_COUNT", pairs.len().to_string());
    for (i, (key, value)) in pairs.into_iter().enumerate() {
        c.env(format!("GIT_CONFIG_KEY_{i}"), key)
            .env(format!("GIT_CONFIG_VALUE_{i}"), value);
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// The folder is in a git repository.
    pub repo: bool,
    /// The repository's top folder, which the files' paths are relative to.
    pub top: std::path::PathBuf,
    /// The branch checked out; none when HEAD is detached.
    pub branch: Option<String>,
    /// The branch has an upstream to push to.
    pub upstream: bool,
    /// Commits not pushed yet, when it has; and commits of the upstream not pulled yet.
    pub ahead: usize,
    pub behind: usize,
    /// The branch pull requests go into, by its local name: `main`.
    pub default_branch: Option<String>,
    pub files: Vec<File>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct File {
    /// Relative to the repository's top.
    pub path: String,
    /// `git status --porcelain`'s two letters: the index's, and the working tree's.
    pub index: char,
    pub tree: char,
    /// What is staged: from the last commit to the index.
    pub staged_diff: Diff,
    /// What is not: from the index to the working tree; all of a file git does not know yet.
    pub unstaged_diff: Diff,
}

impl File {
    pub fn untracked(&self) -> bool {
        self.index == '?'
    }

    /// Something of it is staged.
    pub fn staged(&self) -> bool {
        !matches!(self.index, ' ' | '?')
    }

    /// Something of it is not staged.
    pub fn unstaged(&self) -> bool {
        self.tree != ' '
    }

    /// The lines added and removed, staged or not.
    pub fn counts(&self) -> (usize, usize) {
        (
            self.staged_diff.added + self.unstaged_diff.added,
            self.staged_diff.removed + self.unstaged_diff.removed,
        )
    }
}

/// What is done to one hunk of a file's diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkAct {
    /// A hunk not staged, into the index.
    Stage,
    /// A staged hunk, out of the index.
    Unstage,
    /// A hunk not staged, thrown away from the working tree.
    Discard,
}

impl HunkAct {
    /// Which of the file's diffs the hunk is in: the staged one, else the one not staged.
    pub fn in_staged(self) -> bool {
        self == HunkAct::Unstage
    }
}

/// What can be done from the git tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Stage(String),
    Unstage(String),
    /// Back to what is committed; a file git does not know is removed.
    Discard(String),
    /// Hunk `at` of the file's diff (by [`HunkAct::in_staged`]), whose `@@` line is `head`: when it
    /// is no longer that hunk, nothing is done.
    Hunk {
        path: String,
        act: HunkAct,
        at: usize,
        head: String,
    },
    /// What is staged, or everything when nothing is.
    Commit(String),
    Push,
    /// A pull request of the branch, its title and description from its commits; or, given, with
    /// this title and description.
    Pr(Option<(String, String)>),
    /// Another branch checked out, or a new one made from where HEAD is.
    Switch(String),
    NewBranch(String),
    /// The branch's pull request merged by `gh pr merge`: `merge`, `squash` or `rebase`.
    Merge(String),
    /// Merged so, by GitHub, once its checks pass (`gh pr merge --auto`).
    AutoMerge(String),
    /// A pull request made as a draft, with this title and description.
    DraftPr(String, String),
    /// The pull request ready for review, or back to a draft.
    Ready(bool),
    /// The folder made a repository.
    Init,
    /// What the upstream has, taken in when it only adds to the branch.
    Pull,
    /// The pull request's base merged into the branch, by GitHub.
    UpdateBranch,
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Result<String, String> {
    run_within(cwd, program, args, None)
}

/// Creates a secret gist of `markdown`, named `file`, with `program` (`gh`); says its URL. Never
/// public: `gh gist create` makes a secret one unless told otherwise, and is not told.
pub fn gist(cwd: &Path, program: &str, file: &str, description: &str, markdown: &str) -> Result<String, String> {
    let out = exec(
        cwd,
        program,
        &["gist", "create", "--filename", file, "--desc", description, "-"],
        Some(REMOTE_WAIT),
        Some(markdown),
    )
    // gh says what is wrong first, and how else to go about it after ("Alternatively, …").
    .map_err(|e| {
        e.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_owned()
    })?;
    out.lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("https://"))
        .map(str::to_owned)
        .ok_or_else(|| out.trim().to_owned())
}

/// Runs `program` in `cwd` with nothing to read from, so that nothing it asks waits for an answer:
/// git and gh are told not to ask on the terminal, and with `wait`, what takes longer is stopped.
fn run_within(cwd: &Path, program: &str, args: &[&str], wait: Option<Duration>) -> Result<String, String> {
    exec(cwd, program, args, wait, None).map(|out| out.trim().to_owned())
}

/// `sasacode update --check`: the line the CLI says about its newest release, in `lang`. Nothing
/// is downloaded.
pub fn update_check(cwd: &Path, program: &str, lang: Option<&str>) -> Result<String, String> {
    let envs: Vec<(&str, &str)> = lang.map(|l| ("SASACODE_LANG", l)).into_iter().collect();
    exec_with(
        cwd,
        program,
        &["update", "--check"],
        Some(Duration::from_secs(30)),
        None,
        &envs,
    )
    .map(|out| out.trim().lines().last().unwrap_or_default().to_owned())
    .map_err(|e| {
        e.lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or_default()
            .to_owned()
    })
}

/// Runs `program` as [`run_within`] does, with `input` to read if given, and says what it wrote
/// as it wrote it: a diff's last line of context may be a blank line's one space.
fn exec(
    cwd: &Path,
    program: &str,
    args: &[&str],
    wait: Option<Duration>,
    input: Option<&str>,
) -> Result<String, String> {
    exec_with(cwd, program, args, wait, input, &[])
}

/// [`exec`], with `envs` set for `program`.
fn exec_with(
    cwd: &Path,
    program: &str,
    args: &[&str],
    wait: Option<Duration>,
    input: Option<&str>,
    envs: &[(&str, &str)],
) -> Result<String, String> {
    use std::io::{Read, Write};
    let mut command = if program == "git" {
        command()
    } else {
        Command::new(program)
    };
    if program == "gh" {
        guard_env(&mut command);
    }
    let mut child = command
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().copied())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        let input = input.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let until = wait.map(|w| std::time::Instant::now() + w);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if until.is_none_or(|u| std::time::Instant::now() < u) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!(
                    "{program}: gave up after {}s",
                    wait.unwrap_or_default().as_secs()
                ));
            }
            Err(e) => break Err(format!("{program}: {e}")),
        }
    };
    let text = |b: Vec<u8>| String::from_utf8_lossy(&b).into_owned();
    let (out, err) = (
        text(out.join().unwrap_or_default()),
        text(err.join().unwrap_or_default()),
    );
    if status?.success() {
        Ok(out)
    } else {
        let err = err.trim();
        Err(if err.is_empty() {
            out.trim().to_owned()
        } else {
            err.to_owned()
        })
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    run(cwd, "git", args)
}

/// The repository's top folder, where the paths git gives are relative to.
fn top(cwd: &Path) -> Option<std::path::PathBuf> {
    git(cwd, &["rev-parse", "--show-toplevel"]).ok().map(Into::into)
}

/// What a commit message is written from: the subjects of the repository's last commits, for
/// their style, and the change to commit: what is staged, else all of it, new files named.
pub fn commit_context(cwd: &Path) -> Result<(String, String), String> {
    let top = top(cwd).ok_or_else(|| "not a git repository".to_owned())?;
    let log = git(&top, &["log", "-10", "--format=%s"]).unwrap_or_default();
    let staged = git(&top, &shown("diff", &["--cached", "--quiet"])).is_err();
    let mut change = if staged {
        git(&top, &shown("diff", &["--cached"]))?
    } else {
        git(&top, &shown("diff", &["HEAD"])).or_else(|_| git(&top, &shown("diff", &[])))?
    };
    if !staged {
        let new = git(&top, &["ls-files", "--others", "--exclude-standard"]).unwrap_or_default();
        if !new.trim().is_empty() {
            change.push_str("\n\nNew files:\n");
            change.push_str(&new);
        }
    }
    if change.trim().is_empty() {
        return Err("nothing to commit".into());
    }
    Ok((log, change))
}

/// What a pull request of the branch is written from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrContext {
    /// The branch it goes into, as git names it: `origin/main`.
    pub base: String,
    /// The branch's commits, oldest first: subject and body.
    pub commits: Vec<(String, String)>,
    /// What the branch changes: by file, and the diff.
    pub stat: String,
    pub diff: String,
}

impl PrContext {
    /// A title and a description as `gh pr create --fill` makes them: one commit's own, else the
    /// branch's commits listed.
    pub fn filled(&self, branch: &str) -> (String, String) {
        match self.commits.as_slice() {
            [(subject, body)] => (subject.clone(), body.trim().to_owned()),
            commits => (
                branch.to_owned(),
                commits
                    .iter()
                    .map(|(s, _)| format!("- {s}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
    }
}

pub fn pr_context(cwd: &Path) -> Result<PrContext, String> {
    let top = top(cwd).ok_or_else(|| "not a git repository".to_owned())?;
    let base = base_of(&top).ok_or_else(|| "no branch to compare with".to_owned())?;
    let range = format!("{base}..HEAD");
    let log = git(
        &top,
        &["log", "--reverse", "--format=%s%x1f%b%x1e", "--end-of-options", &range],
    )?;
    let commits = log
        .split('\u{1e}')
        .filter_map(|c| {
            let (subject, body) = c.trim().split_once('\u{1f}')?;
            Some((subject.trim().to_owned(), body.trim().to_owned()))
        })
        .filter(|(s, _)| !s.is_empty())
        .collect();
    let since = format!("{base}...HEAD");
    Ok(PrContext {
        stat: git(&top, &shown("diff", &["--stat", "--end-of-options", &since])).unwrap_or_default(),
        diff: git(&top, &shown("diff", &["--end-of-options", &since])).unwrap_or_default(),
        base,
        commits,
    })
}

/// The repository as it is now.
pub fn status(cwd: &Path) -> Status {
    let Some(top) = top(cwd) else {
        return Status::default();
    };
    let mut status = Status {
        repo: true,
        top: top.clone(),
        default_branch: base_of(&top).map(|b| b.trim_start_matches("origin/").to_owned()),
        ..Status::default()
    };
    let Ok(out) = git(
        &top,
        &["status", "--porcelain=v1", "-z", "--branch", "--untracked-files=all"],
    ) else {
        return status;
    };
    let mut entries = out.split('\0').filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if let Some(head) = entry.strip_prefix("## ") {
            let (branch, rest) = head.split_once("...").unwrap_or((head, ""));
            status.branch = (!branch.starts_with("HEAD") && !branch.starts_with("No commits"))
                .then(|| branch.trim().to_owned())
                .or_else(|| branch.strip_prefix("No commits yet on ").map(str::to_owned));
            status.upstream = !rest.is_empty();
            let count = |word: &str| {
                rest.split_once(word)
                    .and_then(|(_, n)| n.trim_end_matches(']').split(',').next()?.trim().parse().ok())
                    .unwrap_or(0)
            };
            status.ahead = count("ahead ");
            status.behind = count("behind ");
            continue;
        }
        let mut letters = entry.chars();
        let (Some(index), Some(tree)) = (letters.next(), letters.next()) else {
            continue;
        };
        let path = entry.get(3..).unwrap_or_default().to_owned();
        // A rename names where it came from next; the diff is of where it is.
        if index == 'R' || index == 'C' {
            entries.next();
        }
        let mut file = File {
            path,
            index,
            tree,
            staged_diff: Diff::default(),
            unstaged_diff: Diff::default(),
        };
        if status.files.len() < DIFFED {
            if file.staged() {
                file.staged_diff = file_diff(&top, &file.path, true);
            }
            if file.untracked() {
                file.unstaged_diff = new_file(&top.join(&file.path));
            } else if file.unstaged() {
                file.unstaged_diff = file_diff(&top, &file.path, false);
            }
        }
        status.files.push(file);
    }
    status
}

/// All of a file git does not know yet, as new lines, when it is small enough to read.
fn new_file(file: &Path) -> Diff {
    let small = std::fs::metadata(file).is_ok_and(|m| m.is_file() && m.len() <= UNTRACKED_MAX);
    match small.then(|| std::fs::read_to_string(file).ok()).flatten() {
        Some(text) => Diff::create(&text),
        None => Diff::default(),
    }
}

/// `git diff` of `path`: what is staged (from the last commit, or from nothing before the first),
/// else what is not.
fn raw_diff(top: &Path, path: &str, staged: bool) -> Result<String, String> {
    let mut args = shown("diff", &[]);
    if staged {
        args.push("--cached");
    }
    args.extend(["--", path]);
    exec(top, "git", &args, None, None)
}

/// The hunks of what is staged in `path`, or of what is not.
fn file_diff(top: &Path, path: &str, staged: bool) -> Diff {
    let out = raw_diff(top, path, staged).unwrap_or_default();
    // The hunks only: the lines before the first say which files, as `---` and `+++`.
    let hunks = out.find("\n@@").map_or("", |at| &out[at + 1..]);
    Diff::parse(hunks)
}

/// A patch of hunk `at` of `diff` alone, with the lines that say which file it is: when that hunk
/// is there and its `@@` line is `head`.
pub(crate) fn hunk_patch(diff: &str, at: usize, head: &str) -> Option<String> {
    let mut header = String::new();
    let mut hunks: Vec<String> = Vec::new();
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@") {
            hunks.push(String::new());
        }
        match hunks.last_mut() {
            Some(hunk) => hunk.push_str(line),
            None => header.push_str(line),
        }
    }
    let hunk = hunks.get(at)?;
    if hunk.lines().next()?.trim_end() != head.trim_end() || header.is_empty() {
        return None;
    }
    let mut patch = header + hunk;
    if !patch.ends_with('\n') {
        patch.push('\n');
    }
    Some(patch)
}

/// The branches there are to check out: the repository's own, the one last committed to first,
/// then those only a remote has, by the name they would be checked out as.
pub fn branches(cwd: &Path) -> Vec<String> {
    let list = |refs: &str| -> Vec<String> {
        git(
            cwd,
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short)",
                refs,
            ],
        )
        .map(|out| out.lines().map(str::to_owned).collect())
        .unwrap_or_default()
    };
    let mut all = list("refs/heads");
    for remote in list("refs/remotes") {
        // `origin/main` is checked out as `main`; `origin` alone is the remote's HEAD.
        let Some((_, name)) = remote.split_once('/') else {
            continue;
        };
        if name != "HEAD" && !all.iter().any(|b| b == name) {
            all.push(name.to_owned());
        }
    }
    all
}

/// The pull request of the branch checked out, as `gh` sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// OPEN, MERGED or CLOSED.
    pub state: String,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED, or none.
    pub decision: String,
    pub checks: Vec<Check>,
    /// What reviewers said: on the pull request, and on lines of its diff.
    pub comments: Vec<Remark>,
    /// A draft, not ready for review yet; and to be merged by GitHub once its checks pass.
    pub draft: bool,
    pub auto_merge: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub name: String,
    /// How it went: passed, failed, pending, or skipped.
    pub state: CheckState,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    Failed,
    Pending,
    Skipped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Remark {
    pub author: String,
    pub body: String,
    /// The file and line it was said on, for one on the diff.
    pub path: Option<String>,
    pub line: Option<usize>,
}

impl Pr {
    pub fn failing(&self) -> impl Iterator<Item = &Check> {
        self.checks.iter().filter(|c| c.state == CheckState::Failed)
    }

    /// Whether a check of it, still open, has not finished.
    pub fn pending(&self) -> bool {
        self.state == "OPEN" && self.checks.iter().any(|c| c.state == CheckState::Pending)
    }
}

/// What finished between two reads of a pull request: none while its checks run, or when it is
/// another one; otherwise how many failed.
pub fn checks_finished(before: Option<&Pr>, now: Option<&Pr>) -> Option<usize> {
    let (before, now) = (before?, now?);
    (before.number == now.number && before.pending() && !now.pending() && !now.checks.is_empty())
        .then(|| now.failing().count())
}

/// A check of `statusCheckRollup` as `gh pr view --json` gives it: a check run or a status.
fn check(v: &serde_json::Value) -> Check {
    let word = |key: &str| v[key].as_str().unwrap_or_default().to_ascii_uppercase();
    let (status, conclusion, state) = (word("status"), word("conclusion"), word("state"));
    let state = match (status.as_str(), conclusion.as_str(), state.as_str()) {
        (_, "SUCCESS" | "NEUTRAL", _) | (_, _, "SUCCESS") => CheckState::Passed,
        (_, "SKIPPED", _) => CheckState::Skipped,
        (_, "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE", _)
        | (_, _, "FAILURE" | "ERROR") => CheckState::Failed,
        _ => CheckState::Pending,
    };
    let name = v["name"].as_str().or_else(|| v["context"].as_str()).unwrap_or("?");
    let url = v["detailsUrl"]
        .as_str()
        .or_else(|| v["targetUrl"].as_str())
        .unwrap_or_default();
    Check {
        name: name.to_owned(),
        state,
        url: url.to_owned(),
    }
}

/// The pull request of the branch checked out, with its checks and what reviewers said; none when
/// it has none, or `gh` is not there or not signed in.
pub fn pr(cwd: &Path) -> Option<Pr> {
    let out = run_within(
        cwd,
        "gh",
        &[
            "pr",
            "view",
            "--json",
            "number,title,url,state,reviewDecision,statusCheckRollup,comments,reviews,isDraft,autoMergeRequest",
        ],
        Some(REMOTE_WAIT),
    )
    .ok()?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_owned();
    let mut pr = Pr {
        number: v["number"].as_u64()?,
        title: text(&v["title"]),
        url: text(&v["url"]),
        state: text(&v["state"]),
        decision: text(&v["reviewDecision"]),
        checks: v["statusCheckRollup"]
            .as_array()
            .map(|all| all.iter().map(check).collect())
            .unwrap_or_default(),
        comments: Vec::new(),
        draft: v["isDraft"].as_bool().unwrap_or(false),
        auto_merge: v["autoMergeRequest"].is_object(),
    };
    let said = |v: &serde_json::Value| Remark {
        author: text(&v["author"]["login"]),
        body: text(&v["body"]),
        path: None,
        line: None,
    };
    for key in ["reviews", "comments"] {
        for c in v[key].as_array().into_iter().flatten() {
            if !c["body"].as_str().unwrap_or_default().trim().is_empty() {
                pr.comments.push(said(c));
            }
        }
    }
    // What was said on lines of the diff is not in `gh pr view`: it is asked of the API.
    let path = format!("repos/{{owner}}/{{repo}}/pulls/{}/comments", pr.number);
    if let Ok(out) = run_within(cwd, "gh", &["api", &path], Some(REMOTE_WAIT))
        && let Ok(serde_json::Value::Array(all)) = serde_json::from_str(&out)
    {
        for c in &all {
            pr.comments.push(Remark {
                author: text(&c["user"]["login"]),
                body: text(&c["body"]),
                path: c["path"].as_str().map(str::to_owned),
                line: c["line"]
                    .as_u64()
                    .or_else(|| c["original_line"].as_u64())
                    .map(|n| n as usize),
            });
        }
    }
    Some(pr)
}

/// Where the branch `branch`'s pull request stands: OPEN, MERGED or CLOSED, none when it has none.
pub fn pr_state(cwd: &Path, branch: &str) -> Option<String> {
    let out = run_within(
        cwd,
        "gh",
        &[
            "pr", "list", "--state", "all", "--head", branch, "--limit", "1", "--json", "state",
        ],
        Some(REMOTE_WAIT),
    )
    .ok()?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    v[0]["state"].as_str().map(str::to_owned)
}

/// The end of what the failed checks of the branch's pull request wrote, for the agent to read:
/// `gh run view --log-failed` of each failed run.
pub fn failed_logs(cwd: &Path, pr: &Pr) -> String {
    let mut out = String::new();
    for check in pr.failing() {
        out.push_str(&format!("## {}\n{}\n", check.name, check.url));
        // A check run's address names its run: …/actions/runs/<run>/job/<job>.
        let run = check
            .url
            .split("/runs/")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .filter(|id| id.chars().all(|c| c.is_ascii_digit()));
        if let Some(run) = run
            && let Ok(log) = run_within(cwd, "gh", &["run", "view", run, "--log-failed"], Some(REMOTE_WAIT))
        {
            let lines: Vec<&str> = log.lines().collect();
            let tail = &lines[lines.len().saturating_sub(FAILED_LOG_LINES)..];
            out.push_str("```\n");
            out.push_str(&tail.join("\n"));
            out.push_str("\n```\n");
        }
    }
    out
}

/// How much of a failed check's log goes to the agent: its end, where the failure is said.
const FAILED_LOG_LINES: usize = 120;

/// The line number in the new file of each line of `diff`, where it has one.
pub fn numbers(diff: &Diff) -> Vec<Option<usize>> {
    // A new file's diff has no hunk line to say where it starts: at its first line.
    let mut next = 1usize;
    diff.lines
        .iter()
        .map(|line| match line.sign {
            Sign::Hunk => {
                next = line
                    .text
                    .split_once('+')
                    .and_then(|(_, rest)| rest.split([',', ' ']).next()?.parse().ok())
                    .unwrap_or(0);
                None
            }
            Sign::Del => None,
            Sign::Add | Sign::Context => {
                let n = next;
                next += 1;
                Some(n)
            }
        })
        .collect()
}

/// Does `op`, and says what came of it.
pub fn apply(cwd: &Path, op: &Op, branch: Option<&str>) -> Result<String, String> {
    if *op == Op::Init {
        return git(cwd, &["init", "-q"]).map(|_| "git init".to_owned());
    }
    let top = top(cwd).ok_or_else(|| "not a git repository".to_owned())?;
    match op {
        Op::Stage(path) => git(&top, &["add", "--", path]),
        Op::Unstage(path) => git(&top, &["restore", "--staged", "--", path]),
        Op::Discard(path) => {
            let tracked = git(&top, &["ls-files", "--error-unmatch", "--", path]).is_ok();
            if tracked {
                git(
                    &top,
                    &["restore", "--staged", "--worktree", "--source=HEAD", "--", path],
                )
            } else {
                git(&top, &["clean", "-f", "-q", "--", path])
            }
        }
        Op::Commit(message) => {
            let staged = git(&top, &shown("diff", &["--cached", "--quiet"])).is_err();
            if !staged {
                git(&top, &["add", "-A"])?;
            }
            git(&top, &["commit", "-q", "-m", message])
                .map(|_| git(&top, &["log", "-1", "--format=%h %s"]).unwrap_or_default())
        }
        Op::Push => {
            let upstream = git(&top, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_ok();
            match (upstream, branch) {
                (true, _) => run_within(&top, "git", &["push", "-q"], Some(REMOTE_WAIT)),
                (false, Some(branch)) => run_within(
                    &top,
                    "git",
                    &["push", "-q", "-u", "--end-of-options", "origin", branch],
                    Some(REMOTE_WAIT),
                ),
                (false, None) => Err("no branch to push".into()),
            }
            .map(|out| if out.is_empty() { "pushed".to_owned() } else { out })
        }
        Op::Pr(None) => run_within(&top, "gh", &["pr", "create", "--fill"], Some(REMOTE_WAIT)),
        Op::Pr(Some((title, body))) => run_within(
            &top,
            "gh",
            &["pr", "create", "--title", title, "--body", body],
            Some(REMOTE_WAIT),
        ),
        Op::Hunk { path, act, at, head } => {
            let diff = raw_diff(&top, path, act.in_staged())?;
            let patch = hunk_patch(&diff, *at, head).ok_or_else(|| "the file changed since it was shown".to_owned())?;
            let args: &[&str] = match act {
                HunkAct::Stage => &["apply", "--cached", "-"],
                HunkAct::Unstage => &["apply", "--cached", "-R", "-"],
                HunkAct::Discard => &["apply", "-R", "-"],
            };
            exec(&top, "git", args, None, Some(&patch)).map(|out| out.trim().to_owned())
        }
        Op::Switch(branch) => git(&top, &["switch", "-q", "--end-of-options", branch]).map(|_| branch.clone()),
        Op::NewBranch(name) => git(&top, &["switch", "-q", "-c", name.trim()]).map(|_| name.trim().to_owned()),
        Op::Merge(how) => run_within(
            &top,
            "gh",
            &["pr", "merge", &format!("--{}", method(how))],
            Some(REMOTE_WAIT),
        )
        .map(|out| if out.is_empty() { "merged".to_owned() } else { out }),
        Op::AutoMerge(how) => run_within(
            &top,
            "gh",
            &["pr", "merge", "--auto", &format!("--{}", method(how))],
            Some(REMOTE_WAIT),
        )
        .map(|out| {
            if out.is_empty() {
                "auto-merge on".to_owned()
            } else {
                out
            }
        }),
        Op::DraftPr(title, body) => run_within(
            &top,
            "gh",
            &["pr", "create", "--draft", "--title", title, "--body", body],
            Some(REMOTE_WAIT),
        ),
        Op::Ready(true) => run_within(&top, "gh", &["pr", "ready"], Some(REMOTE_WAIT)),
        Op::Ready(false) => run_within(&top, "gh", &["pr", "ready", "--undo"], Some(REMOTE_WAIT)),
        Op::Init => unreachable!("made before the repository's top is looked for"),
        Op::Pull => run_within(&top, "git", &["pull", "-q", "--ff-only"], Some(REMOTE_WAIT))
            .map(|out| if out.is_empty() { "pulled".to_owned() } else { out }),
        Op::UpdateBranch => run_within(&top, "gh", &["pr", "update-branch"], Some(REMOTE_WAIT)),
    }
}

/// A merge method `gh` takes; a merge commit when it is not one.
fn method(how: &str) -> &str {
    match how {
        "squash" | "rebase" => how,
        _ => "merge",
    }
}

/// The branch the folder's work is compared with: the remote's default, else `main` or `master`.
fn base_of(top: &Path) -> Option<String> {
    git(top, &["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .ok()
        .filter(|b| !b.is_empty() && b != "origin/HEAD")
        .or_else(|| {
            ["origin/main", "origin/master", "main", "master"]
                .into_iter()
                .find(|b| git(top, &["rev-parse", "--verify", "--quiet", b]).is_ok())
                .map(str::to_owned)
        })
}

/// The files of a diff of several, each with its hunks.
pub fn split_diff(text: &str) -> Vec<(String, Diff)> {
    let mut files = Vec::new();
    for chunk in text
        .split("\ndiff --git ")
        .map(|c| c.strip_prefix("diff --git ").unwrap_or(c))
    {
        let Some(head) = chunk.lines().next() else {
            continue;
        };
        let Some((_, path)) = head.rsplit_once(" b/") else {
            continue;
        };
        let hunks: String = chunk
            .lines()
            .skip_while(|l| !l.starts_with("@@"))
            .map(|l| format!("{l}\n"))
            .collect();
        files.push((path.to_owned(), Diff::parse(&hunks)));
    }
    files
}

/// What a scope of the git tab shows: the files of the branch since its base, or of one commit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scoped {
    /// What it is compared with: the base, or the commit's parent.
    pub base: String,
    pub files: Vec<(String, Diff)>,
}

/// The branch's work since its base, as a pull request would show it.
pub fn branch_diff(cwd: &Path, whitespace: bool) -> Result<Scoped, String> {
    let top = top(cwd).ok_or_else(|| "not a git repository".to_owned())?;
    let base = base_of(&top).ok_or_else(|| "no branch to compare with".to_owned())?;
    let since = format!("{base}...HEAD");
    let mut args = shown("diff", &[]);
    if !whitespace {
        args.push("-w");
    }
    args.extend(["--end-of-options", since.as_str()]);
    Ok(Scoped {
        files: split_diff(&git(&top, &args)?),
        base,
    })
}

/// The branch's commits since its base, newest first; the last ones when it is the base.
pub fn commits(cwd: &Path) -> Vec<(String, String)> {
    let Some(top) = top(cwd) else {
        return Vec::new();
    };
    let range = base_of(&top).map(|b| format!("{b}..HEAD"));
    let mut args = vec!["log", "--format=%h%x1f%s", "-50"];
    if let Some(range) = &range {
        args.extend(["--end-of-options", range]);
    }
    let listed = |args: &[&str]| -> Vec<(String, String)> {
        git(&top, args)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('\u{1f}'))
            .map(|(h, s)| (h.to_owned(), s.to_owned()))
            .collect()
    };
    let found = listed(&args);
    if found.is_empty() && range.is_some() {
        return listed(&["log", "--format=%h%x1f%s", "-20"]);
    }
    found
}

/// What one commit changed.
pub fn commit_diff(cwd: &Path, sha: &str, whitespace: bool) -> Result<Scoped, String> {
    let top = top(cwd).ok_or_else(|| "not a git repository".to_owned())?;
    let mut args = shown("show", &["--format="]);
    if !whitespace {
        args.push("-w");
    }
    args.extend(["--end-of-options", sha]);
    Ok(Scoped {
        files: split_diff(&git(&top, &args)?),
        base: format!("{sha}^"),
    })
}

/// The working tree as a tree object, with the files git does not ignore that it does not track
/// yet: written through an index of its own, so the repository's index and files are left as they
/// are (the files' contents are kept as objects, as `git add` keeps them). A turn's start and end
/// are compared with these. None outside a repository.
pub fn snapshot(cwd: &Path) -> Option<String> {
    let top = top(cwd)?;
    let index = top.join(git(&top, &["rev-parse", "--git-path", "index"]).ok()?);
    // In a folder of its own that no other user can open, named so it cannot be guessed; git
    // writes its lock next to the index. Removed with all in it when done.
    let dir = private_dir("sasacode-turn-")?;
    let own = dir.path().join("index");
    // Begun from the repository's, git does not read again the files it already knows unchanged.
    if index.is_file() && std::fs::copy(&index, &own).is_err() {
        return None;
    }
    let env = [("GIT_INDEX_FILE", own.to_str()?)];
    let tree = exec_with(&top, "git", &["add", "-A", "."], None, None, &env)
        .and_then(|_| exec_with(&top, "git", &["write-tree"], None, None, &env));
    tree.ok().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
}

/// A new folder in the system's temporary one, named `prefix` and what cannot be guessed, that
/// only the user can open; removed with what is in it when dropped.
pub(crate) fn private_dir(prefix: &str) -> Option<tempfile::TempDir> {
    let mut b = tempfile::Builder::new();
    b.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        b.permissions(std::fs::Permissions::from_mode(0o700));
    }
    b.tempdir().ok()
}

/// What a turn changed: from the tree taken as it began to the one taken as it ended, or to the
/// working tree as it is now while it runs.
pub fn turn_diff(cwd: &Path, from: &str, to: Option<&str>, whitespace: bool) -> Result<Scoped, String> {
    let top = top(cwd).ok_or_else(|| "not a git repository".to_owned())?;
    let now;
    let to = match to {
        Some(to) => to,
        None => {
            now = snapshot(&top).ok_or_else(|| "git write-tree".to_owned())?;
            now.as_str()
        }
    };
    let mut args = shown("diff", &[]);
    if !whitespace {
        args.push("-w");
    }
    args.extend(["--end-of-options", from, to]);
    Ok(Scoped {
        files: split_diff(&git(&top, &args)?),
        base: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-git-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "test@example.invalid"],
            &["config", "user.name", "test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git(&dir, args).unwrap();
        }
        dir
    }

    #[test]
    fn a_turn_is_what_changed_between_two_snapshots_and_the_index_is_left_alone() {
        let dir = repo("turn");
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "build/\n").unwrap();
        git(&dir, &["add", "-A"]).unwrap();
        git(&dir, &["commit", "-q", "-m", "first"]).unwrap();
        // Staged before the turn: still staged after, and not part of what the turn changed.
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        git(&dir, &["add", "a.txt"]).unwrap();
        let from = snapshot(&dir).unwrap();
        // The turn: a file changed, one made (not tracked), one made where git ignores.
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(dir.join("new.txt"), "made\n").unwrap();
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("build/out"), "x").unwrap();
        let to = snapshot(&dir).unwrap();
        let turn = turn_diff(&dir, &from, Some(&to), true).unwrap();
        let files: Vec<&str> = turn.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(files, ["a.txt", "new.txt"]);
        assert_eq!(turn.files[0].1.added, 1, "only the turn's line");
        // While it runs, to the working tree as it is.
        assert_eq!(turn_diff(&dir, &from, None, true).unwrap(), turn);
        // The index is as it was: a.txt staged as before, new.txt not tracked.
        let status = git(&dir, &["status", "--porcelain"]).unwrap();
        assert!(status.contains("MM a.txt") && status.contains("?? new.txt"), "{status}");
        assert!(snapshot(&std::env::temp_dir().join("no-such-repo-here")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_commit_message_and_a_pull_request_are_written_from_the_change_and_the_branchs_commits() {
        let dir = repo("context");
        assert!(commit_context(&dir).is_err());
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        // Before the first commit: the new file is named.
        let (_, change) = commit_context(&dir).unwrap();
        assert!(change.contains("a.txt"));
        apply(&dir, &Op::Commit("feat: first".into()), None).unwrap();
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        let (log, change) = commit_context(&dir).unwrap();
        assert_eq!(log, "feat: first");
        assert!(change.contains("+two") && change.contains("New files:\nb.txt"));
        // What is staged, alone.
        git(&dir, &["add", "b.txt"]).unwrap();
        let (_, change) = commit_context(&dir).unwrap();
        assert!(change.contains("+b") && !change.contains("+two"));

        git(&dir, &["checkout", "-q", "-b", "topic"]).unwrap();
        apply(&dir, &Op::Commit("fix: one\n\nwhy".into()), None).unwrap();
        std::fs::write(dir.join("c.txt"), "c\n").unwrap();
        apply(&dir, &Op::Commit("fix: two".into()), None).unwrap();
        let pr = pr_context(&dir).unwrap();
        assert_eq!(pr.base, "main");
        assert_eq!(
            pr.commits,
            vec![("fix: one".into(), "why".into()), ("fix: two".into(), String::new())]
        );
        assert!(pr.stat.contains("c.txt") && pr.diff.contains("+c"));
        assert_eq!(pr.filled("topic"), ("topic".into(), "- fix: one\n- fix: two".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_branch_and_each_commit_are_diffed_since_the_base_and_a_folder_is_made_a_repository() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-git-{}-init", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!status(&dir).repo);
        apply(&dir, &Op::Init, None).unwrap();
        assert!(status(&dir).repo);
        let _ = std::fs::remove_dir_all(&dir);

        let dir = repo("scope");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        apply(&dir, &Op::Commit("base".into()), None).unwrap();
        assert_eq!(status(&dir).default_branch.as_deref(), Some("main"));
        git(&dir, &["checkout", "-q", "-b", "topic"]).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/b.rs"), "fn b() {}\n").unwrap();
        apply(&dir, &Op::Commit("add b".into()), None).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n  two\n").unwrap();
        apply(&dir, &Op::Commit("indent".into()), None).unwrap();

        let branch = branch_diff(&dir, true).unwrap();
        assert_eq!(branch.base, "main");
        let paths: Vec<&str> = branch.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "src/b.rs"]);
        assert_eq!((branch.files[1].1.added, branch.files[1].1.removed), (1, 0));
        // White space left out, the indent is no change.
        let paths: Vec<String> = branch_diff(&dir, false)
            .unwrap()
            .files
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(paths, vec!["src/b.rs"]);

        let commits = commits(&dir);
        let subjects: Vec<&str> = commits.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(subjects, vec!["indent", "add b"]);
        let one = commit_diff(&dir, &commits[1].0, true).unwrap();
        assert_eq!(one.files.len(), 1);
        assert_eq!(one.files[0].0, "src/b.rs");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_is_not_committed_is_listed_with_its_diff_and_can_be_committed() {
        let dir = repo("status");
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        apply(&dir, &Op::Commit("first".into()), None).unwrap();
        assert!(status(&dir).files.is_empty());

        std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.join("new file.txt"), "hello\n").unwrap();
        let s = status(&dir);
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert!(s.repo && !s.upstream);
        let paths: Vec<&str> = s.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.txt", "new file.txt"]);
        let a = &s.files[0];
        assert_eq!(a.counts(), (2, 1));
        assert!(a.staged_diff.is_empty());
        assert_eq!(numbers(&a.unstaged_diff), vec![None, Some(1), None, Some(2), Some(3)]);
        assert!(s.files[1].untracked() && s.files[1].unstaged_diff.added == 1);
        assert_eq!(numbers(&s.files[1].unstaged_diff), vec![Some(1)]);

        // Staged, then unstaged; discarded, the new file is gone.
        apply(&dir, &Op::Stage("a.txt".into()), None).unwrap();
        assert!(status(&dir).files[0].staged());
        apply(&dir, &Op::Unstage("a.txt".into()), None).unwrap();
        assert!(!status(&dir).files[0].staged());
        apply(&dir, &Op::Discard("new file.txt".into()), None).unwrap();
        assert!(!dir.join("new file.txt").exists());

        // With nothing staged, a commit takes everything.
        let said = apply(&dir, &Op::Commit("second".into()), None).unwrap();
        assert!(said.ends_with("second"));
        assert!(status(&dir).files.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_branch_is_pushed_to_its_remote_and_many_files_are_listed_without_reading_all() {
        let dir = repo("push");
        let bare = dir.with_extension("remote.git");
        let _ = std::fs::remove_dir_all(&bare);
        git(&dir, &["init", "-q", "--bare", bare.to_str().unwrap()]).unwrap();
        git(&dir, &["remote", "add", "origin", bare.to_str().unwrap()]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        apply(&dir, &Op::Commit("first".into()), None).unwrap();
        assert!(!status(&dir).upstream);
        apply(&dir, &Op::Push, Some("main")).unwrap();
        let s = status(&dir);
        assert!(s.upstream && s.ahead == 0);
        // A bare repository is read only when named.
        let named = ["--git-dir", bare.to_str().unwrap(), "log", "-1", "--format=%s", "main"];
        assert_eq!(git(&dir, &named).unwrap(), "first");

        for n in 0..DIFFED + 5 {
            std::fs::write(dir.join(format!("new{n:03}.txt")), "x\n").unwrap();
        }
        let s = status(&dir);
        assert_eq!(s.files.len(), DIFFED + 5);
        assert!(s.files[..DIFFED].iter().all(|f| f.unstaged_diff.added == 1));
        assert!(s.files[DIFFED..].iter().all(|f| f.unstaged_diff.lines.is_empty()));
        std::fs::remove_dir_all(dir).unwrap();
        std::fs::remove_dir_all(bare).unwrap();
    }

    #[test]
    fn one_hunk_is_staged_unstaged_or_discarded_and_the_others_stay() {
        let dir = repo("hunks");
        let lines: Vec<String> = (1..=30).map(|n| format!("line {n}")).collect();
        std::fs::write(dir.join("a.txt"), lines.join("\n") + "\n").unwrap();
        apply(&dir, &Op::Commit("first".into()), None).unwrap();
        // Two changes far apart: two hunks. The first is at a blank line's end of context.
        let mut changed = lines.clone();
        changed[1] = "LINE 2".into();
        changed[27] = String::new();
        std::fs::write(dir.join("a.txt"), changed.join("\n") + "\n").unwrap();
        let f = &status(&dir).files[0];
        let heads: Vec<String> = f
            .unstaged_diff
            .lines
            .iter()
            .filter(|l| l.sign == Sign::Hunk)
            .map(|l| l.text.clone())
            .collect();
        assert_eq!(heads.len(), 2);

        let hunk = |act, at: usize, head: &str| Op::Hunk {
            path: "a.txt".into(),
            act,
            at,
            head: head.to_owned(),
        };
        // The second staged: the first is still not.
        apply(&dir, &hunk(HunkAct::Stage, 1, &heads[1]), None).unwrap();
        let f = &status(&dir).files[0];
        assert!(f.staged() && f.unstaged());
        assert_eq!((f.staged_diff.added, f.unstaged_diff.added), (1, 1));
        assert!(
            f.staged_diff
                .lines
                .iter()
                .any(|l| l.sign == Sign::Add && l.text.is_empty())
        );

        // A hunk named by what it was before is not touched when it is no longer that one.
        assert!(apply(&dir, &hunk(HunkAct::Stage, 0, "@@ -1,9 +1,9 @@ nothing"), None).is_err());

        // Unstaged again, then the first thrown away: only the second change is left.
        let staged_head = status(&dir).files[0]
            .staged_diff
            .lines
            .iter()
            .find(|l| l.sign == Sign::Hunk)
            .map(|l| l.text.clone())
            .unwrap();
        apply(&dir, &hunk(HunkAct::Unstage, 0, &staged_head), None).unwrap();
        assert!(!status(&dir).files[0].staged());
        apply(&dir, &hunk(HunkAct::Discard, 0, &heads[0]), None).unwrap();
        // Git on Windows writes the lines back with CRLF.
        let now = std::fs::read_to_string(dir.join("a.txt"))
            .unwrap()
            .replace("\r\n", "\n");
        assert!(now.contains("line 2\n") && !now.contains("line 28\n"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_branch_is_made_switched_to_and_listed_newest_first() {
        let dir = repo("branches");
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        apply(&dir, &Op::Commit("first".into()), None).unwrap();
        apply(&dir, &Op::NewBranch(" topic ".into()), None).unwrap();
        assert_eq!(status(&dir).branch.as_deref(), Some("topic"));
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        apply(&dir, &Op::Commit("second".into()), None).unwrap();
        assert_eq!(branches(&dir), vec!["topic", "main"]);
        apply(&dir, &Op::Switch("main".into()), None).unwrap();
        assert_eq!(status(&dir).branch.as_deref(), Some("main"));
        assert!(apply(&dir, &Op::Switch("nowhere".into()), None).is_err());
        // A branch's name is never taken as an option: no such branch, and HEAD stays on main.
        assert!(apply(&dir, &Op::Switch("--detach".into()), None).is_err());
        assert_eq!(status(&dir).branch.as_deref(), Some("main"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_checks_state_is_read_from_a_check_run_or_a_status() {
        let v = |s: &str| check(&serde_json::from_str(s).unwrap());
        let run = v(
            r#"{"__typename":"CheckRun","name":"rust","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://github.com/o/r/actions/runs/12/job/34"}"#,
        );
        assert_eq!((run.name.as_str(), run.state), ("rust", CheckState::Failed));
        assert_eq!(
            v(r#"{"name":"a","status":"IN_PROGRESS","conclusion":""}"#).state,
            CheckState::Pending
        );
        assert_eq!(
            v(r#"{"name":"a","status":"COMPLETED","conclusion":"SUCCESS"}"#).state,
            CheckState::Passed
        );
        assert_eq!(
            v(r#"{"name":"a","status":"COMPLETED","conclusion":"SKIPPED"}"#).state,
            CheckState::Skipped
        );
        let status = v(r#"{"__typename":"StatusContext","context":"ci/x","state":"ERROR","targetUrl":"u"}"#);
        assert_eq!(
            (status.name.as_str(), status.state, status.url.as_str()),
            ("ci/x", CheckState::Failed, "u")
        );
    }

    #[test]
    fn a_hunks_patch_has_the_files_lines_and_that_hunk_only() {
        let diff = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n@@ -9 +9 @@ ctx\n-c\n+d\n \n";
        let patch = hunk_patch(diff, 1, "@@ -9 +9 @@ ctx").unwrap();
        assert_eq!(
            patch,
            "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -9 +9 @@ ctx\n-c\n+d\n \n"
        );
        assert!(hunk_patch(diff, 1, "@@ -1 +1 @@").is_none());
        assert!(hunk_patch(diff, 2, "@@ -9 +9 @@ ctx").is_none());
    }

    #[test]
    fn a_program_that_waits_too_long_is_stopped() {
        let dir = std::env::temp_dir();
        #[cfg(unix)]
        {
            let start = std::time::Instant::now();
            let said = run_within(&dir, "sleep", &["5"], Some(Duration::from_millis(200)));
            assert!(said.is_err_and(|e| e.contains("gave up")));
            assert!(start.elapsed() < Duration::from_secs(3));
        }
        assert!(run_within(&dir, "git", &["--version"], Some(REMOTE_WAIT)).is_ok_and(|v| v.starts_with("git")));
    }

    #[test]
    fn a_folder_outside_a_repository_has_none() {
        let dir = std::env::temp_dir().join(format!("sasacode-gui-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The temp folder may itself be inside a repository on some machines; only say what is
        // certain.
        if top(&dir).is_none() {
            assert_eq!(status(&dir), Status::default());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_options_given_to_git_are_those_the_shipped_plugins_give() {
        let bundled = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../sasacode-bundled/src/git_args.rs"
        ));
        let quoted = |all: &[&str]| all.iter().map(|a| format!("{a:?}")).collect::<Vec<_>>().join(", ");
        for (name, ours) in [("GUARD", &GUARD[..]), ("NO_DIFF_HELPERS", &NO_DIFF_HELPERS[..])] {
            let line = format!("pub const {name}: [&str; {}] = [{}];", ours.len(), quoted(ours));
            assert!(bundled.contains(&line), "{line}");
        }
        let c = command();
        let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(args, GUARD);
        assert_eq!(
            shown("diff", &["x"]),
            ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "x"]
        );
    }

    /// A repository whose config starts a program that leaves `marker` when git reads the working
    /// tree, with a change not committed.
    #[cfg(unix)]
    fn monitored(repo: &Path, marker: &Path) {
        std::fs::create_dir_all(repo).unwrap();
        git(repo, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(repo, &["add", "-A"]).unwrap();
        let who = [
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
        ];
        git(repo, &[&who[..], &["commit", "-q", "-m", "first"]].concat()).unwrap();
        std::fs::write(repo.join("a.txt"), "two\n").unwrap();
        let command = format!("touch '{}'; false", marker.display());
        git(repo, &["config", "core.fsmonitor", &command]).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reading_a_repository_starts_no_program_its_config_names() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, marker) = (dir.path().join("repo"), dir.path().join("marker"));
        monitored(&repo, &marker);
        // git as anyone runs it there does start it: what follows is not idle.
        let plain = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(plain.status.success());
        if !marker.exists() {
            eprintln!("this git does not start a config's fsmonitor; nothing to compare with");
            return;
        }
        std::fs::remove_file(&marker).unwrap();
        let read = status(&repo);
        assert!(read.repo && read.files.len() == 1, "{read:?}");
        assert!(snapshot(&repo).is_some());
        let _ = commit_context(&repo);
        let _ = branches(&repo);
        let _ = commits(&repo);
        let _ = crate::composer::project_files(&repo, true);
        assert!(!marker.exists(), "a program from the repository's config ran");
        // The git another program (gh) runs is told the same through its environment.
        if std::env::var_os("GIT_CONFIG_COUNT").is_none() {
            let mut c = Command::new("git");
            guard_env(&mut c);
            let out = c.args(["status", "--porcelain"]).current_dir(&repo).output().unwrap();
            assert!(out.status.success());
            assert!(
                !marker.exists(),
                "a program from the repository's config ran under gh's git"
            );
        }
    }

    #[test]
    fn a_bare_repository_in_the_folder_is_not_read_unless_named() {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("b.git");
        Command::new("git")
            .args(["init", "-q", "--bare"])
            .arg(&bare)
            .output()
            .unwrap();
        // Found there by a git not told otherwise; not by the window's.
        let plain = Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&bare)
            .output()
            .unwrap();
        if plain.status.success() {
            assert!(git(&bare, &["rev-parse", "--git-dir"]).is_err());
        }
        let named = ["--git-dir", bare.to_str().unwrap(), "rev-parse", "--is-bare-repository"];
        assert_eq!(git(dir.path(), &named).as_deref(), Ok("true"));
    }
}
