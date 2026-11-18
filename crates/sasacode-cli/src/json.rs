//! JSON files under ~/.sasacode and in projects. Two ways to read, chosen by what a bad file costs.

use std::io::ErrorKind;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;

use sasacode_host::t;
use sasacode_plugin_api::{create_private_dir, write_private};

/// A file the user edits (config.json): missing is `{}`, but one that cannot be parsed is an
/// error, so nothing is used half-read and nothing overwrites it.
pub fn read_json(path: &Path) -> anyhow::Result<Value> {
    let fail = |error: String| {
        anyhow::anyhow!(t!(
            "failed to read {path}: {error}",
            path = path.display(),
            error = error
        ))
    };
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            serde_json::from_str(&text).map_err(|e| fail(e.to_string()))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Value::Object(Default::default())),
        Err(e) => Err(fail(e.to_string())),
    }
}

/// A file sasacode can do without (a cache, a manifest it only probes for): any failure is
/// `fallback`.
pub fn read_json_or<T: DeserializeOwned>(path: &Path, fallback: T) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(fallback)
}

/// Written readable by the user only: these files hold settings, trust decisions and caches.
/// `JSON.stringify(value, null, 2)` plus a newline.
pub fn write_json(path: &Path, value: &impl serde::Serialize) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir)?;
    }
    let text = format!("{}\n", sasacode_ai::json::stringify_pretty(value));
    write_private(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_is_empty_bad_is_an_error_and_writes_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b.json");
        assert_eq!(read_json(&p).unwrap(), serde_json::json!({}));
        write_json(&p, &serde_json::json!({"x": [1, 2], "y": {}})).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "{\n  \"x\": [\n    1,\n    2\n  ],\n  \"y\": {}\n}\n"
        );
        assert_eq!(read_json(&p).unwrap(), serde_json::json!({"x": [1, 2], "y": {}}));
        std::fs::write(&p, "{oops").unwrap();
        assert!(read_json(&p).is_err());
        assert_eq!(read_json_or(&p, 7), 7);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.path().join("a")).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
