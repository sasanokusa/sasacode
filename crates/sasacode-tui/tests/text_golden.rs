//! The text primitives checked against output recorded from pi-tui itself (tests/fixtures/
//! pi_tui_golden.json, dumped with bun from @earendil-works/pi-tui 0.87.1): widths with both
//! ambiguous settings, wrapping, truncation, slicing and the editor's word wrap.

use std::sync::Mutex;

use serde_json::Value;

use sasacode_tui::text::{set_ambiguous_width, slice_by_column, truncate_to_width, visible_width, wrap_text_with_ansi};

static LOCK: Mutex<()> = Mutex::new(());

fn golden() -> Value {
    let text = include_str!("fixtures/pi_tui_golden.json");
    serde_json::from_str(text).expect("fixture parses")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|s| s.as_str().expect("string").to_string())
        .collect()
}

#[test]
fn widths_match_pi_tui_for_both_ambiguous_settings() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let g = golden();
    for (setting, key) in [(1u8, "narrow"), (2, "wide")] {
        set_ambiguous_width(setting);
        let mut bad = Vec::new();
        for (s, w) in g["width"][key].as_object().expect("object") {
            let want = w.as_u64().expect("number") as usize;
            let got = visible_width(s);
            if got != want {
                bad.push(format!("{s:?}: want {want}, got {got}"));
            }
        }
        assert!(bad.is_empty(), "{key}: {bad:#?}");
    }
    set_ambiguous_width(1);
}

#[test]
fn wrapping_matches_pi_tui() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_ambiguous_width(1);
    let g = golden();
    for case in g["wrap"].as_array().expect("array") {
        let text = case["text"].as_str().expect("text");
        let width = case["width"].as_u64().expect("width") as usize;
        assert_eq!(
            wrap_text_with_ansi(text, width),
            strings(&case["out"]),
            "wrap({text:?}, {width})"
        );
    }
}

#[test]
fn truncation_matches_pi_tui() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_ambiguous_width(1);
    let g = golden();
    for case in g["truncate"].as_array().expect("array") {
        let text = case["text"].as_str().expect("text");
        let width = case["width"].as_u64().expect("width") as usize;
        let ellipsis = case["ellipsis"].as_str().expect("ellipsis");
        let pad = case["pad"].as_bool().expect("pad");
        assert_eq!(
            truncate_to_width(text, width, ellipsis, pad),
            case["out"].as_str().expect("out"),
            "truncate({text:?}, {width}, {ellipsis:?}, {pad})"
        );
    }
}

#[test]
fn slicing_matches_pi_tui() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_ambiguous_width(1);
    let g = golden();
    for case in g["slice"].as_array().expect("array") {
        let text = case["text"].as_str().expect("text");
        let start = case["start"].as_u64().expect("start") as usize;
        let len = case["len"].as_u64().expect("len") as usize;
        let strict = case["strict"].as_bool().expect("strict");
        assert_eq!(
            slice_by_column(text, start, len, strict),
            case["out"].as_str().expect("out"),
            "slice({text:?}, {start}, {len}, {strict})"
        );
    }
}

#[test]
fn editor_word_wrap_matches_pi_tui() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_ambiguous_width(1);
    let g = golden();
    for case in g["wordWrap"].as_array().expect("array") {
        let text = case["text"].as_str().expect("text");
        let width = case["width"].as_u64().expect("width") as usize;
        let want: Vec<String> = case["out"]
            .as_array()
            .expect("array")
            .iter()
            .map(|c| c["text"].as_str().expect("text").to_string())
            .collect();
        let got = sasacode_tui::ui::editing::word_wrap_line(text, width, None);
        let texts: Vec<String> = got.iter().map(|c| c.text.clone()).collect();
        assert_eq!(texts, want, "wordWrap({text:?}, {width})");
        // The ranges are byte offsets here (UTF-16 in pi-tui): convert the recorded ones.
        let byte_at = |u16_index: u64| -> usize {
            let mut units = 0u64;
            for (i, ch) in text.char_indices() {
                if units >= u16_index {
                    return i;
                }
                units += ch.len_utf16() as u64;
            }
            text.len()
        };
        let want_ranges: Vec<(usize, usize)> = case["out"]
            .as_array()
            .expect("array")
            .iter()
            .map(|c| {
                (
                    byte_at(c["startIndex"].as_u64().expect("start")),
                    byte_at(c["endIndex"].as_u64().expect("end")),
                )
            })
            .collect();
        let got_ranges: Vec<(usize, usize)> = got.iter().map(|c| (c.start, c.end)).collect();
        assert_eq!(got_ranges, want_ranges, "ranges of wordWrap({text:?}, {width})");
    }
}

/// pi-tui renders LaTeX; the port shows math as its source (rust/DECISIONS.md, rust/research/tui.md).
const MATH_CASE: &str = "Math $x^2$ and $$y$$";

#[test]
fn markdown_matches_pi_tui() {
    use sasacode_tui::ui::Component;
    use sasacode_tui::ui::markdown::{Markdown, set_hyperlinks_override};
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    set_ambiguous_width(1);
    let g = golden();
    let mut bad = Vec::new();
    for (key, links) in [("markdown", false), ("markdownLinks", true)] {
        set_hyperlinks_override(Some(links));
        for case in g[key].as_array().expect("array") {
            let text = case["text"].as_str().expect("text");
            let width = case["width"].as_u64().expect("width") as usize;
            let got = Markdown::new(text, 0, 0, sasacode_tui::theme::MARKDOWN_THEME).render(width);
            let want = if text == MATH_CASE {
                let line = "Math $x^2$ and $$y$$";
                sasacode_tui::text::wrap_text_with_ansi(line, width)
                    .into_iter()
                    .map(|l| format!("{l}{}", " ".repeat(width - visible_width(&l))))
                    .collect()
            } else {
                strings(&case["out"])
            };
            if got != want {
                bad.push(format!("{key} {text:?} @{width}\n  want {want:?}\n  got  {got:?}"));
            }
        }
    }
    set_hyperlinks_override(None);
    assert!(bad.is_empty(), "{} mismatches:\n{}", bad.len(), bad.join("\n"));
}
