//! Commands: what the shell itself can do with no window in mind, such as
//! the overview, the on-screen keyboard, cycling focus, the layouts, the
//! shortcut list, a screenshot and the tour of the desktop.

use derisk_palette_sdk::{Category, Entry, Guest, Hook, Manifest, Value, View, export, json};

struct Commands;

impl Guest for Commands {
    fn describe() -> Manifest {
        Manifest::new(
            "commands",
            "The shell's own commands: overview, keyboard, focus, layouts",
        )
        .hooks(&[Hook::Entries])
        .categories(&[Category::Command])
        .actions(&[
            "overview",
            "keyboard",
            "focus_next",
            "focus_previous",
            "set_layout",
            "shortcut_help",
            "show_desktop",
            "screenshot",
            "tutorial",
        ])
    }

    fn entries(_: View) -> Vec<Entry> {
        entries()
    }

    fn query(_: View, _: String) -> Vec<Entry> {
        Vec::new()
    }
}

/// The catalog, which needs nothing from the view.
pub fn entries() -> Vec<Entry> {
    let commands: [(&str, &str, Value, Option<&str>, &str); 10] = [
        (
            "Overview",
            "view-app-grid",
            json!({"action": "overview"}),
            Some("Super"),
            "expose desktop show all",
        ),
        (
            "On-Screen Keyboard",
            "input-keyboard",
            json!({"action": "keyboard"}),
            None,
            "osk virtual touch type keys show hide",
        ),
        (
            "Next Window",
            "go-next",
            json!({"action": "focus_next"}),
            Some("Alt+Tab"),
            "focus switch cycle",
        ),
        (
            "Previous Window",
            "go-previous",
            json!({"action": "focus_previous"}),
            Some("Alt+Shift+Tab"),
            "focus switch cycle back",
        ),
        (
            "Tall Layout",
            "view-dual",
            json!({"action": "set_layout", "layout": "tall"}),
            Some("Super+Shift+M"),
            "tiling main stack",
        ),
        (
            "Monocle Layout",
            "view-fullscreen",
            json!({"action": "set_layout", "layout": "monocle"}),
            Some("Super+M"),
            "tiling fullscreen one at a time",
        ),
        (
            "Keyboard Shortcuts",
            "preferences-desktop-keyboard-shortcuts",
            json!({"action": "shortcut_help", "visible": true}),
            Some("Super+/"),
            "keys keybindings hotkeys help list",
        ),
        (
            "Show Desktop",
            "user-desktop",
            json!({"action": "show_desktop"}),
            Some("Super+D"),
            "minimize all hide windows wallpaper",
        ),
        (
            "Take a Screenshot",
            "applets-screenshooter",
            json!({"action": "screenshot"}),
            Some("Print"),
            "screen capture picture print",
        ),
        (
            "Take the Tour",
            "help-browser",
            json!({"action": "tutorial", "visible": true}),
            None,
            "tutorial welcome help introduction getting started guide",
        ),
    ];
    commands
        .into_iter()
        .map(|(title, icon, action, shortcut, keywords)| {
            Entry::new(Category::Command, icon, title, &[action])
                .detail("Desktop")
                .keywords(keywords)
                .shortcut(shortcut)
        })
        .collect()
}

export!(Commands with_types_in derisk_palette_sdk::bindings);
