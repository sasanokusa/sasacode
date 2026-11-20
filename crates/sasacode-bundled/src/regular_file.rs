//! Reading a file a tool is about to change, from a hook that runs before the call is judged: only
//! a regular file is read (a FIFO, a device or a socket could block or never end), and never more
//! than a cap.

use std::io::Read;
use std::path::Path;

/// The bytes of `path` (links followed) when it is a regular file of at most `cap` bytes. `None`
/// when it is missing, unreadable, something else than a regular file, or larger than `cap`.
pub fn read_capped(path: &Path, cap: u64) -> Option<Vec<u8>> {
    // Not opened unless it is a regular file: opening a FIFO waits for a writer.
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Swapped for a FIFO since the check above, the open still returns at once.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(cap.saturating_add(1)).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= cap).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_regular_file_up_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, "12345").unwrap();
        assert_eq!(read_capped(&f, 5), Some(b"12345".to_vec()));
        assert_eq!(read_capped(&f, 4), None);
        assert_eq!(read_capped(&dir.path().join("none"), 5), None);
        assert_eq!(read_capped(dir.path(), 5), None);
    }

    #[cfg(unix)]
    #[test]
    fn leaves_a_fifo_and_a_device_unread() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        // With no writer, reading the FIFO would wait for ever.
        assert_eq!(read_capped(&fifo, 1024), None);
        assert_eq!(read_capped(Path::new("/dev/zero"), 1024), None);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&fifo, &link).unwrap();
        assert_eq!(read_capped(&link, 1024), None);
    }
}
