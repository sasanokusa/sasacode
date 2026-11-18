//! East Asian Ambiguous characters (①, ○, ※ in some fonts, …) take one cell in most terminals but
//! two in ones set up for CJK. If the UI counts them differently from the terminal, lines overlap.
//! Ask the terminal: print one, then read the cursor position back (DSR). The answer can be forced
//! with SASACODE_AMBIGUOUS_WIDTH=1 or 2 (`ambiguous.ts`).

use std::time::Duration;

use crate::term::Terminal;
use crate::text::set_ambiguous_width;

/// Set the width the UI counts ambiguous characters with: forced by the environment, else measured.
pub fn match_ambiguous_width(term: &mut dyn Terminal) {
    match std::env::var("SASACODE_AMBIGUOUS_WIDTH").as_deref() {
        Ok("1") => return set_ambiguous_width(1),
        Ok("2") => return set_ambiguous_width(2),
        _ => {}
    }
    if let Some(w) = term.probe_ambiguous_width() {
        set_ambiguous_width(w);
    }
}

/// The column in a cursor position report (`ESC [ row ; col R`) found in `buf`.
pub fn parse_report(buf: &[u8]) -> Option<u32> {
    let text = String::from_utf8_lossy(buf);
    let mut rest = text.as_ref();
    while let Some(i) = rest.find("\x1b[") {
        let after = &rest[i + 2..];
        let row_len = after.bytes().take_while(u8::is_ascii_digit).count();
        if row_len > 0 && after.as_bytes().get(row_len) == Some(&b';') {
            let cols = &after[row_len + 1..];
            let col_len = cols.bytes().take_while(u8::is_ascii_digit).count();
            if col_len > 0 && cols.as_bytes().get(col_len) == Some(&b'R') {
                return cols[..col_len].parse().ok();
            }
        }
        rest = &rest[i + 2..];
    }
    None
}

/// Print ① from column 1 and read where the cursor went: column 3 means two cells. Reads the tty
/// itself, before the event reader starts (crossterm's own query waits two seconds).
#[cfg(unix)]
pub fn probe_tty(timeout: Duration) -> Option<u8> {
    use std::io::{IsTerminal, Write};
    use std::time::Instant;

    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    let was_raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
    if !was_raw {
        crossterm::terminal::enable_raw_mode().ok()?;
    }
    let mut out = std::io::stdout();
    let _ = out.write_all("\r①\x1b[6n".as_bytes());
    let _ = out.flush();
    let deadline = Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::new();
    let mut found = None;
    while found.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
        use std::os::fd::AsFd;
        let stdin = std::io::stdin();
        let mut fds = [PollFd::new(stdin.as_fd(), PollFlags::POLLIN)];
        let ms = PollTimeout::try_from(left).unwrap_or(PollTimeout::MAX);
        if !matches!(poll(&mut fds, ms), Ok(ready) if ready > 0) {
            break;
        }
        let mut chunk = [0u8; 256];
        // The tty itself, unbuffered: std's Stdin would keep what follows the report from the event reader.
        let n = match nix::unistd::read(stdin.as_fd(), &mut chunk) {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        buf.extend_from_slice(&chunk[..n]);
        found = parse_report(&buf);
    }
    let _ = out.write_all(b"\r\x1b[2K");
    let _ = out.flush();
    if !was_raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    found.map(|col| if col == 3 { 2 } else { 1 })
}

/// Windows consoles answer DSR only through the console API crossterm's reader owns: keep the
/// default (one cell) unless SASACODE_AMBIGUOUS_WIDTH says otherwise.
#[cfg(not(unix))]
pub fn probe_tty(_timeout: Duration) -> Option<u8> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_column_of_a_cursor_report() {
        assert_eq!(parse_report(b"\x1b[12;3R"), Some(3));
        assert_eq!(parse_report(b"x\x1b[A\x1b[1;2R"), Some(2));
        assert_eq!(parse_report(b"\x1b[1;R"), None);
    }
}
