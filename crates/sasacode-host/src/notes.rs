//! Notes from the harness to the model (`packages/host/src/notes.ts`).

use sasacode_ai::UserContent;

/// How a bundled plugin's note to the model starts: a user turn kept in the history (turn_end's
/// inject), marked as not the user's. The interface shows it as a dim line, not as a prompt.
pub const NOTE_PREFIX: &str = "[Reminder from sasacode, not from the user] ";

pub fn harness_note(text: &str) -> String {
    format!("{NOTE_PREFIX}{text}")
}

/// The note's text without the mark, when every block of a user message is such a note.
pub fn note_text(content: &[UserContent]) -> Option<String> {
    let texts: Vec<&str> = content
        .iter()
        .map(|b| b.as_text().and_then(|t| t.strip_prefix(NOTE_PREFIX)))
        .collect::<Option<_>>()?;
    if texts.is_empty() {
        return None;
    }
    Some(texts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_is_recognised_only_when_every_block_is_one() {
        let note = |t: &str| UserContent::text(harness_note(t));
        assert_eq!(note_text(&[note("a"), note("b")]).as_deref(), Some("a b"));
        assert_eq!(note_text(&[note("a"), UserContent::text("user")]), None);
        assert_eq!(note_text(&[]), None);
    }
}
