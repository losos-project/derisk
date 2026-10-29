//! What the shell calls an app: its name and icon, looked up by the app ID
//! its windows carry.
//!
//! A Wayland window only says its `app_id` (`org.mozilla.firefox`, `foot`),
//! which is an identifier, not something to show a person. The installed
//! `.desktop` files say what the app is called and which icon it uses, so
//! title bars, the top bar, the overview and Snap Assist show those instead.
//!
//! ```
//! use derisk_desktop::{apps::Apps, desktop::DesktopEntry};
//!
//! let firefox = DesktopEntry::parse("org.mozilla.firefox.desktop", "\
//! [Desktop Entry]
//! Type=Application
//! Name=Firefox
//! Icon=firefox
//! Exec=firefox %u
//! ").unwrap();
//! let apps = Apps::new([(&firefox, "")]);
//! assert_eq!(apps.look("org.mozilla.firefox").name, "Firefox");
//! // Older builds report the bare program name.
//! assert_eq!(apps.look("firefox").icon, "firefox");
//! // Unknown apps keep a readable form of their ID.
//! assert_eq!(apps.look("org.example.notes").name, "Notes");
//! ```

use std::{borrow::Cow, collections::HashMap};

use crate::desktop::DesktopEntry;

/// How to show one app.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppLook<'a> {
    /// The name to show, from `Name`, or derived from the app ID when no
    /// `.desktop` file matches.
    pub name: Cow<'a, str>,
    /// Icon theme name or absolute path. Without a matching `.desktop` file
    /// it is the app ID, which is the icon name most apps install under.
    pub icon: &'a str,
    /// A glyph to draw when the icon theme has no such icon, empty if none.
    pub glyph: &'a str,
    /// Whether its windows turn on focus mode
    /// ([`DesktopEntry::wants_focus`]); never for an app nothing describes.
    pub focus: bool,
}

#[derive(Clone, Debug)]
struct Known {
    name: String,
    icon: String,
    glyph: String,
    focus: bool,
}

/// App IDs mapped to the `.desktop` files that describe them.
#[derive(Clone, Debug, Default)]
pub struct Apps {
    apps: Vec<Known>,
    /// Lowercased match keys to indices in `apps`.
    keys: HashMap<String, usize>,
}

/// The file ID without `.desktop`: `org.mozilla.firefox`.
fn stem(id: &str) -> &str {
    id.strip_suffix(".desktop").unwrap_or(id)
}

/// The last part of a reverse-DNS ID: `firefox` for `org.mozilla.firefox`.
fn last_part(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

/// A readable name for an app nothing describes: the last part of a
/// reverse-DNS ID, capitalized (`org.example.notes` is "Notes", `foot` is
/// "Foot").
fn readable(app_id: &str) -> String {
    let part = if app_id.contains('.') {
        last_part(app_id)
    } else {
        app_id
    };
    let mut chars = part.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

impl Apps {
    /// Indexes `entries`, each with a glyph to fall back to when its icon is
    /// not in the theme (empty for none). Earlier entries win a shared key.
    pub fn new<'a>(entries: impl IntoIterator<Item = (&'a DesktopEntry, &'a str)>) -> Self {
        let mut apps = Self::default();
        let mut programs = Vec::new();
        let mut short = Vec::new();
        for (entry, glyph) in entries {
            let i = apps.apps.len();
            apps.apps.push(Known {
                name: entry.name.clone(),
                icon: entry.icon.clone(),
                glyph: glyph.to_owned(),
                focus: entry.wants_focus(),
            });
            // The spec says the app ID is the desktop file ID, and
            // StartupWMClass names it when the two differ, so those are
            // exact and go in first.
            apps.add(stem(&entry.id), i);
            if !entry.wm_class.is_empty() {
                apps.add(&entry.wm_class, i);
            }
            // Many apps get this wrong one way or the other: a window
            // reporting `firefox` for `org.mozilla.firefox.desktop`, or the
            // program name. Those guesses only fill keys nothing exact took.
            short.push((last_part(stem(&entry.id)).to_owned(), i));
            if let Some(program) = entry.argv().first() {
                let program = program.rsplit('/').next().unwrap_or(program);
                programs.push((program.to_owned(), i));
            }
        }
        // A program several apps start through (`flatpak`, `env`, `derisk`)
        // says nothing about which one a window belongs to.
        let mut count = HashMap::<&str, usize>::new();
        for (program, _) in &programs {
            *count.entry(program).or_default() += 1;
        }
        let programs: Vec<_> = programs
            .iter()
            .filter(|(program, _)| count[program.as_str()] == 1)
            .cloned()
            .collect();
        for (key, i) in short.into_iter().chain(programs) {
            apps.add(&key, i);
        }
        apps
    }

    fn add(&mut self, key: &str, i: usize) {
        if !key.is_empty() {
            self.keys.entry(key.to_lowercase()).or_insert(i);
        }
    }

    /// The name and icon to show for windows of `app_id`.
    pub fn look<'a>(&'a self, app_id: &'a str) -> AppLook<'a> {
        let key = app_id.to_lowercase();
        // The palette launches apps by desktop file ID, `.desktop` and all.
        let key = stem(&key);
        // `org.mozilla.firefox` for an app installed as `firefox.desktop`.
        let found = self.keys.get(key).or_else(|| self.keys.get(last_part(key)));
        match found.map(|&i| &self.apps[i]) {
            Some(app) => AppLook {
                name: Cow::Borrowed(&app.name),
                icon: if app.icon.is_empty() {
                    stem(app_id)
                } else {
                    &app.icon
                },
                glyph: &app.glyph,
                focus: app.focus,
            },
            None => AppLook {
                name: Cow::Owned(readable(app_id)),
                icon: app_id,
                glyph: "",
                focus: false,
            },
        }
    }
}
