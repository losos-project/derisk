//! What Settings' Privacy page says about file search, read from the same
//! `settings.conf` that `derisk-settings` writes: whether to index at all,
//! and the folders indexed beside home. Settings restarts the service when
//! either changes, so they are read once, at the start.
//!
//! Only the two `search.` lines are read here, by the format
//! `derisk-settings` writes them in, so the service does not link the
//! settings app and its interface to read two values.

use std::{
    fs,
    path::{Path, PathBuf},
};

/// The file search settings.
#[derive(Debug, PartialEq, Eq)]
pub struct Settings {
    /// Off, nothing is indexed and what was is deleted.
    pub index: bool,
    /// Folders indexed as well as home, each absolute.
    pub folders: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            index: true,
            folders: Vec::new(),
        }
    }
}

impl Settings {
    /// The settings in `$XDG_CONFIG_HOME/derisk/settings.conf`, or under
    /// `~/.config` without it. A missing or unreadable file is the
    /// defaults: home indexed, nothing else.
    pub fn read() -> Self {
        path()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// The settings in `text`. A value that does not parse leaves its
    /// default, as `derisk-settings` does with it.
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (key.trim(), value.trim()) {
                ("search.index", "true") => settings.index = true,
                ("search.index", "false") => settings.index = false,
                ("search.folders", value) => {
                    let folders: Option<Vec<PathBuf>> = value
                        .split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(|p| Some(PathBuf::from(p)).filter(|p| p.is_absolute()))
                        .collect();
                    if let Some(folders) = folders {
                        settings.folders = folders;
                    }
                }
                _ => {}
            }
        }
        settings
    }
}

/// `derisk-settings`' `default_path`, which this has to agree with.
fn path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| Path::new(dir).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))?;
    Some(config.join("derisk").join("settings.conf"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_lines_are_read_and_the_rest_left() {
        let settings = Settings::parse(
            "theme = dark\nsearch.index = false\nsearch.folders = /mnt/data, /srv/notes\n",
        );
        assert!(!settings.index);
        assert_eq!(
            settings.folders,
            [PathBuf::from("/mnt/data"), PathBuf::from("/srv/notes")]
        );
    }

    #[test]
    fn a_relative_folder_leaves_the_default() {
        let settings = Settings::parse("search.index = maybe\nsearch.folders = data, /mnt\n");
        assert_eq!(settings, Settings::default());
    }
}
