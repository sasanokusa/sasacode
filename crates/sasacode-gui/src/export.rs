//! The conversation as Markdown, to be copied: each message, what was answered, and a line for
//! each call made on the way.

use std::path::PathBuf;
use std::time::SystemTime;

use sasacode_protocol::{Message, read_entries, restore, text_of};

use crate::i18n::Strings;
use crate::transcript::{Note, Step, Transcript};

pub fn markdown(transcript: &Transcript, s: Strings) -> String {
    let mut out = String::new();
    if let Some(title) = transcript.title() {
        out.push_str(&format!("# {}\n\n", crate::view::first_line(title).trim()));
    }
    for ex in &transcript.exchanges {
        out.push_str(&format!("## {}\n\n{}\n\n", s.ex_you, ex.prompt.trim()));
        out.push_str("## sasacode\n\n");
        for step in &ex.steps {
            match step {
                Step::Text(md) if !md.raw.trim().is_empty() => out.push_str(&format!("{}\n\n", md.raw.trim())),
                Step::Tool(tool) => out.push_str(&format!("- `{}` {}\n\n", tool.name, tool.summary.trim())),
                Step::Note(Note::Error(body)) => out.push_str(&format!("> {}\n\n", body.trim().replace('\n', "\n> "))),
                _ => {}
            }
        }
    }
    for queued in &transcript.queued {
        out.push_str(&format!("## {}\n\n{}\n\n", s.ex_you, queued.trim()));
    }
    out.trim_end().to_owned() + "\n"
}

/// What is said in each log (the messages and the replies, not what the tools wrote), lower case,
/// for the search of the list.
pub fn bodies(logs: Vec<(PathBuf, SystemTime)>) -> Vec<(PathBuf, SystemTime, String)> {
    logs.into_iter()
        .map(|(path, when)| {
            let said = read_entries(&path)
                .map(|entries| {
                    restore(&entries)
                        .messages
                        .iter()
                        .filter_map(|m| match m {
                            Message::User { content, .. } => Some(text_of(content)),
                            Message::Assistant(a) => Some(text_of(&a.content)),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                        .to_lowercase()
                })
                .unwrap_or_default();
            (path, when, said)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_conversation_is_written_as_markdown() {
        let mut t = Transcript::default();
        t.send("こんにちは", Instant::now());
        let line = r#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"はい、**どうぞ**。"}],"provider":"fake","model":"m","usage":{"input":1,"output":1},"stopReason":"stop"}}"#;
        t.apply(&sasacode_protocol::parse_event(line).unwrap(), Instant::now());
        let md = markdown(&t, Strings::get("ja"));
        assert_eq!(
            md,
            "# こんにちは\n\n## あなた\n\nこんにちは\n\n## sasacode\n\nはい、**どうぞ**。\n"
        );
    }
}
