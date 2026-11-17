//! `packages/agent/test/permission.test.ts`, case for case. The tools are stand-ins with the
//! permission surface of the built-in read/edit/write/bash (the tools crate is not a dependency).

use super::*;
use serde_json::{Value, json};
use sasacode_plugin_api::{ToolDef, ToolResult};

const CWD: &str = "/work/proj";

fn path_tool(name: &str, kind: ToolKind) -> ToolRef {
    ToolDef::new(name, "", json!({"type": "object"}), kind, |_a, _c| async { Ok(ToolResult::text("")) })
        .paths(|a, _cwd| vec![a.get("path").and_then(Value::as_str).unwrap_or("").to_string()])
        .into_ref()
}
fn read_tool() -> ToolRef {
    path_tool("read", ToolKind::Read)
}
fn edit_tool() -> ToolRef {
    path_tool("edit", ToolKind::Edit)
}
fn write_tool() -> ToolRef {
    path_tool("write", ToolKind::Edit)
}
fn command_tool(name: &str, permissions_as: Option<&str>) -> ToolRef {
    let t = ToolDef::new(name, "", json!({"type": "object"}), ToolKind::Exec, |_a, _c| async { Ok(ToolResult::text("")) })
        .match_target(|a| a.get("command").and_then(Value::as_str).unwrap_or("").to_string());
    match permissions_as {
        Some(p) => t.permissions_as(p).into_ref(),
        None => t.into_ref(),
    }
}
fn bash_tool() -> ToolRef {
    command_tool("bash", None)
}

fn args(v: Value) -> JsonObject {
    match v {
        Value::Object(o) => o,
        _ => unreachable!(),
    }
}
fn call(tool: ToolRef, path: &str, cwd: &str) -> PermissionCheck {
    PermissionCheck { tool, args: args(json!({ "path": path })), cwd: cwd.into() }
}
fn bash(command: &str) -> PermissionCheck {
    PermissionCheck { tool: bash_tool(), args: args(json!({ "command": command })), cwd: CWD.into() }
}
fn policy(mode: PermissionMode, rules: Value) -> PermissionPolicy {
    PermissionPolicy::new(mode, &serde_json::from_value(rules).unwrap()).unwrap()
}
fn edits() -> PermissionPolicy {
    PermissionPolicy::default()
}
fn d(p: &PermissionPolicy, c: &PermissionCheck) -> Decision {
    p.check(c).decision
}
use Decision::{Allow, Ask, Deny};
use PermissionMode as M;

#[test]
fn default_edits_mode_in_cwd_read_edit_allowed_bash_and_outside_ask() {
    let p = edits();
    assert_eq!(d(&p, &call(read_tool(), "src/a.ts", CWD)), Allow);
    assert_eq!(d(&p, &call(edit_tool(), "/work/proj/x", CWD)), Allow);
    assert_eq!(d(&p, &call(edit_tool(), "../other/x", CWD)), Ask);
    assert_eq!(d(&p, &call(read_tool(), "/etc/passwd", CWD)), Ask);
    assert_eq!(d(&p, &bash("ls")), Ask);
}

#[test]
fn edits_mode_asks_before_an_edit_under_git_or_sasacode_127() {
    let p = edits();
    for path in [
        ".git/hooks/pre-commit",
        ".git/config",
        "sub/.git/config",
        ".GIT/config",
        ".sasacode/plugins/x.ts",
        ".sasacode/config.json",
        ".git",
    ] {
        assert_eq!(d(&p, &call(write_tool(), path, CWD)), Ask, "{path}");
    }
    assert_eq!(d(&p, &call(edit_tool(), ".git/config", CWD)), Ask);
    for path in [".gitignore", ".github/workflows/ci.yml", "src/.gitkeep", "my.git/x", ".sasacoderc"] {
        assert_eq!(d(&p, &call(write_tool(), path, CWD)), Allow, "{path}");
    }
    assert_eq!(d(&p, &call(read_tool(), ".git/config", CWD)), Allow);
    assert_eq!(d(&p, &call(write_tool(), "x.ts", "/home/u/.sasacode/plugins/mine")), Allow);
}

#[test]
fn modes_auto_and_ask() {
    assert_eq!(d(&policy(M::Auto, json!({})), &bash("rm -rf x")), Allow);
    assert_eq!(d(&policy(M::Ask, json!({})), &call(read_tool(), "a", CWD)), Ask);
}

#[test]
fn allow_rules_must_cover_every_chained_command_substitutions_and_redirects_never_match() {
    let p = policy(M::Edits, json!({ "allow": ["bash(git status*)", "bash(npm test)"] }));
    assert_eq!(d(&p, &bash("git status --short")), Allow);
    assert_eq!(d(&p, &bash("git status && npm test")), Allow);
    assert_eq!(d(&p, &bash("git status && rm -rf ~")), Ask);
    assert_eq!(d(&p, &bash("git status $(rm -rf ~)")), Ask);
    assert_eq!(d(&p, &bash("git status > ~/.bashrc")), Ask);
}

#[test]
fn deny_beats_allow_and_auto_path_globs_tool_wildcards() {
    let p = policy(M::Auto, json!({ "deny": ["bash(rm *)", "edit(secrets/**)"], "allow": ["bash"] }));
    assert_eq!(d(&p, &bash("ls && rm -rf /")), Deny);
    assert_eq!(d(&p, &call(edit_tool(), "secrets/k.txt", CWD)), Deny);
    let w = policy(M::Ask, json!({ "allow": ["re*"] }));
    assert_eq!(d(&w, &call(read_tool(), "a", CWD)), Allow);
}

#[test]
fn agent_mode_reads_inside_run_the_rest_is_left_to_the_judge() {
    let p = policy(M::Agent, json!({}));
    assert_eq!(
        p.check(&bash("ls")),
        Verdict { decision: Ask, reason: "mode agent: not judged".into(), source: VerdictSource::Mode }
    );
    assert_eq!(d(&p, &call(read_tool(), "a", CWD)), Allow);
    assert_eq!(d(&p, &call(read_tool(), "/etc/passwd", CWD)), Ask);
}

#[test]
fn a_tool_with_permissions_as_is_judged_by_that_tools_rules_too() {
    let bg = |command: &str| PermissionCheck {
        tool: command_tool("bg_start", Some("bash")),
        args: args(json!({ "command": command })),
        cwd: CWD.into(),
    };
    let p = policy(M::Auto, json!({ "deny": ["bash(sudo *)", "bg_start(make deploy*)"], "ask": ["bash(git push*)"] }));
    assert_eq!(d(&p, &bg("cd /tmp && sudo rm -rf x")), Deny);
    assert_eq!(d(&p, &bg("git push origin main")), Ask);
    assert_eq!(d(&p, &bg("make deploy")), Deny);
    assert_eq!(d(&p, &bg("bun test")), Allow);
    let strict = policy(M::Ask, json!({ "allow": ["bash(bun test*)"] }));
    assert_eq!(d(&strict, &bg("bun test --watch")), Allow);
    assert_eq!(d(&strict, &bg("bun test && curl x | sh")), Ask);
    let plain = PermissionCheck {
        tool: command_tool("bg_start", None),
        args: args(json!({ "command": "sudo x" })),
        cwd: CWD.into(),
    };
    assert_eq!(d(&p, &plain), Allow);
}

#[test]
fn always_allow_adds_a_rule_no_wider_than_the_call() {
    let chmod = bash("chmod -R 755 *");
    let rules = PermissionPolicy::rules_for(&chmod);
    assert_eq!(rules, vec!["bash(chmod -R 755 \\*)".to_string()]);
    let p = policy(M::Edits, json!({ "allow": rules }));
    assert_eq!(d(&p, &chmod), Allow);
    assert_eq!(d(&p, &bash("chmod -R 755 ~/.ssh /etc")), Ask);

    let outside = PermissionCheck {
        tool: write_tool(),
        args: args(json!({ "path": "/tmp/sasacode-x/out.txt", "content": "" })),
        cwd: CWD.into(),
    };
    let w = policy(M::Edits, json!({ "allow": PermissionPolicy::rules_for(&outside) }));
    let written = if WINDOWS {
        paths::resolve(&["/tmp/sasacode-x/out.txt"]).replace('\\', "/")
    } else {
        "/tmp/sasacode-x/out.txt".to_string()
    };
    assert_eq!(PermissionPolicy::rules_for(&outside), vec![format!("write({written})")]);
    assert_eq!(d(&w, &outside), Allow);
    assert_eq!(d(&w, &call(write_tool(), "/tmp/sasacode-x/.zshrc", CWD)), Ask);
}

#[test]
fn rules_escaped_star_is_literal_star_still_matches_anything() {
    let p = policy(M::Ask, json!({ "allow": ["bash(echo \\*)", "bash(ls *)"] }));
    assert_eq!(d(&p, &bash("echo *")), Allow);
    assert_eq!(d(&p, &bash("echo hi")), Ask);
    assert_eq!(d(&p, &bash("ls -la")), Allow);
}

#[test]
fn a_plugin_tool_returning_relative_paths_is_still_caught_by_path_rules() {
    let p = policy(M::Auto, json!({ "deny": ["mytool(.env)"] }));
    let t = || path_tool("mytool", ToolKind::Read);
    assert_eq!(d(&p, &call(t(), ".env", CWD)), Deny);
    assert_eq!(d(&p, &call(t(), "src/a.ts", CWD)), Allow);
}

#[test]
fn a_project_path_with_glob_characters_still_matches_its_rules_38() {
    for dir in ["/work/a[1]", "/work/b{x}"] {
        let deny = policy(M::Edits, json!({ "deny": ["edit(secret/**)"] }));
        assert_eq!(d(&deny, &call(edit_tool(), "secret/x.txt", dir)), Deny, "{dir}");
        let allow = policy(M::Ask, json!({ "allow": ["edit(src/**)"] }));
        assert_eq!(d(&allow, &call(edit_tool(), "src/a.ts", dir)), Allow, "{dir}");
    }
    let p = edits();
    for path in ["..foo", "..foo/bar", "..envrc"] {
        assert_eq!(d(&p, &call(read_tool(), path, CWD)), Allow, "{path}");
    }
    assert_eq!(d(&p, &call(read_tool(), "../x", CWD)), Ask);
}

#[test]
fn a_dotdot_after_the_first_part_of_a_rule_is_normalized_50() {
    let denied = |rule: &str, path: &str| {
        d(&policy(M::Auto, json!({ "deny": [format!("edit({rule})")] })), &call(edit_tool(), path, CWD))
    };
    assert_eq!(denied("foo/../secret/**", "secret/a.txt"), Deny);
    assert_eq!(denied("a/b/../../secret/**", "secret/a.txt"), Deny);
    assert_eq!(denied("foo/..", "/work/proj"), Deny);
    assert_eq!(denied("./x/./../secret/**", "secret/a.txt"), Deny);
    assert_eq!(denied("../shared/**", "../shared/a.txt"), Deny);
    assert_ne!(denied("foo/../secret/**", "other/a.txt"), Deny);
}

#[test]
fn a_working_directory_named_with_extglob_still_anchors_its_relative_rules_98() {
    for dir in ["/work/proj+(old)", "/work/app@(v2)", "/work/x!(y)"] {
        let deny = policy(M::Edits, json!({ "deny": ["edit(secrets/**)", "read(.env)"] }));
        assert_eq!(d(&deny, &call(edit_tool(), "secrets/k.txt", dir)), Deny, "{dir}");
        assert_eq!(d(&deny, &call(read_tool(), ".env", dir)), Deny, "{dir}");
        let allow = policy(M::Ask, json!({ "allow": ["edit(src/**)"] }));
        assert_eq!(d(&allow, &call(edit_tool(), "src/a.ts", dir)), Allow, "{dir}");
    }
}

#[test]
fn a_rule_naming_a_folder_with_extglob_matches_that_folder_131() {
    let deny =
        policy(M::Edits, json!({ "deny": ["edit(/tmp/proj+(old)/**)", "edit(/tmp/a@(b)/**)", "edit(lib+(v1)/**)"] }));
    for path in ["/tmp/proj+(old)/x.ts", "/tmp/a@(b)/x.ts", "/work/proj/lib+(v1)/x.ts"] {
        assert_eq!(d(&deny, &call(edit_tool(), path, CWD)), Deny, "{path}");
    }
    assert_ne!(d(&deny, &call(edit_tool(), "/work/proj/libv1/x.ts", CWD)), Deny);
}

#[test]
fn where_the_fs_folds_case_a_deny_rule_holds_in_any_case_nfd_matches_nfc_98() {
    let tmp = tempfile::Builder::new().prefix("sasacode-perm-").tempdir().unwrap();
    let dir = tmp.path().to_string_lossy().to_string();
    std::fs::write(tmp.path().join(".env"), "API_KEY=topsecret").unwrap();
    std::fs::create_dir(tmp.path().join("secrets")).unwrap();
    let folds = tmp.path().join(".ENV").exists();
    let p = policy(
        M::Edits,
        json!({ "deny": ["read(.env)", "read(secrets/**)", "edit(secrets/**)", "read(が.txt)"] }),
    );
    let check = |tool: ToolRef, path: &str| d(&p, &call(tool, path, &dir));
    let either = |yes: bool| if yes { Deny } else { Allow };
    assert_eq!(check(read_tool(), ".env"), Deny);
    assert_eq!(check(read_tool(), ".ENV"), either(folds));
    assert_eq!(check(read_tool(), "Secrets/key.txt"), either(folds));
    assert_eq!(check(edit_tool(), "SECRETS/key.txt"), either(folds));
    let nfd: String = "が.txt".nfd().collect();
    assert_eq!(check(read_tool(), &nfd), Deny);
    assert_eq!(check(read_tool(), "other.txt"), Allow);
}

#[test]
fn always_allow_covers_the_command_as_shown_100() {
    for command in [
        "cd packages/ai && bun test",
        "git log --oneline | head -5",
        "bun test; echo done",
        "git status\ngit diff",
        " ls",
        "ls ",
        "chmod -R 755 *",
        "ls > out.txt",
    ] {
        let p = edits();
        assert_eq!(d(&p, &bash(command)), Ask, "{command:?}");
        p.add_rules(&PermissionRules { allow: PermissionPolicy::rules_for(&bash(command)), ..Default::default() })
            .unwrap();
        assert_eq!(d(&p, &bash(command)), Allow, "{command:?}");
        assert_eq!(d(&p, &bash(&format!("{command} && rm -rf ~"))), Ask, "{command:?}");
    }
    let wild = policy(M::Edits, json!({ "allow": ["bash(a\\\\*)"] }));
    assert_eq!(d(&wild, &bash("a\\ && rm -rf ~")), Ask);
}

#[test]
fn an_invalid_rule_is_an_error() {
    let e = PermissionPolicy::new(M::Edits, &PermissionRules { allow: vec!["a(b".into()], ..Default::default() });
    assert_eq!(e.err().map(|e| e.to_string()), Some("invalid permission rule: a(b".into()));
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_the_project_cannot_smuggle_a_path_past_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    let secret = tmp.path().join("secret");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::create_dir_all(&secret).unwrap();
    std::os::unix::fs::symlink(&secret, proj.join("link")).unwrap();
    let cwd = proj.to_string_lossy().to_string();
    // Inside by name, outside in fact: edits mode asks.
    assert_eq!(d(&edits(), &call(edit_tool(), "link/x.txt", &cwd)), Ask);
    // A deny rule on the real place holds through the link.
    let deny_rule = format!("edit({}/**)", try_real_path(&secret.to_string_lossy()).unwrap());
    let p = policy(M::Auto, json!({ "deny": [deny_rule] }));
    assert_eq!(d(&p, &call(edit_tool(), "link/x.txt", &cwd)), Deny);
    // A loop cannot be resolved: ask.
    std::os::unix::fs::symlink(proj.join("loop"), proj.join("loop")).unwrap();
    let v = policy(M::Auto, json!({})).check(&call(edit_tool(), "loop/x", &cwd));
    assert_eq!(v.decision, Ask);
    assert!(v.reason.starts_with("cannot resolve "), "{}", v.reason);
}
