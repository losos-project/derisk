//! The derisk core apps drawn with GPUI.
//!
//! Each app runs as its own process and opens an ordinary Wayland (or X11)
//! window, so the compositor manages it like any other client, and nothing
//! here draws with egui. `derisk session` opens the ported apps this way when
//! `derisk-gpui` is installed beside it. GPUI 0.2 can only draw into windows
//! it opens itself, so the shell's own chrome, which the compositor paints,
//! is still egui; GPUI panels and overlays run as the compositor's runtime
//! clients instead (`derisk session --runtime`).
//!
//! Apps take their colors, radius and fonts from the desktop theme
//! (`mcsapi-theme`, chosen in Settings) and follow it while running: when the
//! settings file changes they re-read it and redraw.
//!
//! Ported so far: [`CALCULATOR`]. The others still run in-process with egui.
//!
//! [`ask`] is the portal's consent dialog, run as `derisk-gpui ask` so the
//! portal can show it from its D-Bus service by spawning a process, and
//! [`pick`] its color and area pickers, run as `derisk-gpui pick`.
//!
//! ```console
//! $ cargo run -p derisk-gpui --features gpui -- org.derisk.calculator
//! $ cargo run -p derisk-gpui --features gpui -- ask --title "Take a screenshot?"
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::{path::PathBuf, time::SystemTime};

use derisk_settings::Settings;
use mcsapi_theme::{Library, Theme};

pub mod ask;
#[cfg(feature = "gpui")]
pub mod calculator;
pub mod pick;
#[cfg(feature = "gpui")]
mod run;

#[cfg(feature = "gpui")]
pub use run::run;

/// App ID of the Calculator, the same as its in-process egui version.
pub const CALCULATOR: &str = "org.derisk.calculator";

/// App IDs this crate can open.
pub const APPS: [&str; 1] = [CALCULATOR];

/// Follows the theme chosen in the settings file.
#[derive(Debug)]
pub struct ThemeWatch {
    path: Option<PathBuf>,
    library: Library,
    modified: Option<Option<SystemTime>>,
}

impl ThemeWatch {
    /// Watches the settings at `path` (usually
    /// [`derisk_settings::default_path`]), loading themes from `library`.
    pub fn new(path: Option<PathBuf>, library: Library) -> Self {
        Self {
            path,
            library,
            modified: None,
        }
    }

    /// The theme the settings choose now.
    pub fn theme(&self) -> Theme {
        let settings = self
            .path
            .as_deref()
            .and_then(|path| Settings::load(path).ok())
            .map(|(settings, _)| settings)
            .unwrap_or_default();
        settings.theme_spec(&self.library).0
    }

    /// The theme on the first call and whenever the settings file's
    /// modification time changes (including when it appears or goes away),
    /// else `None`.
    pub fn poll(&mut self) -> Option<Theme> {
        let modified = self
            .path
            .as_deref()
            .and_then(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok());
        if self.modified == Some(modified) {
            return None;
        }
        self.modified = Some(modified);
        Some(self.theme())
    }
}

impl Default for ThemeWatch {
    /// The user's settings and theme files.
    fn default() -> Self {
        Self::new(derisk_settings::default_path(), Library::xdg("derisk"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_settings_file() {
        let dir = std::env::temp_dir().join(format!("derisk-gpui-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.conf");
        let mut watch = ThemeWatch::new(Some(path.clone()), Library::new([dir.clone()]));
        assert_eq!(watch.poll(), Some(Theme::dark()));
        assert_eq!(watch.poll(), None);

        std::fs::write(&path, "appearance.scheme = light\n").unwrap();
        let theme = watch.poll().expect("the file appeared");
        assert_eq!(theme.scheme, mcsapi_theme::Scheme::Light);
        assert_eq!(watch.poll(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
