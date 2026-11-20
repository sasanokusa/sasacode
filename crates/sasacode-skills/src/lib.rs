//! Agent Skills (`packages/skills/src/index.ts`): SKILL.md folders found in the global and project
//! skills directories, listed in the system prompt by name and description, and `/skill:<name>`
//! commands that send a skill's instructions along with the next message.

pub mod builtin;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;

use sasacode_plugin_api::hooks::{SystemPromptEvent, SystemPromptResult};
use sasacode_plugin_api::{
    Command, CommandContext, InjectWhen, Lang, NotifyLevel, PermissionRules, PluginApi, hook, sasacode_home,
};

pub use builtin::{BUILTIN_SKILLS, materialize_builtin_skills};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

/// JS `\s` (and what `String.prototype.trim` removes): ASCII whitespace, the line terminators,
/// NBSP, BOM and the Unicode space separators. Not U+0085, which Rust's `char::is_whitespace` has.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// What JS `.` does not match.
fn is_js_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// The text between the opening `---` line and the next line starting with `---`
/// (`/^---\r?\n([\s\S]*?)\r?\n---/`).
fn frontmatter_block(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?;
    let body = rest.strip_prefix("\r\n").or_else(|| rest.strip_prefix('\n'))?;
    // The lazy group stops at the first "\n---", leaving a "\r" before it out.
    let end = body.find("\n---")?;
    let end = if end > 0 && body.as_bytes()[end - 1] == b'\r' {
        end - 1
    } else {
        end
    };
    Some(&body[..end])
}

/// `/^([A-Za-z0-9_-]+):\s*(.*)$/` on one line: the key and the raw value.
fn key_value(line: &str) -> Option<(&str, &str)> {
    let key_len = line
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        .count();
    if key_len == 0 {
        return None;
    }
    let after = line[key_len..].strip_prefix(':')?;
    let value = after.trim_start_matches(is_js_space);
    // `(.*)$` must reach the end of the line without crossing a line terminator.
    if value.contains(is_js_line_terminator) {
        return None;
    }
    Some((&line[..key_len], value))
}

/// Minimal YAML frontmatter reader for the standard fields (plain, quoted, or folded `>` / `|` scalars).
pub fn parse_frontmatter(text: &str) -> IndexMap<String, String> {
    let mut out = IndexMap::new();
    let Some(block) = frontmatter_block(text) else {
        return out;
    };
    let lines: Vec<&str> = block.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut i = 0;
    while i < lines.len() {
        let Some((key, raw)) = key_value(lines[i]) else {
            i += 1;
            continue;
        };
        let mut value = js_trim(raw).to_string();
        if matches!(value.as_str(), ">" | "|" | ">-" | "|-") {
            // `/^\s+\S|^\s*$/`: an indented line or a blank one continues the block.
            let mut block: Vec<&str> = Vec::new();
            while i + 1 < lines.len() && (lines[i + 1].is_empty() || lines[i + 1].starts_with(is_js_space)) {
                i += 1;
                block.push(js_trim(lines[i]));
            }
            let sep = if value.starts_with('>') { " " } else { "\n" };
            value = js_trim(&block.join(sep)).to_string();
        } else {
            value = unquote(&value).to_string();
        }
        out.insert(key.to_string(), value);
        i += 1;
    }
    out
}

/// `value.replace(/^(["'])(.*)\1$/, "$2")`: the same quote at both ends comes off.
fn unquote(value: &str) -> &str {
    let mut chars = value.chars();
    match (chars.next(), chars.next_back()) {
        (Some(q @ ('"' | '\'')), Some(last)) if last == q => &value[1..value.len() - 1],
        _ => value,
    }
}

/// Folders holding a SKILL.md with a name and a description. A later directory (project) overrides
/// an earlier one (global) for a skill of the same name, which keeps its place in the list.
pub fn discover_skills(dirs: &[PathBuf]) -> std::io::Result<Vec<Skill>> {
    discover_skills_in(dirs, None)
}

/// [`discover_skills`], with `project` (the project's skills folder, read before the project is
/// trusted) taken only as files of its own: the folder is skipped when it or `.sasacode` is a link,
/// and so is a skill folder or SKILL.md in it that is a link, as it could put any file into the prompt.
pub fn discover_skills_in(dirs: &[PathBuf], project: Option<&Path>) -> std::io::Result<Vec<Skill>> {
    let mut by_name: IndexMap<String, Skill> = IndexMap::new();
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        let own = project == Some(dir.as_path());
        if own && !in_place(dir) {
            continue;
        }
        // Sorted by name, as Node's readdirSync returns them (libuv sorts scandir), so the result
        // does not depend on the file system's own order.
        let mut entries: Vec<std::ffi::OsString> = std::fs::read_dir(dir)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<_, _>>()?;
        entries.sort();
        for entry in entries {
            let path = dir.join(&entry).join("SKILL.md");
            if !std::fs::metadata(&path).is_ok_and(|m| m.is_file())
                || own && (is_link(&dir.join(entry)) || is_link(&path))
            {
                continue;
            }
            let fm = parse_frontmatter(&String::from_utf8_lossy(&std::fs::read(&path)?));
            let (Some(name), Some(description)) = (fm.get("name"), fm.get("description")) else {
                continue;
            };
            if name.is_empty() || description.is_empty() {
                continue;
            }
            by_name.insert(
                name.clone(),
                Skill {
                    name: name.clone(),
                    description: description.clone(),
                    path,
                },
            );
        }
    }
    Ok(by_name.into_values().collect())
}

#[derive(Debug, Clone, Default)]
pub struct SkillsOptions {
    /// Where the built-in skills are written out (`~/.sasacode/builtin-skills`).
    pub builtin_dir: Option<PathBuf>,
}

/// A plugin set up with its configuration, ready for the plugin host to load.
pub use sasacode_plugin_api::PluginInit;

/// Agent Skills adapter (`createSkillsPlugin`): only name, description and path go into the system
/// prompt; the model reads SKILL.md with the read tool when it needs it (progressive disclosure, A3).
pub fn create_skills_plugin(dirs: Vec<PathBuf>, opts: SkillsOptions) -> PluginInit {
    Box::new(move |api| Box::pin(skills_plugin(api, dirs, opts)))
}

/// The project's skills folder, `<cwd>/.sasacode/skills`, unless that is the user's own (`home_skills`, when
/// run from the home folder).
pub fn project_skills_dir(cwd: &Path, home_skills: &Path) -> Option<PathBuf> {
    Some(cwd.join(".sasacode").join("skills")).filter(|p| p != home_skills)
}

/// Whether `p` is a symbolic link, or on Windows a junction or any other reparse point.
fn is_link(p: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(p) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// `<cwd>/.sasacode/skills` as a folder of its own: it exists, neither it nor `.sasacode` is a link (or
/// a junction), and its real path is still `<cwd>/.sasacode/skills`. One that is not there is not
/// "in place": a link made later at that path must not be covered by what was decided now.
fn in_place(project: &Path) -> bool {
    let Some(cwd) = project.parent().and_then(Path::parent) else {
        return false;
    };
    if is_link(project) || project.parent().is_some_and(is_link) {
        return false;
    }
    match (std::fs::canonicalize(project), std::fs::canonicalize(cwd)) {
        // Spelled as the file system has it: where it folds case, `.sasacode/Skills` is the same folder.
        (Ok(real), Ok(cwd)) => {
            let rest: Vec<_> = real.strip_prefix(&cwd).into_iter().flat_map(Path::components).collect();
            real.is_dir()
                && matches!(rest.as_slice(), [a, b]
                    if a.as_os_str().eq_ignore_ascii_case(".sasacode") && b.as_os_str().eq_ignore_ascii_case("skills"))
        }
        _ => false,
    }
}

fn en(api: &PluginApi) -> bool {
    api.ui().lang() == Lang::En
}

/// The body of [`create_skills_plugin`].
pub async fn skills_plugin(api: PluginApi, mut dirs: Vec<PathBuf>, opts: SkillsOptions) -> anyhow::Result<()> {
    if let Some(builtin_dir) = &opts.builtin_dir {
        // Built-ins come first so a user or project skill with the same name overrides them.
        match materialize_builtin_skills(builtin_dir) {
            Ok(()) => dirs.insert(0, builtin_dir.clone()),
            Err(e) => {
                let what = if en(&api) {
                    "built-in skills unavailable"
                } else {
                    "組み込みの Skills を使えません"
                };
                api.ui().notify(&format!("{what}: {e}"), NotifyLevel::Warning);
            }
        }
    }
    // The project's folder counts only where it really is <cwd>/.sasacode/skills: through a link it
    // could be any folder, and the read rule below would cover all of it.
    let project = project_skills_dir(&api.cwd(), &sasacode_home().join("skills"));
    dirs.retain(|d| project.as_ref() != Some(d) || in_place(d));
    let skills = discover_skills_in(&dirs, project.as_deref())?;
    if skills.is_empty() {
        return Ok(());
    }
    // Skill files live outside the project too (~/.sasacode/skills); reading them must not prompt every time.
    api.add_permission_rules(PermissionRules {
        allow: dirs.iter().map(|d| format!("read({}/**)", d.display())).collect(),
        ..Default::default()
    })?;

    let mut listing = vec![
        "Agent Skills are available. Each is a folder with a SKILL.md holding instructions; when a task matches a skill's description, read its SKILL.md with the read tool first and follow it.".to_string(),
    ];
    listing.extend(
        skills
            .iter()
            .map(|s| format!("- {}: {} ({})", s.name, s.description, s.path.display())),
    );
    let listing = listing.join("\n");
    api.on::<hook::SystemPrompt, _, _>(move |SystemPromptEvent { prompt }, _| {
        let prompt = format!("{prompt}\n\n{listing}");
        async move { Ok(Some(SystemPromptResult { prompt: Some(prompt) })) }
    });

    for s in &skills {
        let s = Arc::new(s.clone());
        let cmd_api = api.clone();
        let run_skill = s.clone();
        // In full: /help wraps it, and the completion list shortens it to the width with an ellipsis.
        let command = Command::new(
            &format!("skill:{}", s.name),
            &s.description,
            move |CommandContext { args }| {
                let api = cmd_api.clone();
                let s = run_skill.clone();
                async move { run_skill_command(&api, &s, &args).await }
            },
        )
        .hint("[task]");
        api.register_command(command);
    }
    let list_api = api.clone();
    let skills = Arc::new(skills);
    let description = if en(&api) {
        "list the available skills"
    } else {
        "利用できる Skills の一覧"
    };
    api.register_command(Command::new("skills", description, move |_| {
        let api = list_api.clone();
        let skills = skills.clone();
        async move {
            let text: Vec<String> = skills
                .iter()
                .map(|s| format!("{} — {}\n  {}", s.name, s.description, s.path.display()))
                .collect();
            api.ui().notify(&text.join("\n"), NotifyLevel::Info);
            Ok(())
        }
    }));
    Ok(())
}

async fn run_skill_command(api: &PluginApi, s: &Skill, args: &str) -> anyhow::Result<()> {
    let body = String::from_utf8_lossy(&tokio::fs::read(&s.path).await?).into_owned();
    let task = if args.is_empty() {
        String::new()
    } else {
        format!("\n\nTask: {args}")
    };
    api.session().inject_text(
        &format!(
            "Use the \"{}\" skill ({}). Its instructions:\n\n{body}{task}",
            s.name,
            s.path.display()
        ),
        if args.is_empty() {
            InjectWhen::Next
        } else {
            InjectWhen::Now
        },
    );
    if args.is_empty() {
        let note = if en(api) {
            format!("skill {} goes with your next message", s.name)
        } else {
            format!("skill {} は次のメッセージと一緒に送られます", s.name)
        };
        api.ui().notify(&note, NotifyLevel::Info);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fm(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    // skills.test.ts: "frontmatter: plain, quoted and folded values"
    #[test]
    fn frontmatter_plain_quoted_and_folded_values() {
        assert_eq!(
            parse_frontmatter("---\nname: pdf\ndescription: \"Work with PDFs: merge, split\"\n---\nbody"),
            fm(&[("name", "pdf"), ("description", "Work with PDFs: merge, split")])
        );
        assert_eq!(
            parse_frontmatter("---\nname: x\ndescription: >\n  line one\n  line two\nlicense: MIT\n---"),
            fm(&[("name", "x"), ("description", "line one line two"), ("license", "MIT")])
        );
    }

    #[test]
    fn frontmatter_edges_follow_the_ts_regexes() {
        // No frontmatter, or one that never closes.
        assert!(parse_frontmatter("name: x").is_empty());
        assert!(parse_frontmatter("---\nname: x\n").is_empty());
        // CRLF files.
        assert_eq!(
            parse_frontmatter("---\r\nname: x\r\ndescription: 'y'\r\n---\r\n"),
            fm(&[("name", "x"), ("description", "y")])
        );
        // A literal block keeps its lines; blank lines continue it; a lone or mismatched quote stays.
        assert_eq!(
            parse_frontmatter("---\na: |\n  one\n\n  two\nb: \"\nc: 'x\"\n---"),
            fm(&[("a", "one\n\ntwo"), ("b", "\""), ("c", "'x\"")])
        );
        // Japanese text and full-width spaces trim like JS.
        assert_eq!(
            parse_frontmatter("---\nname:\u{3000}資料\u{3000}\n---"),
            fm(&[("name", "資料")])
        );
        // Not a key line: skipped.
        assert_eq!(
            parse_frontmatter("---\n# comment\nname x\nname: y\n---"),
            fm(&[("name", "y")])
        );
    }
}
