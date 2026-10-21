//! What `android-app-support` says of the Pixel, and the apps it lists.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Where the Pixel is: still to be made, being made, running and so on.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    /// Not yet asked: the first answer has not come.
    #[default]
    Unknown,
    /// This computer cannot run it: no KVM, or not x86-64.
    Unsupported,
    /// Never set up.
    New,
    /// Downloading Android, or starting the Pixel the first time.
    SettingUp,
    /// The setup stopped short; [`Status::detail`] says why.
    Failed,
    /// Set up, and not running.
    Off,
    /// Running, and Android not yet started.
    Starting,
    /// Running, its apps ready to open.
    Running,
}

impl State {
    /// The state in a few words, for the page's header.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Unknown => "Looking for the Pixel",
            Self::Unsupported => "Not available on this computer",
            Self::New => "Not set up yet",
            Self::SettingUp => "Setting up",
            Self::Failed => "Setup did not finish",
            Self::Off => "Off",
            Self::Starting => "Starting",
            Self::Running => "Running",
        }
    }

    /// Whether the Pixel exists, and can be started and stopped.
    pub fn is_set_up(self) -> bool {
        matches!(self, Self::Off | Self::Starting | Self::Running)
    }
}

/// `android-app-support status`'s answer.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize)]
pub struct Status {
    /// Where the Pixel is.
    pub state: State,
    /// What it is doing in words: the setup's progress, or why it failed.
    #[serde(default)]
    pub detail: String,
}

impl Status {
    /// Reads the line of JSON `android-app-support status` prints.
    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line.trim()).ok()
    }
}

/// One of the Pixel's apps, as it lists them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AndroidApp {
    /// Its package, which opens it (`com.android.chrome`).
    pub package: String,
    /// Its name on the Pixel's home screen.
    pub name: String,
    /// Whether the Pixel came with it, rather than the person installing it.
    pub system: bool,
    /// Its icon as the Pixel draws it, where `android-app-support` put it.
    pub icon: PathBuf,
}

/// Where `android-app-support` keeps a person's Pixel and its app list:
/// `$XDG_DATA_HOME/android-app-support`.
pub fn data_dir() -> Option<PathBuf> {
    let data = derisk_sys::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            derisk_sys::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
        })?;
    Some(data.join("android-app-support"))
}

/// Reads `apps.tsv` in `dir`, the list the Pixel last gave: a line per
/// app, its package, name and `system` or `user` between tabs. The apps
/// the person installed come first, then the Pixel's own, each by name.
pub fn read_apps(dir: &Path) -> Vec<AndroidApp> {
    std::fs::read_to_string(dir.join("apps.tsv"))
        .map(|text| parse_apps(&text, &dir.join("icons")))
        .unwrap_or_default()
}

/// [`read_apps`] of `text`, with icons in `icons`.
pub fn parse_apps(text: &str, icons: &Path) -> Vec<AndroidApp> {
    let mut apps: Vec<AndroidApp> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let package = fields.next()?.trim();
            let name = fields.next()?.trim();
            let system = fields.next()? == "system";
            (!package.is_empty()).then(|| AndroidApp {
                package: package.to_owned(),
                name: if name.is_empty() { package } else { name }.to_owned(),
                system,
                icon: icons.join(format!("{package}.png")),
            })
        })
        .collect();
    apps.sort_by_key(|a| (a.system, a.name.to_lowercase()));
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_state() {
        let status = Status::parse(
            r#"{"state":"setting-up","detail":"Downloading the Android Emulator: 12M of 333M","serial":"emulator-5554"}"#,
        )
        .unwrap();
        assert_eq!(status.state, State::SettingUp);
        assert!(status.detail.starts_with("Downloading"));
        for (json, state) in [
            ("unsupported", State::Unsupported),
            ("new", State::New),
            ("failed", State::Failed),
            ("off", State::Off),
            ("starting", State::Starting),
            ("running", State::Running),
        ] {
            let line = format!(r#"{{"state":"{json}"}}"#);
            assert_eq!(Status::parse(&line).unwrap().state, state);
        }
        assert_eq!(Status::parse("not json"), None);
        assert!(State::Running.is_set_up() && !State::New.is_set_up());
    }

    #[test]
    fn lists_installed_apps_before_the_pixels_own() {
        let apps = parse_apps(
            "com.android.chrome\tChrome\tsystem\n\
             org.fdroid.fdroid\tF-Droid\tuser\n\
             com.android.settings\tSettings\tsystem\n\
             broken line\n\
             com.example\t\tuser\n",
            Path::new("/icons"),
        );
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["com.example", "F-Droid", "Chrome", "Settings"]);
        assert_eq!(apps[1].icon, Path::new("/icons/org.fdroid.fdroid.png"));
        assert!(!apps[1].system && apps[2].system);
    }
}
