//! permission-presets (`packages/bundled/src/permission-presets.ts`): named rule sets, enabled with
//! plugins.settings["permission-presets"].presets (default: guard).

use std::sync::OnceLock;

use futures::future::BoxFuture;
use regex::Regex;
use serde_json::Value;

use sasacode_agent::permission::chain_re;
use sasacode_host::t;
use sasacode_plugin_api::{
    Command, Decision, NotifyLevel, PermissionResult, PermissionRules, PluginApi, VerdictSource, hook,
};

use crate::repair::js::{js_string, js_trim};

/// A named rule set.
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    /// In Japanese, as the TS wrote it; shown through `t()`.
    pub description: &'static str,
    pub allow: &'static [&'static str],
    pub ask: &'static [&'static str],
    pub deny: &'static [&'static str],
    pub soft_deny: &'static [&'static str],
}

impl Preset {
    pub fn rules(&self) -> PermissionRules {
        let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect();
        PermissionRules {
            allow: v(self.allow),
            ask: v(self.ask),
            deny: v(self.deny),
            soft_deny: v(self.soft_deny),
        }
    }
}

/// The presets, in the order the TS object listed them.
pub const PRESETS: &[(&str, Preset)] = &[
    (
        "guard",
        Preset {
            description: "取り返しのつかない操作を常に拒否（sudo、ルートの rm -rf、ディスク操作、パイプで sh に流す等）。ホーム配下の rm -rf と force push は softDeny（jev-guard などが確認に回せる）。ssh・scp・sftp・リモートへの rsync は常に確認",
            allow: &[],
            deny: &[
                "bash(sudo *)",
                "bash(rm -rf /)",
                "bash(rm -rf /*)",
                "bash(mkfs*)",
                "bash(dd *of=/dev/*)",
                "bash(sh)",
                "bash(bash)",
                "bash(zsh)",
                "bash(chmod -R 777 /*)",
            ],
            // Broad patterns that also catch legitimate calls (rm -rf ~/proj/build, a force push to
            // one's own branch): still denied, unless a permission plugin hands them to the user.
            soft_deny: &[
                "bash(rm -rf ~*)",
                "bash(rm -rf $HOME*)",
                "bash(git push --force*)",
                "bash(git push -f*)",
            ],
            // Other machines: even a read there is outside what the harness (or Jev, which sees only
            // the command line) can check, so always ask, in auto mode too.
            ask: &["bash(ssh *)", "bash(scp *)", "bash(sftp *)", "bash(rsync *:*)"],
        },
    ),
    (
        "read-only-shell",
        Preset {
            description: "読み取りだけのシェルコマンドを確認なしで許可（ls, cat, rg, git status/diff/log など）。書き出しやコマンドの実行をするオプション（rg --pre、git diff --output、tree -o など）、それを作りうるシェル展開、.git以下の編集は確認に回す。catなどは作業ディレクトリの外のファイルも読める",
            allow: &[
                // A word followed by `*` also matches a longer word, so each rule is the command alone
                // or the command, a space and its arguments.
                "bash(ls)",
                "bash(ls *)",
                "bash(pwd)",
                "bash(cat *)",
                "bash(head *)",
                "bash(tail *)",
                "bash(wc *)",
                "bash(rg *)",
                "bash(grep *)",
                "bash(tree)",
                "bash(tree *)",
                "bash(file *)",
                "bash(which *)",
                "bash(git status)",
                "bash(git status *)",
                "bash(git diff)",
                "bash(git diff *)",
                "bash(git log)",
                "bash(git log *)",
                "bash(git show)",
                "bash(git show *)",
                "bash(git branch)",
                "bash(git rev-parse)",
                "bash(git rev-parse *)",
            ],
            // git diff/log/show/status run what .git/config names (diff.external, textconv, core.fsmonitor): an edit there,
            // which edits mode allows in the project, would make the allowed git commands run a program unasked (#116).
            ask: &["edit(**/.git/**)", "write(**/.git/**)"],
            deny: &[],
            soft_deny: &[],
        },
    ),
    (
        "tests",
        Preset {
            description: "よくあるテスト・型チェックコマンドを確認なしで許可",
            // Each is the command alone or the command, a space and its arguments: `cargo test*` would
            // also run a `cargo test-foo` found on PATH.
            allow: &[
                "bash(bun test)",
                "bash(bun test *)",
                "bash(npm test)",
                "bash(npm test *)",
                "bash(pnpm test)",
                "bash(pnpm test *)",
                "bash(yarn test)",
                "bash(yarn test *)",
                "bash(cargo test)",
                "bash(cargo test *)",
                "bash(go test)",
                "bash(go test *)",
                "bash(pytest)",
                "bash(pytest *)",
                "bash(tsc --noEmit)",
                "bash(tsc --noEmit *)",
                "bash(bun run typecheck)",
                "bash(bun run typecheck *)",
            ],
            ask: &[],
            deny: &[],
            soft_deny: &[],
        },
    ),
];

/// The preset called `name`.
pub fn preset(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|(_, p)| p)
}

/// Options that turn a read-only-shell command into one that writes files or runs programs. The
/// allow rules match by prefix, so these are handed back to the user. Tokens are compared without
/// quotes and backslashes; git also takes unambiguous prefixes of long options (--outp=…). One of
/// these commands with a shell expansion in it goes back to the user as well: bash expands
/// ${X:+--output=…}, $'--output=…' and {--output=…,x} into the option before the command runs.
fn writes_or_runs_table() -> &'static [(Regex, Regex)] {
    static T: OnceLock<Vec<(Regex, Regex)>> = OnceLock::new();
    T.get_or_init(|| {
        [
            (r"^rg$", r"^--(pre|hostname-bin)(=|$)"),
            (r"^git (diff|log|show)$", r"^--(ou|ex)"),
            (r"^tree$", r"^(-[a-zA-Z]*[oR]|--(o|fromfile))"),
            (r"^file$", r"^(-[a-zA-Z]*C|--c)"),
        ]
        .iter()
        // INFALLIBLE: constant patterns.
        .map(|(c, o)| (Regex::new(c).expect("valid regex"), Regex::new(o).expect("valid regex")))
        .collect()
    })
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    // INFALLIBLE: constant patterns.
    cell.get_or_init(|| Regex::new(pattern).expect("valid regex"))
}

pub fn writes_or_runs(command: &str) -> bool {
    static QUOTES: OnceLock<Regex> = OnceLock::new();
    static SPACE: OnceLock<Regex> = OnceLock::new();
    chain_re().split(command).any(|piece| {
        let bare = re(&QUOTES, r#"["'\\]"#).replace_all(js_trim(piece), "");
        let words: Vec<&str> = re(&SPACE, r"\s+").split(&bare).collect();
        writes_or_runs_table().iter().any(|(cmd, opt)| {
            let first_two = words.iter().take(2).copied().collect::<Vec<_>>().join(" ");
            let name = if cmd.is_match(&first_two) {
                2
            } else if cmd.is_match(words.first().copied().unwrap_or("")) {
                1
            } else {
                0
            };
            name > 0 && (expands(piece) || words.iter().skip(name).any(|w| opt.is_match(w)))
        })
    })
}

/// Does bash change `piece` before the command sees it ($VAR, ${…}, $'…', `…`, {a,b}, {1..3})?
/// Single quotes keep it as written (#116).
fn expands(piece: &str) -> bool {
    static SINGLE: OnceLock<Regex> = OnceLock::new();
    static DOUBLE: OnceLock<Regex> = OnceLock::new();
    static BRACE: OnceLock<Regex> = OnceLock::new();
    let bare = re(&SINGLE, r"'[^']*'").replace_all(piece, "''"); // $'…' keeps its $
    bare.contains(['$', '`'])
        || re(&BRACE, r"\{[^{}]*(,|\.\.)[^{}]*\}").is_match(&re(&DOUBLE, r#""[^"]*""#).replace_all(&bare, r#""""#))
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let enabled: Vec<String> = match api.settings().get("presets") {
            Some(Value::Array(a)) => a.iter().map(js_string).collect(),
            _ => vec!["guard".to_string()],
        };
        for name in &enabled {
            match preset(name) {
                None => {
                    let known = PRESETS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ");
                    api.ui().notify(
                        &t!(
                            "unknown permission preset \"{name}\" ({known})",
                            name = name,
                            known = known
                        ),
                        NotifyLevel::Warning,
                    );
                }
                Some(p) => api.add_permission_rules(p.rules())?,
            }
        }
        if enabled.iter().any(|n| n == "read-only-shell") {
            api.on::<hook::Permission, _, _>(|ev, _c| {
                // Only calls a rule allowed: auto mode (and the user's own asks) are left as they are.
                let out = (ev.tool.name() == "bash"
                    && ev.verdict.decision == Decision::Allow
                    && ev.verdict.source == VerdictSource::Rule
                    && writes_or_runs(&ev.args.get("command").filter(|v| !v.is_null()).map(js_string).unwrap_or_default()))
                .then(|| PermissionResult {
                    decision: Some(Decision::Ask),
                    reason: Some(
                        "read-only-shell: this option writes files or runs programs (or a shell expansion could make one)"
                            .to_string(),
                    ),
                });
                async move { Ok(out) }
            });
        }
        let a = api.clone();
        api.register_command(Command::new(
            "presets",
            &t!("権限プリセットの一覧"),
            move |_c| {
                let lines: Vec<String> = PRESETS
                    .iter()
                    .map(|(n, p)| {
                        let on = if enabled.iter().any(|e| e == n) { "●" } else { "○" };
                        format!("{on} {n} — {}", t!(p.description))
                    })
                    .chain(std::iter::once(t!(
                        "有効化: config の plugins.settings[\"permission-presets\"].presets"
                    )))
                    .collect();
                a.ui().notify(&lines.join("\n"), NotifyLevel::Info);
                async { Ok(()) }
            },
        ));
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_that_write_or_run_and_expansions_that_could_make_them() {
        assert!(writes_or_runs("rg --pre=./x foo"));
        assert!(writes_or_runs("git diff --outp=/tmp/x"));
        assert!(writes_or_runs("tree -fo out.txt"));
        assert!(writes_or_runs("ls && rg ${X:+--pre=sh} foo"));
        assert!(writes_or_runs("rg {--pre=sh,x} foo"));
        assert!(!writes_or_runs("rg 'a{b,c}' foo"));
        assert!(!writes_or_runs("rg foo && git status"));
    }
}
