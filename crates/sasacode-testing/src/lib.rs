//! Test support (`packages/testing`, `packages/e2e/test`): a provider that replays recorded replies,
//! a fake OpenAI-compatible model server, and the harness that runs a sasacode binary against it.
//! Not part of the app.

pub mod fake_server;
pub mod harness;
pub mod replay;

pub use fake_server::{FakeServer, Reply, Script, ToolCallShape, ToolCallSpec};
pub use replay::{
    Delta, DeltaKind, RecordedRequest, Recording, RecordingProvider, ReplayItem, ReplayOptions, ReplayProvider,
};

/// Byte offsets in `s` of the UTF-16 offsets `cuts` (ascending), each moved forward to the next char
/// boundary when it falls inside a surrogate pair. The TS sliced strings by UTF-16 units; for text in
/// the Basic Multilingual Plane (Japanese included) the pieces are the same, and a character outside
/// it stays whole where JS would have cut it into two lone surrogates.
pub(crate) fn utf16_cuts(s: &str, cuts: &[usize]) -> Vec<usize> {
    let mut out = Vec::with_capacity(cuts.len());
    let mut wanted = cuts.iter().copied().peekable();
    let mut units = 0;
    for (i, c) in s.char_indices() {
        while wanted.next_if(|&w| w <= units).is_some() {
            out.push(i);
        }
        units += c.len_utf16();
    }
    out.extend(wanted.map(|_| s.len()));
    out
}

/// `s.length` in JS: UTF-16 code units.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_count_utf16_units_and_keep_pairs_whole() {
        assert_eq!(utf16_cuts("hello", &[0, 2, 5, 9]), vec![0, 2, 5, 5]);
        // Japanese: one unit, three bytes each.
        assert_eq!(utf16_cuts("日本語です", &[1, 3]), vec![3, 9]);
        // An emoji is two units: a cut in its middle moves past it.
        assert_eq!(utf16_cuts("a😀b", &[1, 2, 3]), vec![1, 5, 5]);
        assert_eq!(utf16_len("a😀b"), 4);
    }
}
