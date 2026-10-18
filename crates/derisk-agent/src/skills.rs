//! The skills that come with the Agent, handed to Claude Code as a plugin.
//!
//! Sonne compiled these into its own agent. Claude Code reads skills from
//! folders instead, so every turn writes them out as a Claude Code plugin
//! under the store (`plugin/`) and passes it with `--plugin-dir`: the skills
//! are there for that turn only, never copied into `~/.claude`, and a person's
//! own skills there keep working beside them. They show up to the agent as
//! `derisk:<name>`.

use std::path::{Path, PathBuf};

use miette::{Context as _, IntoDiagnostic as _, Result};

/// Each built-in skill's folder name and `SKILL.md`.
pub const BUILTIN: [(&str, &str); 3] = [
    (
        "create-skill",
        include_str!("../skills/create-skill/SKILL.md"),
    ),
    (
        "customize-desktop",
        include_str!("../skills/customize-desktop/SKILL.md"),
    ),
    (
        "reduce-online-fingerprint",
        include_str!("../skills/reduce-online-fingerprint/SKILL.md"),
    ),
];

/// The plugin's manifest. Its name is the prefix the skills are listed under.
const MANIFEST: &str = r#"{
  "name": "derisk",
  "description": "The derisk Agent's built-in skills: write new skills, restyle the desktop, and delete unused online accounts.",
  "author": {"name": "derisk"}
}
"#;

/// Writes the plugin into `root/plugin`, replacing what an older Agent wrote
/// there, and returns its folder for `--plugin-dir`.
pub fn install(root: &Path) -> Result<PathBuf> {
    let dir = root.join("plugin");
    let write = |path: PathBuf, text: &str| -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .into_diagnostic()
                .wrap_err_with(|| format!("creating {}", parent.display()))?;
        }
        // Unchanged files are left alone, so a turn does not rewrite them
        // under a chat that is reading them.
        if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
            return Ok(());
        }
        std::fs::write(&path, text)
            .into_diagnostic()
            .wrap_err_with(|| format!("writing {}", path.display()))
    };
    write(dir.join(".claude-plugin/plugin.json"), MANIFEST)?;
    for (name, text) in BUILTIN {
        write(dir.join("skills").join(name).join("SKILL.md"), text)?;
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_skill_names_its_folder() {
        for (name, text) in BUILTIN {
            let front = text
                .strip_prefix("---\n")
                .and_then(|rest| rest.split_once("\n---\n"))
                .map(|(front, _)| front)
                .expect("frontmatter");
            assert!(
                front.lines().any(|line| line == format!("name: {name}")),
                "{name}"
            );
            assert!(front.lines().any(|line| line.starts_with("description: ")));
        }
    }

    #[test]
    fn installs_a_claude_code_plugin() -> Result<()> {
        let dir = tempfile::tempdir().into_diagnostic()?;
        let plugin = install(dir.path())?;
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(plugin.join(".claude-plugin/plugin.json"))
                .into_diagnostic()?,
        )
        .into_diagnostic()?;
        assert_eq!(manifest["name"], "derisk");
        assert!(plugin.join("skills/customize-desktop/SKILL.md").exists());
        // A second turn finds it in place.
        assert_eq!(install(dir.path())?, plugin);
        Ok(())
    }
}
