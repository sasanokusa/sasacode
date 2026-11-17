//! Plan mode: a run that looks and plans, and changes nothing. The CLI has no such mode; the
//! window makes one of what it has. The run is asked under `ask` with reads allowed, its system
//! prompt says to end with a plan, and the window denies each write and edit it asks for. Commands
//! are put to the user as under `ask`: one may be a search, or may change something. Once a plan
//! is written, the window offers to go ahead with it in the mode chosen before.
//!
//! What it cannot stop: the CLI looks at the allow rules of its config files before the mode, so a
//! call one of them allows (`permissions.allow` with `edit(src/**)`, say) runs unasked here too;
//! and the CLI has no flag to deny one.

use std::path::{Path, PathBuf};

use crate::app::App;
use crate::runner::RunSpec;

/// The name plan mode has among the permission modes.
pub const MODE: &str = "plan";

/// What the run is told, after its own system prompt.
const PROMPT: &str = "\
# Plan mode

The user chose plan mode for this run. Look into the task and write a plan; change nothing.

- Read the files that matter. Commands that only look (searches, listings, `git status`, `git log`,
  `git diff`) may help; the user is asked before any command runs.
- Do not write or edit files: those calls are denied in plan mode. Do not change anything another
  way either (shell redirection, `sed -i`, commits, installs).
- End with the plan: numbered steps, each naming the files to change and what changes in them;
  then the risks and open questions; then how the result will be checked.
- Keep it short enough to review, and write it in the language the user writes in.

When the user has read the plan they tell you to go ahead; only then are changes made.
";

/// Whether plan mode denies a call of `tool`: what writes files.
pub fn denies(tool: &str) -> bool {
    matches!(tool, "write" | "edit")
}

/// `spec` as a plan run: asked under `ask`, reads allowed and nothing else, the prompt added from
/// `file`. What "always" allowed in the conversation's earlier runs is not passed: a command once
/// allowed, `git *` say, could change something, and in plan mode each command is asked.
pub fn shape(spec: &mut RunSpec, file: Option<PathBuf>) {
    spec.permission = Some("ask".into());
    spec.allow = vec!["read".into()];
    spec.system_file = file;
}

/// Writes the prompt to `path` unless it holds it already: the file the run is pointed at.
fn write_prompt(path: &Path) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|now| now == PROMPT) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, PROMPT)
}

impl App {
    /// The file plan mode's prompt is in, under sasacode's home; none when it cannot be written,
    /// and the run is still asked under `ask` with its writes denied.
    pub(crate) fn plan_file(&self) -> Option<PathBuf> {
        let path = self.home.as_ref()?.join("gui").join("plan-mode.md");
        write_prompt(&path).ok().map(|()| path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_writes_files_is_denied() {
        assert!(denies("write"));
        assert!(denies("edit"));
        assert!(!denies("read"));
        assert!(!denies("bash"));
    }

    #[test]
    fn a_plan_run_is_asked_with_reads_allowed_and_nothing_else() {
        let mut spec = RunSpec {
            program: "sasacode".into(),
            cwd: PathBuf::from("/tmp"),
            prompt: String::new(),
            resume: None,
            model: None,
            permission: Some("plan".into()),
            thinking: None,
            no_session: false,
            lang: None,
            allow: vec![
                "bash(npm test)".into(),
                "edit(src/**)".into(),
                "*".into(),
                "write".into(),
            ],
            trust_project: false,
            slash: false,
            system_file: None,
        };
        shape(&mut spec, Some(PathBuf::from("/home/gui/plan-mode.md")));
        assert_eq!(spec.permission.as_deref(), Some("ask"));
        assert_eq!(spec.allow, ["read"]);
        let args = spec.args();
        assert!(args.windows(2).any(|w| w == ["--permission", "ask"]));
        assert!(args.windows(2).any(|w| w == ["--allow", "read"]));
        assert!(
            args.windows(2)
                .any(|w| w == ["--append-system-prompt-file", "/home/gui/plan-mode.md"])
        );
    }

    #[test]
    fn the_prompt_is_written_once_and_kept() {
        let dir = std::env::temp_dir().join(format!("sasacode-plan-{}", std::process::id()));
        let path = dir.join("gui").join("plan-mode.md");
        write_prompt(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), PROMPT);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        write_prompt(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), before);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
