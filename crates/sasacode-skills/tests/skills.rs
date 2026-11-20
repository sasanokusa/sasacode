//! packages/skills/test/skills.test.ts.

mod support;

use std::path::{Path, PathBuf};

use sasacode_plugin_api::{CommandContext, InjectWhen};
use sasacode_skills::{Skill, SkillsOptions, create_skills_plugin, discover_skills, parse_frontmatter};
use support::FakeHost;

fn skill(dir: &Path, name: &str, fm: &str, body: &str) {
    std::fs::create_dir_all(dir.join(name)).unwrap();
    std::fs::write(dir.join(name).join("SKILL.md"), format!("---\n{fm}\n---\n\n{body}\n")).unwrap();
}

#[test]
fn discovery_needs_name_and_description_later_dirs_override() {
    let root = tempfile::tempdir().unwrap();
    let g = root.path().join("global");
    let p = root.path().join("project");
    skill(&g, "a", "name: alpha\ndescription: global alpha", "Do the thing.");
    skill(&g, "b", "name: beta", "Do the thing."); // no description: skipped
    skill(&p, "a2", "name: alpha\ndescription: project alpha", "Do the thing.");
    let found = discover_skills(&[g, p.clone(), root.path().join("missing")]).unwrap();
    assert_eq!(
        found,
        vec![Skill {
            name: "alpha".into(),
            description: "project alpha".into(),
            path: p.join("a2").join("SKILL.md")
        }]
    );
}

/// The read rules the plugin added. The TS test asked the agent's permission policy whether
/// `read` of the file is allowed; the policy lives in sasacode-agent, so this checks the rule.
fn read_rules(host: &FakeHost) -> Vec<String> {
    host.rules().into_iter().flat_map(|r| r.allow).collect()
}

#[tokio::test]
async fn plugin_lists_skills_in_the_system_prompt_allows_reading_them_and_skill_name_injects_the_body() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("home-skills");
    skill(
        &dir,
        "deploy",
        "name: deploy\ndescription: Deploy the app to staging",
        "Run make deploy.",
    );
    let host = FakeHost::new(root.path().join("proj"), serde_json::json!({}));
    host.load(
        "skills",
        create_skills_plugin(vec![dir.clone()], SkillsOptions::default()),
    )
    .await
    .unwrap();
    let prompt = host.system_prompt("base").await;
    let path = dir.join("deploy").join("SKILL.md");
    assert!(prompt.contains(&format!("- deploy: Deploy the app to staging ({})", path.display())));
    assert!(!prompt.contains("make deploy")); // body is not preloaded
    assert_eq!(read_rules(&host), vec![format!("read({}/**)", dir.display())]);
    let cmd = host.command("skill:deploy").unwrap();
    (cmd.run)(CommandContext { args: String::new() }).await.unwrap();
    // Queued skill content is delivered with the next prompt.
    let injected = host.injected();
    assert_eq!(injected.len(), 1);
    assert_eq!(injected[0].1, InjectWhen::Next);
    assert!(injected[0].0.starts_with(&format!(
        "Use the \"deploy\" skill ({}). Its instructions:\n\n---",
        path.display()
    )));
    assert!(injected[0].0.ends_with("Run make deploy.\n"));
    assert_eq!(
        host.notes(),
        vec!["skill deploy は次のメッセージと一緒に送られます".to_string()]
    );
    // With a task it goes now.
    (cmd.run)(CommandContext { args: "ship it".into() }).await.unwrap();
    let (text, when) = host.injected().pop().unwrap();
    assert_eq!(when, InjectWhen::Now);
    assert!(text.ends_with("Run make deploy.\n\n\nTask: ship it"));
}

#[tokio::test]
async fn built_in_tool_authoring_skill_is_written_out_listed_and_callable() {
    let root = tempfile::tempdir().unwrap();
    let builtin_dir = root.path().join("builtin");
    let host = FakeHost::new(root.path(), serde_json::json!({}));
    let opts = SkillsOptions {
        builtin_dir: Some(builtin_dir.clone()),
    };
    host.load("skills", create_skills_plugin(vec![], opts.clone()))
        .await
        .unwrap();
    let path: PathBuf = builtin_dir.join("tool-authoring").join("SKILL.md");
    let fm = parse_frontmatter(&std::fs::read_to_string(&path).unwrap());
    assert_eq!(fm.get("name").map(String::as_str), Some("tool-authoring"));
    assert!(host.command_names().contains(&"skill:tool-authoring".to_string()));
    // The whole description: /help wraps it, the completion list shortens it to the width (#22).
    let description = fm.get("description").unwrap();
    assert!(description.encode_utf16().count() > 80);
    assert_eq!(&host.command("skill:tool-authoring").unwrap().description, description);
    assert_eq!(read_rules(&host), vec![format!("read({}/**)", builtin_dir.display())]);
    // A user skill with the same name wins over the built-in.
    let user = root.path().join("user-skills");
    skill(
        &user,
        "ta",
        "name: tool-authoring\ndescription: my own version",
        "Do the thing.",
    );
    let host2 = FakeHost::new(root.path(), serde_json::json!({}));
    host2
        .load("skills", create_skills_plugin(vec![user], opts))
        .await
        .unwrap();
    assert_eq!(
        host2.command("skill:tool-authoring").map(|c| c.description),
        Some("my own version".to_string())
    );
}

#[tokio::test]
async fn the_skills_command_and_an_empty_set() {
    let root = tempfile::tempdir().unwrap();
    // No skills at all: nothing is registered, no rule, no prompt change.
    let host = FakeHost::new(root.path(), serde_json::json!({}));
    host.load(
        "skills",
        create_skills_plugin(vec![root.path().join("none")], SkillsOptions::default()),
    )
    .await
    .unwrap();
    assert!(host.command_names().is_empty());
    assert!(host.rules().is_empty());
    assert_eq!(host.system_prompt("base").await, "base");
    // /skills lists name, description and path.
    let dir = root.path().join("s");
    skill(&dir, "x", "name: x\ndescription: does x", "body");
    let host = FakeHost::new(root.path(), serde_json::json!({}));
    host.load(
        "skills",
        create_skills_plugin(vec![dir.clone()], SkillsOptions::default()),
    )
    .await
    .unwrap();
    (host.command("skills").unwrap().run)(CommandContext::default())
        .await
        .unwrap();
    assert_eq!(
        host.notes(),
        vec![format!("x — does x\n  {}", dir.join("x").join("SKILL.md").display())]
    );
}

/// The project's skills folder is read before the project is trusted: it counts only as files of
/// its own inside the project, while the global folder may hold links.
#[cfg(unix)]
#[tokio::test]
async fn project_skills_are_read_only_from_files_of_the_projects_own() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let (global, outside) = (root.join("global"), root.join("outside"));
    skill(&outside, "far", "name: far\ndescription: from elsewhere", "Far away.");
    std::fs::create_dir_all(&global).unwrap();
    symlink(outside.join("far"), global.join("far")).unwrap();

    // The whole folder is a link: no skills from it, and no read rule for it.
    let linked = root.join("linked");
    std::fs::create_dir_all(linked.join(".sasacode")).unwrap();
    symlink(&outside, linked.join(".sasacode/skills")).unwrap();
    // .sasacode is a link: the same.
    let dot = root.join("dot");
    std::fs::create_dir_all(&dot).unwrap();
    std::fs::create_dir_all(root.join("dot-elsewhere/skills")).unwrap();
    skill(
        &root.join("dot-elsewhere/skills"),
        "near",
        "name: near\ndescription: near",
        "Near.",
    );
    symlink(root.join("dot-elsewhere"), dot.join(".sasacode")).unwrap();
    for proj in [&linked, &dot] {
        let host = FakeHost::new(proj.clone(), serde_json::json!({}));
        let dirs = vec![global.clone(), proj.join(".sasacode/skills")];
        host.load("skills", create_skills_plugin(dirs, SkillsOptions::default()))
            .await
            .unwrap();
        let prompt = host.system_prompt("base").await;
        assert!(
            prompt.contains("- far: from elsewhere"),
            "the global folder's link is kept"
        );
        assert!(!prompt.contains("- near:"));
        assert_eq!(read_rules(&host), vec![format!("read({}/**)", global.display())]);
    }

    // A folder of its own: its own skills are read; a skill folder or a SKILL.md that is a link is not.
    let proj = root.join("proj");
    let own = proj.join(".sasacode/skills");
    skill(&own, "mine", "name: mine\ndescription: the project's", "Mine.");
    symlink(outside.join("far"), own.join("far2")).unwrap();
    std::fs::create_dir_all(own.join("file-link")).unwrap();
    symlink(outside.join("far/SKILL.md"), own.join("file-link/SKILL.md")).unwrap();
    let host = FakeHost::new(proj.clone(), serde_json::json!({}));
    host.load(
        "skills",
        create_skills_plugin(vec![own.clone()], SkillsOptions::default()),
    )
    .await
    .unwrap();
    let prompt = host.system_prompt("base").await;
    assert!(prompt.contains("- mine: the project's"));
    assert!(!prompt.contains("- far:"));
    assert_eq!(read_rules(&host), vec![format!("read({}/**)", own.display())]);
}
