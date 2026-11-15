//! What the window remembers between launches, in `$SASACODE_HOME/gui.json`: the settings last
//! used and the models that were typed, so a model name is typed once.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const MAX_MODELS: usize = 12;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prefs {
    pub model: Option<String>,
    pub permission: Option<String>,
    pub effort: Option<String>,
    /// Models used from this window, newest first.
    pub models: Vec<String>,
}

fn file(home: &Path) -> PathBuf {
    home.join("gui.json")
}

impl Prefs {
    /// A missing or unreadable file is an empty memory, not an error.
    pub fn load(home: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(file(home)) else {
            return Self::default();
        };
        let v: Value = serde_json::from_str(&raw).unwrap_or_default();
        let s = |key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            model: s("model"),
            permission: s("permission"),
            effort: s("effort"),
            models: v
                .get("models")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
        }
    }

    pub fn save(&self, home: &Path) {
        let v = json!({
            "model": self.model,
            "permission": self.permission,
            "effort": self.effort,
            "models": self.models,
        });
        let _ = std::fs::create_dir_all(home);
        let _ = std::fs::write(file(home), format!("{v:#}\n"));
    }

    pub fn remember_model(&mut self, model: &str) {
        self.models.retain(|m| m != model);
        self.models.insert(0, model.to_owned());
        self.models.truncate(MAX_MODELS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_saved_is_read_back_and_a_model_is_kept_once() {
        let home = std::env::temp_dir().join(format!("sasacode-gui-prefs-{}", std::process::id()));
        let mut p = Prefs::load(&home);
        assert_eq!(p, Prefs::default());
        p.remember_model("a/one");
        p.remember_model("b/two");
        p.remember_model("a/one");
        p.model = Some("a/one".into());
        p.effort = Some("high".into());
        p.save(&home);
        let read = Prefs::load(&home);
        assert_eq!(read.models, ["a/one", "b/two"]);
        assert_eq!(read, p);
        let _ = std::fs::remove_dir_all(&home);
    }
}
