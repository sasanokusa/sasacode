//! Piped input for `-p` (`packages/cli/src/stdin.ts`).

use std::io::{IsTerminal, Read};
use std::time::Duration;

use sasacode_host::t;

/// How long a pipe may stay silent before it is taken for one nobody writes to (without --stdin).
const PIPE_WAIT: Duration = Duration::from_millis(5000);

enum Source {
    None,
    File,
    Pipe,
}

fn source() -> Source {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Source::None;
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsFd;
        let Ok(st) = nix::sys::stat::fstat(stdin.as_fd()) else {
            return Source::None;
        };
        let kind = nix::sys::stat::SFlag::from_bits_truncate(st.st_mode) & nix::sys::stat::SFlag::S_IFMT;
        if kind == nix::sys::stat::SFlag::S_IFCHR {
            return Source::None; // a terminal or /dev/null
        }
        if kind == nix::sys::stat::SFlag::S_IFREG {
            return Source::File;
        }
    }
    Source::Pipe
}

/// Piped input is appended to -p. A file is read whole. A pipe is read to its end once something
/// arrives; one that stays open and silent (a parent shell's) is left alone after PIPE_WAIT, with
/// a note, unless `wait` (--stdin) says the input is coming however long it takes.
///
/// The read runs on a thread of its own: a pipe left alone stays blocked there until the process
/// exits, without holding up anything else.
pub async fn read_piped_stdin(wait: bool) -> String {
    let file = match source() {
        Source::None => return String::new(),
        Source::File => true,
        Source::Pipe => false,
    };
    let (first_tx, first_rx) = tokio::sync::oneshot::channel::<Option<Vec<u8>>>();
    let (rest_tx, rest_rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buf = vec![0u8; 64 * 1024];
        let n = loop {
            match stdin.read(&mut buf) {
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break 0,
            }
        };
        if n == 0 {
            let _ = first_tx.send(None);
            return;
        }
        buf.truncate(n);
        if first_tx.send(Some(buf)).is_err() {
            return;
        }
        let mut rest = Vec::new();
        let _ = stdin.read_to_end(&mut rest);
        let _ = rest_tx.send(rest);
    });
    let first = if wait || file {
        first_rx.await.ok().flatten()
    } else {
        match tokio::time::timeout(PIPE_WAIT, first_rx).await {
            Ok(r) => r.ok().flatten(),
            Err(_) => {
                eprintln!(
                    "{}",
                    t!(
                        "warning: nothing arrived on stdin within {s}s, so it was not read (pass --stdin to wait for it)",
                        s = PIPE_WAIT.as_secs()
                    )
                );
                return String::new();
            }
        }
    };
    let Some(mut bytes) = first else { return String::new() };
    bytes.extend(rest_rx.await.unwrap_or_default());
    String::from_utf8_lossy(&bytes).into_owned()
}
