//! Skills shipped inside sasacode (`packages/skills/src/builtin.ts`).

use std::path::Path;

/// Skills shipped inside sasacode (and inside the single binary): (folder name, SKILL.md text).
pub const BUILTIN_SKILLS: &[(&str, &str)] = &[("tool-authoring", include_str!("../builtin/tool-authoring/SKILL.md"))];

/// Write the built-in skills under `dir` so they are ordinary files the model can read with the
/// read tool. Rewritten only when the content changed (e.g. after an upgrade).
pub fn materialize_builtin_skills(dir: &Path) -> std::io::Result<()> {
    for (name, body) in BUILTIN_SKILLS {
        let path = dir.join(name).join("SKILL.md");
        // readFileSync(path, "utf8") decodes leniently, so compare the decoded text the same way.
        if path.exists() && std::fs::read(&path).is_ok_and(|b| String::from_utf8_lossy(&b) == *body) {
            continue;
        }
        std::fs::create_dir_all(dir.join(name))?;
        std::fs::write(&path, body)?;
    }
    Ok(())
}
