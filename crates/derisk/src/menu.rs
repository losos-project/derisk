//! A global (top bar) menu registry, macOS/Unity style.
//!
//! Apps (or a `com.canonical.AppMenu.Registrar` / dbusmenu bridge in the host)
//! register menus per window. The top bar shows the focused window's menus,
//! followed by a shell-provided "Window" menu.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{action::Action, snap::SnapZone};

/// One entry in a menu.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MenuEntry {
    /// An activatable item. `id` is sent back to the app when chosen.
    Item {
        /// Identifier reported on activation.
        id: String,
        /// Visible text.
        label: String,
        /// Optional shortcut hint, e.g. `Ctrl+S`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shortcut: Option<String>,
        /// Disabled items are shown but cannot be chosen.
        #[serde(default = "enabled")]
        enabled: bool,
    },
    /// A nested menu.
    Submenu {
        /// Visible text.
        label: String,
        /// Child entries.
        entries: Vec<MenuEntry>,
    },
    /// A divider.
    Separator,
}

fn enabled() -> bool {
    true
}

impl MenuEntry {
    /// An enabled item without a shortcut.
    pub fn item(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::Item {
            id: id.into(),
            label: label.into(),
            shortcut: None,
            enabled: true,
        }
    }

    /// Adds a shortcut hint to an item; other entries are returned unchanged.
    pub fn with_shortcut(mut self, hint: impl Into<String>) -> Self {
        if let Self::Item { shortcut, .. } = &mut self {
            *shortcut = Some(hint.into());
        }
        self
    }
}

/// A top-level menu such as "File".
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Menu {
    /// Title shown in the bar.
    pub title: String,
    /// Entries.
    pub entries: Vec<MenuEntry>,
}

/// Prefix of item IDs handled by the shell rather than the app.
pub const SHELL_PREFIX: &str = "derisk.";

/// The shell's own "Window" menu, appended after every app's menus.
pub fn window_menu() -> Menu {
    Menu {
        title: "Window".into(),
        entries: vec![
            MenuEntry::item("derisk.minimize", "Minimize").with_shortcut("Super+↓"),
            MenuEntry::item("derisk.maximize", "Maximize").with_shortcut("Super+↑"),
            MenuEntry::Submenu {
                label: "Snap".into(),
                entries: vec![
                    MenuEntry::item("derisk.snap.left", "Left Half").with_shortcut("Super+←"),
                    MenuEntry::item("derisk.snap.right", "Right Half").with_shortcut("Super+→"),
                    MenuEntry::item("derisk.snap.top_left", "Top Left").with_shortcut("Super+W Q"),
                    MenuEntry::item("derisk.snap.top_right", "Top Right")
                        .with_shortcut("Super+W E"),
                    MenuEntry::item("derisk.snap.bottom_left", "Bottom Left")
                        .with_shortcut("Super+W Z"),
                    MenuEntry::item("derisk.snap.bottom_right", "Bottom Right")
                        .with_shortcut("Super+W C"),
                ],
            },
            MenuEntry::item("derisk.tile", "Tile").with_shortcut("Super+T"),
            MenuEntry::item("derisk.float", "Float").with_shortcut("Super+F"),
            MenuEntry::item("derisk.promote", "Make Main Window").with_shortcut("Super+Enter"),
            MenuEntry::item("derisk.keep_on_top", "Keep on Top").with_shortcut("Super+Shift+T"),
            MenuEntry::Separator,
            MenuEntry::item("derisk.overview", "Overview").with_shortcut("Super"),
            MenuEntry::item("derisk.close", "Close").with_shortcut("Alt+F4"),
        ],
    }
}

/// Maps a shell menu item ID to the action it performs on `window`.
pub fn shell_action(item: &str, window: u64) -> Option<Action> {
    let window = Some(window);
    let snap = |zone| Action::Snap { window, zone };
    Some(match item.strip_prefix(SHELL_PREFIX)? {
        "minimize" => Action::Minimize { window },
        "maximize" => Action::ToggleMaximize { window },
        "close" => Action::Close { window },
        "tile" => Action::Tile { window },
        "float" => Action::Float { window },
        "promote" => Action::Promote,
        "keep_on_top" => Action::KeepOnTop { window, on: None },
        "overview" => Action::Overview { visible: None },
        "snap.left" => snap(SnapZone::Left),
        "snap.right" => snap(SnapZone::Right),
        "snap.top_left" => snap(SnapZone::TopLeft),
        "snap.top_right" => snap(SnapZone::TopRight),
        "snap.bottom_left" => snap(SnapZone::BottomLeft),
        "snap.bottom_right" => snap(SnapZone::BottomRight),
        _ => return None,
    })
}

/// Menus registered per window, and which agent connection, if any, owns
/// each window's menus.
#[derive(Clone, Debug, Default)]
pub struct GlobalMenu {
    menus: BTreeMap<u64, Vec<Menu>>,
    /// Picks from these windows' menus go back to the connection that
    /// registered them; the rest are only logged.
    owners: BTreeMap<u64, u64>,
}

impl GlobalMenu {
    /// Registers (or replaces) a window's menus with no owner to tell of
    /// picks, as a headless `derisk agent` or the demo does.
    pub fn register(&mut self, window: u64, menus: Vec<Menu>) {
        self.menus.insert(window, menus);
        self.owners.remove(&window);
    }

    /// Registers (or replaces) a window's menus on behalf of agent
    /// connection `owner`, which is then told of picks from them. A window
    /// whose menus another connection owns is refused, with that owner, so
    /// one program cannot take over the menus another keeps current.
    pub fn register_owned(&mut self, window: u64, owner: u64, menus: Vec<Menu>) -> Result<(), u64> {
        if let Some(&other) = self.owners.get(&window)
            && other != owner
        {
            return Err(other);
        }
        self.menus.insert(window, menus);
        self.owners.insert(window, owner);
        Ok(())
    }

    /// Forgets a window's menus and their owner.
    pub fn unregister(&mut self, window: u64) {
        self.menus.remove(&window);
        self.owners.remove(&window);
    }

    /// Forgets the menus agent connection `owner` registered, once it has
    /// closed: they described its windows as it last saw them (a browser's
    /// open tabs, say) and nobody is left to act on a pick. Returns the
    /// windows whose menus went.
    pub fn disown(&mut self, owner: u64) -> Vec<u64> {
        let windows: Vec<u64> = self
            .owners
            .iter()
            .filter(|(_, o)| **o == owner)
            .map(|(w, _)| *w)
            .collect();
        for window in &windows {
            self.unregister(*window);
        }
        windows
    }

    /// The connection that owns a window's menus, if one does.
    pub fn owner(&self, window: u64) -> Option<u64> {
        self.owners.get(&window).copied()
    }

    /// The connection to tell that `item` was picked from `window`'s menus:
    /// its owner, when `item` is an enabled item it registered there. Shell
    /// items (`derisk.`) stay the shell's, and an agent's `activate_menu`
    /// with an ID the owner never listed is not passed on as a pick.
    pub fn recipient(&self, window: u64, item: &str) -> Option<u64> {
        if item.starts_with(SHELL_PREFIX) {
            return None;
        }
        let owner = self.owner(window)?;
        self.app_menus(window)
            .iter()
            .any(|m| has_item(&m.entries, item))
            .then_some(owner)
    }

    /// A window's own menus, if it registered any.
    pub fn app_menus(&self, window: u64) -> &[Menu] {
        self.menus.get(&window).map_or(&[], Vec::as_slice)
    }

    /// Everything the bar shows for `window`: app menus then the Window menu.
    pub fn bar(&self, window: Option<u64>) -> impl Iterator<Item = Menu> + '_ {
        window.into_iter().flat_map(|w| {
            self.app_menus(w)
                .iter()
                .cloned()
                .chain(std::iter::once_with(window_menu))
        })
    }
}

/// Whether `entries`, submenus included, hold an enabled item `id`.
fn has_item(entries: &[MenuEntry], id: &str) -> bool {
    entries.iter().any(|entry| match entry {
        MenuEntry::Item {
            id: item, enabled, ..
        } => *enabled && item == id,
        MenuEntry::Submenu { entries, .. } => has_item(entries, id),
        MenuEntry::Separator => false,
    })
}
