//! workflows (`plugins/workflows/index.ts` and `prompts.ts`): reusable requests run with
//! `/workflow <name> [request]`. Off until `plugins.enabled` names it.

use std::sync::Arc;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use serde_json::{Value, json};

use sasacode_plugin_api::{
    Command, InjectWhen, Lang, NotifyLevel, PluginApi, SelectOption, ShowTextOptions, TextFormat,
};

use crate::project_checks::valid_name;
use crate::repair::js::{js_space, js_trim, utf16_len};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workflow {
    pub description: String,
    pub prompt: String,
}

fn w(description: &str, prompt: &str) -> Workflow {
    Workflow {
        description: description.into(),
        prompt: prompt.into(),
    }
}

/// The built-in workflows in `lang` (`prompts.ts`).
pub fn builtins(lang: Lang) -> IndexMap<String, Workflow> {
    let list = if lang == Lang::Ja {
        [
            (
                "review",
                w(
                    "変更差分をレビューする",
                    "現在の変更をレビューしてください。{{input}}\nまず対象の差分と周辺コードを読み、動作不良、回帰、データ損失につながる問題を優先してください。git_context があれば活用してください。指摘は根拠となるファイル・行と再現条件を添え、確実な問題と未確認の懸念を区別してください。指摘がなければその旨と未検証の範囲を示してください。レビューのためにコードを書き換えたりコミットしたりしないでください。",
                ),
            ),
            (
                "diagnose",
                w(
                    "失敗を再現して原因を調べる",
                    "次の問題を調査してください。{{input}}\nエラーや失敗条件を確認し、利用できる project_checks またはプロジェクトの文書から必要最小限の再現手順を選んでください。実行時の権限確認には従ってください。観測事実と仮説を分け、原因を絞り込んで最小限の修正案と検証方法を提示してください。まだ修正は適用せず、再現できなければ試したことと不足情報を報告してください。",
                ),
            ),
            (
                "release-check",
                w(
                    "公開前のパッケージ内容と検証状況を確認する",
                    "このプロジェクトの公開準備を確認してください。{{input}}\nプロジェクトの公開手順とマニフェストを読み、版、配布対象、README、ライセンス、依存関係、必要なビルド・検査を確認してください。既存の検査ツールを使い、実行済みと未実行を区別してください。パッケージの dry-run でも lifecycle script が動く可能性を調べてから実行してください。公開・push・タグ作成・バージョン変更はせず、公開判断に必要な問題点と次のコマンドを報告してください。",
                ),
            ),
        ]
    } else {
        [
            (
                "review",
                w(
                    "Review the current diff",
                    "Review the current changes. {{input}}\nRead the diff and surrounding code; use git_context if available. Prioritize bugs, regressions and data loss. Give file/line references and triggering conditions. Separate confirmed issues from uncertainties. If no issues are found, say so and identify unverified areas. Do not modify code or commit while reviewing.",
                ),
            ),
            (
                "diagnose",
                w(
                    "Reproduce a failure and investigate its cause",
                    "Investigate this problem: {{input}}\nRead the failure and choose the smallest reproduction from project_checks or project documentation. Respect execution permissions. Separate observations from hypotheses, narrow down the cause, and propose a minimal fix and validation. Do not apply the fix yet. If reproduction fails, report what you tried and the missing information.",
                ),
            ),
            (
                "release-check",
                w(
                    "Check package contents and release readiness",
                    "Check release readiness for this project. {{input}}\nRead its release procedure and manifests. Check versions, packaged files, README, license, dependencies, build and required checks. Distinguish executed checks from pending ones. Inspect lifecycle scripts before running a package dry-run, as those scripts may execute. Do not publish, push, tag, or change versions. Report blockers and the next commands needed for the release decision.",
                ),
            ),
        ]
    };
    list.into_iter().map(|(n, w)| (n.to_string(), w)).collect()
}

/// `/\{\{(?!input\}\})[^}]*\}\}/`: a placeholder other than {{input}}.
fn unknown_placeholder(prompt: &str) -> bool {
    let mut rest = prompt;
    while let Some(at) = rest.find("{{") {
        let after = &rest[at + 2..];
        if !after.starts_with("input}}") {
            let body = after.find('}').map_or(after, |end| &after[..end]);
            if after[body.len()..].starts_with("}}") {
                return true;
            }
        }
        rest = &rest[at + 1..];
    }
    false
}

pub fn load_workflows(lang: Lang, settings: &Value) -> anyhow::Result<IndexMap<String, Workflow>> {
    let mut workflows = builtins(lang);
    let Some(raw) = settings.get("workflows") else {
        return Ok(workflows);
    };
    let Value::Object(entries) = raw else {
        anyhow::bail!("workflows must be an object");
    };
    for (name, raw) in entries {
        if !valid_name(name) || name == "list" || name == "show" {
            anyhow::bail!("Invalid workflow name: {name}");
        }
        if raw == &Value::Bool(false) {
            workflows.shift_remove(name);
            continue;
        }
        let Value::Object(o) = raw else {
            anyhow::bail!("Invalid workflow: {name}");
        };
        let description = match o.get("description") {
            Some(Value::String(d)) if !js_trim(d).is_empty() && utf16_len(d) <= 200 => d.clone(),
            _ => anyhow::bail!("Invalid description: {name}"),
        };
        let prompt = match o.get("prompt") {
            Some(Value::String(p)) if !js_trim(p).is_empty() && utf16_len(p) <= 20_000 => p.clone(),
            _ => anyhow::bail!("Invalid prompt: {name}"),
        };
        if unknown_placeholder(&prompt) {
            anyhow::bail!("Unknown placeholder in {name}; only {{{{input}}}} is supported");
        }
        workflows.insert(name.clone(), Workflow { description, prompt });
    }
    Ok(workflows)
}

/// The prompt with `input` in place of {{input}} (literally, in one pass), or after it.
pub fn render_prompt(workflow: &Workflow, input: &str) -> String {
    if workflow.prompt.contains("{{input}}") {
        workflow.prompt.replace("{{input}}", input)
    } else if input.is_empty() {
        workflow.prompt.clone()
    } else {
        format!("{}\n\n{input}", workflow.prompt)
    }
}

/// `/^(\S+)(?:\s+([\s\S]*))?$/`: the first word and the rest.
fn split_word(s: &str) -> Option<(&str, Option<&str>)> {
    let end = s.find(js_space).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let rest = &s[end..];
    let after = rest.trim_start_matches(js_space);
    Some((&s[..end], (!rest.is_empty()).then_some(after)))
}

fn settings_schema() -> Value {
    json!({
        "type": "object", "additionalProperties": false, "properties": {
            "workflows": { "type": "object", "additionalProperties": { "anyOf": [
                { "const": false },
                { "type": "object", "required": ["description", "prompt"], "additionalProperties": false, "properties": {
                    "description": { "type": "string", "minLength": 1, "maxLength": 200, "pattern": "\\S" },
                    "prompt": { "type": "string", "minLength": 1, "maxLength": 20000, "pattern": "\\S" },
                } },
            ] } },
        },
    })
}

pub fn plugin(api: PluginApi) -> BoxFuture<'static, anyhow::Result<()>> {
    Box::pin(async move {
        let settings = api.define_settings(&settings_schema(), None)?;
        let lang = api.ui().lang();
        let workflows = Arc::new(load_workflows(lang, &settings)?);
        let ja = lang == Lang::Ja;
        let listing = |w: &IndexMap<String, Workflow>| {
            w.iter()
                .map(|(name, w)| format!("{name}: {}", w.description))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let (wf, wf2, a) = (workflows.clone(), workflows, api.clone());
        api.register_command(
            Command::new(
                "workflow",
                if ja {
                    "定型依頼を実行・表示する"
                } else {
                    "Run or preview a reusable request"
                },
                move |c| {
                    let (wf, a) = (wf.clone(), a.clone());
                    async move {
                        let ui = a.ui();
                        let first = split_word(js_trim(&c.args));
                        let Some((word, rest)) = first.filter(|(w, _)| *w != "list") else {
                            let l = listing(&wf);
                            let none = if ja {
                                "ワークフローはありません。"
                            } else {
                                "No workflows."
                            };
                            ui.notify(if l.is_empty() { none } else { &l }, NotifyLevel::Info);
                            return Ok(());
                        };
                        let showing = word == "show";
                        let preview = if showing {
                            split_word(rest.unwrap_or(""))
                        } else {
                            Some((word, rest))
                        };
                        let name = preview.map_or("", |p| p.0);
                        let Some(workflow) = wf.get(name) else {
                            ui.notify(
                                &format!(
                                    "{}: {name}\n{}",
                                    if ja {
                                        "不明なワークフロー"
                                    } else {
                                        "Unknown workflow"
                                    },
                                    listing(&wf)
                                ),
                                NotifyLevel::Error,
                            );
                            return Ok(());
                        };
                        let prompt = render_prompt(workflow, preview.and_then(|p| p.1).unwrap_or(""));
                        if showing {
                            ui.show_text(ShowTextOptions {
                                title: format!("/workflow {name}"),
                                text: prompt,
                                format: TextFormat::Text,
                            })
                            .await?;
                            return Ok(());
                        }
                        a.session().inject_text(&prompt, InjectWhen::Now);
                        Ok(())
                    }
                },
            )
            .hint("[list | show <name> | <name> [request]]")
            .complete(move |prefix| {
                let showing = prefix.starts_with("show ");
                let start = if showing { &prefix[5..] } else { prefix.as_str() };
                let out: Vec<SelectOption> = wf2
                    .iter()
                    .filter(|(name, _)| name.starts_with(start))
                    .map(|(name, w)| {
                        SelectOption::new(format!("{}{name}", if showing { "show " } else { "" }), name.clone())
                            .describe(w.description.clone())
                    })
                    .collect();
                async move { out }
            }),
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_other_than_input_are_found() {
        assert!(!unknown_placeholder("Explain {{input}} now"));
        assert!(unknown_placeholder("{{env}}"));
        assert!(unknown_placeholder("{{input}} and {{ x }}"));
        assert!(!unknown_placeholder("{{ a } b"));
        assert!(unknown_placeholder("{{{input}}"));
        assert_eq!(split_word("show  explain x"), Some(("show", Some("explain x"))));
        assert_eq!(split_word("explain"), Some(("explain", None)));
    }
}
