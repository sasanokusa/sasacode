//! Put text on the system clipboard (`clipboard.ts`). A local clipboard tool is tried first because
//! many terminals (macOS Terminal.app, Warp, tmux without passthrough) ignore OSC 52; over SSH the
//! local tools would fill the remote machine's clipboard, so there only the terminal is asked.

use std::process::Stdio;

use base64::Engine as _;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Copied {
    /// A clipboard tool took it.
    Native,
    /// The terminal was asked (OSC 52); whether it did cannot be told.
    Terminal,
}

fn clipboard_tools() -> Vec<Vec<&'static str>> {
    if cfg!(target_os = "macos") {
        vec![vec!["pbcopy"]]
    } else if cfg!(windows) {
        vec![vec!["clip"]]
    } else {
        vec![
            vec!["wl-copy"],
            vec!["xclip", "-selection", "clipboard"],
            vec!["xsel", "--clipboard", "--input"],
        ]
    }
}

/// The OSC 52 sequence that asks the terminal to copy `text`.
pub fn osc52(text: &str) -> String {
    format!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
    )
}

async fn run_tool(cmd: &[&str], text: &str) -> bool {
    let Some((prog, args)) = cmd.split_first() else {
        return false;
    };
    let mut c = tokio::process::Command::new(prog);
    c.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // pbcopy decodes its input by the locale and drops non-ASCII text when none is set.
    if cfg!(target_os = "macos") {
        c.env("LC_ALL", "en_US.UTF-8");
    }
    let Ok(mut child) = c.spawn() else { return false };
    let bytes: Vec<u8> = if cfg!(windows) {
        // clip.exe reads UTF-8 only where that is the system code page; UTF-16 with a BOM, anywhere.
        let mut b = vec![0xff, 0xfe];
        for u in text.encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b
    } else {
        text.as_bytes().to_vec()
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(&bytes).await.is_err() {
            let _ = child.kill().await;
            return false;
        }
        drop(stdin);
    }
    matches!(child.wait().await, Ok(s) if s.success())
}

/// Copy `text`: a local tool when there is one (and the session is not remote), else OSC 52 through
/// `write` (the terminal's own writer: nothing reaches stdout around the renderer).
pub async fn copy_to_clipboard(text: &str, write: impl FnOnce(String)) -> Copied {
    let set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let remote = set("SSH_CONNECTION") || set("SSH_TTY");
    if !remote {
        for cmd in clipboard_tools() {
            if run_tool(&cmd, text).await {
                return Copied::Native;
            }
        }
    }
    write(osc52(text));
    Copied::Terminal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_carries_base64() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }
}
