//! packages/cli/test/append-system-prompt.test.ts.

use serde_json::{Value, json};

use sasacode_cli::setup::{SetupOptions, read_prompt_file, setup};

use crate::support::{Home, child, replay_provider};

fn configure(t: &Home, instructions: Option<&str>) {
    let mut config = json!({ "providers": { "test": replay_provider() }, "model": "test/m" });
    if let Some(i) = instructions {
        config["instructions"] = Value::from(i);
    }
    t.config(&config);
}

/// The system prompt setup gives the agent, with `append` as --append-system-prompt-file's text.
async fn system_prompt(t: &Home, append: Option<&str>) -> String {
    let h = setup(SetupOptions {
        no_session: true,
        append_system_prompt: append.map(str::to_string),
        ..SetupOptions::new(t.cwd())
    })
    .await
    .expect("setup");
    h.agent.system_prompt()
}

fn index_of(s: &str, pattern: &str) -> i64 {
    s.find(pattern).map_or(-1, |i| i as i64)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_appended_text_follows_config_instructions_and_precedes_the_environment_facts() {
    let Some(t) = child() else { return };
    configure(&t, Some("FROM-CONFIG"));
    let prompt = system_prompt(&t, Some("FROM-FILE\nsecond line")).await;
    assert!(index_of(&prompt, "FROM-CONFIG") > 0, "{prompt}");
    assert!(
        index_of(&prompt, "FROM-FILE") > index_of(&prompt, "FROM-CONFIG"),
        "{prompt}"
    );
    assert!(
        index_of(&prompt, "Environment:") > index_of(&prompt, "second line"),
        "{prompt}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_option_or_with_an_empty_one_the_system_prompt_is_unchanged() {
    let Some(t) = child() else { return };
    configure(&t, Some("FROM-CONFIG"));
    let plain = system_prompt(&t, None).await;
    assert!(plain.contains("FROM-CONFIG"));
    assert_eq!(system_prompt(&t, Some("")).await, plain);
    assert_eq!(system_prompt(&t, Some(" \n")).await, plain);
}

#[tokio::test(flavor = "multi_thread")]
async fn it_works_without_config_instructions_too() {
    let Some(t) = child() else { return };
    configure(&t, None);
    let prompt = system_prompt(&t, Some("ONLY-FILE")).await;
    assert!(prompt.contains("ONLY-FILE"));
    assert!(!prompt.contains("FROM-CONFIG"));
}

#[test]
fn read_prompt_file_reads_utf8_and_a_file_it_cannot_read_is_an_error_naming_the_path() {
    sasacode_host::set_lang(sasacode_plugin_api::Lang::En); // these assertions quote the English strings
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let file = root.join("prompt.txt");
    let path = |p: &std::path::Path| p.to_string_lossy().into_owned();
    std::fs::write(&file, "人格: ていねいに\n").unwrap();
    assert_eq!(read_prompt_file(&path(&file)).unwrap(), "人格: ていねいに\n");
    std::fs::write(&file, "").unwrap();
    assert_eq!(read_prompt_file(&path(&file)).unwrap(), "");
    let missing = root.join("missing.txt");
    let e = read_prompt_file(&path(&missing)).unwrap_err().to_string();
    assert!(e.contains(&format!("failed to read {}", missing.display())), "{e}");
    let e = read_prompt_file(&path(root)).unwrap_err().to_string(); // a directory
    assert!(e.contains(&format!("failed to read {}", root.display())), "{e}");
}
