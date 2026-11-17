//! What the git tab compares: the changes not committed, the branch's work since its base, or one
//! commit; how a diff is drawn (one column or two, white space or not); files folded, and files
//! marked as viewed.

use iced::Task;

use crate::app::{App, Msg};

/// What the git tab shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Scope {
    /// What is not committed, staged or not.
    #[default]
    Changes,
    /// The branch since its base.
    Branch,
    /// One commit, by its short hash.
    Commit(String),
}

/// A file's diff as it was viewed: its path and a hash of its lines, so that a change to it shows
/// it again.
pub fn viewed_key(path: &str, diff: &crate::diff::Diff) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for line in &diff.lines {
        line.text.hash(&mut h);
        (line.sign as u8).hash(&mut h);
    }
    format!("{path}#{:x}", h.finish())
}

impl App {
    /// What the scope shows, read again.
    pub(crate) fn load_scope(&mut self) -> Task<Msg> {
        let cwd = self.cwd.clone();
        let whitespace = !self.prefs.more.hide_whitespace;
        let scope = self.git_scope.clone();
        if scope == Scope::Changes {
            self.scoped = None;
            return Task::none();
        }
        self.scope_loading = true;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    let commits = crate::git::commits(&cwd);
                    let scoped = match &scope {
                        Scope::Commit(sha) => crate::git::commit_diff(&cwd, sha, whitespace),
                        _ => crate::git::branch_diff(&cwd, whitespace),
                    };
                    (scope, commits, scoped)
                })
                .await
                .map_err(|e| e.to_string())
            },
            |r| match r {
                Ok((scope, commits, scoped)) => Msg::Scoped(scope, commits, scoped.map(Box::new)),
                Err(e) => Msg::Scoped(Scope::Branch, Vec::new(), Err(e)),
            },
        )
    }

    /// Whether a file of the scope was marked as viewed, as it is now.
    pub(crate) fn viewed(&self, key: &str) -> bool {
        let branch = self.git.branch.clone().unwrap_or_default();
        self.prefs.more.viewed.get(&branch).is_some_and(|v| v.contains(key))
    }

    /// The scopes' messages; any other comes back.
    pub(crate) fn update_scope(&mut self, msg: Msg) -> Result<Task<Msg>, Msg> {
        let task = match msg {
            Msg::GitScope(scope) => {
                self.git_scope = scope;
                self.load_scope()
            }
            Msg::Scoped(scope, commits, scoped) => {
                self.scope_loading = false;
                self.scope_commits = commits;
                // An answer for a scope left meanwhile is not shown.
                if scope == self.git_scope {
                    self.scoped = Some(scoped.map(|s| *s));
                }
                Task::none()
            }
            Msg::FoldFiles(fold) => {
                // The scope's files, whose cards fold by their paths.
                let paths: Vec<String> = match (&self.git_scope, &self.scoped) {
                    (Scope::Changes, _) => self.git.files.iter().map(|f| f.path.clone()).collect(),
                    (_, Some(Ok(scoped))) => scoped.files.iter().map(|(p, _)| p.clone()).collect(),
                    _ => Vec::new(),
                };
                for path in paths {
                    if fold {
                        self.chat.closed.insert(path);
                    } else {
                        self.chat.closed.remove(&path);
                    }
                }
                Task::none()
            }
            Msg::SetSplitDiff(on) => {
                let _ = self.set(|p| p.more.split_diff = on);
                Task::none()
            }
            Msg::SetWhitespace(hide) => {
                let _ = self.set(|p| p.more.hide_whitespace = hide);
                self.load_scope()
            }
            Msg::Viewed(key) => {
                let branch = self.git.branch.clone().unwrap_or_default();
                let path = key.split('#').next().unwrap_or_default().to_owned();
                let _ = self.set(|p| {
                    let set = p.more.viewed.entry(branch).or_default();
                    if !set.remove(&key) {
                        // A file viewed before, changed since, is marked as it is now.
                        set.retain(|k| k.split('#').next() != Some(path.as_str()));
                        set.insert(key);
                    }
                });
                Task::none()
            }
            Msg::SetMergeMethod(how) => {
                let _ = self.set(|p| p.more.merge_method = how.to_owned());
                Task::none()
            }
            other => return self.update_extras(other),
        };
        Ok(task)
    }
}

/// The lines of a diff side by side: what was on the left, what is on the right, a removed line
/// beside the line added in its place.
pub fn side_by_side(diff: &crate::diff::Diff) -> Vec<(Option<&crate::diff::Line>, Option<&crate::diff::Line>)> {
    use crate::diff::Sign;
    type Row<'d> = (Option<&'d crate::diff::Line>, Option<&'d crate::diff::Line>);
    fn flush<'d>(
        rows: &mut Vec<Row<'d>>,
        dels: &mut Vec<&'d crate::diff::Line>,
        adds: &mut Vec<&'d crate::diff::Line>,
    ) {
        let n = dels.len().max(adds.len());
        for i in 0..n {
            rows.push((dels.get(i).copied(), adds.get(i).copied()));
        }
        dels.clear();
        adds.clear();
    }
    let mut rows = Vec::new();
    let (mut dels, mut adds): (Vec<&crate::diff::Line>, Vec<&crate::diff::Line>) = (Vec::new(), Vec::new());
    for line in &diff.lines {
        match line.sign {
            Sign::Del => dels.push(line),
            Sign::Add => adds.push(line),
            Sign::Context | Sign::Hunk => {
                flush(&mut rows, &mut dels, &mut adds);
                rows.push((Some(line), Some(line)));
            }
        }
    }
    flush(&mut rows, &mut dels, &mut adds);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{Diff, Sign};

    #[test]
    fn a_removed_line_is_put_beside_the_line_added_in_its_place() {
        let diff = Diff::parse("@@ -1,3 +1,3 @@\n a\n-b\n-c\n+B\n d\n");
        let rows = side_by_side(&diff);
        let signs: Vec<(Option<Sign>, Option<Sign>)> = rows
            .iter()
            .map(|(l, r)| (l.map(|l| l.sign), r.map(|r| r.sign)))
            .collect();
        assert_eq!(
            signs,
            vec![
                (Some(Sign::Hunk), Some(Sign::Hunk)),
                (Some(Sign::Context), Some(Sign::Context)),
                (Some(Sign::Del), Some(Sign::Add)),
                (Some(Sign::Del), None),
                (Some(Sign::Context), Some(Sign::Context)),
            ]
        );
    }

    #[test]
    fn a_changed_file_is_viewed_again() {
        let one = Diff::parse("@@ -1 +1 @@\n-a\n+b\n");
        let two = Diff::parse("@@ -1 +1 @@\n-a\n+c\n");
        assert_ne!(viewed_key("x.rs", &one), viewed_key("x.rs", &two));
        assert_eq!(viewed_key("x.rs", &one), viewed_key("x.rs", &one.clone()));
    }
}
