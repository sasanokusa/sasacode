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
