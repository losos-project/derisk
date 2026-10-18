//! The Settings app's sidebar: its pages, grouped. The rest of its frame,
//! the page's header, the cards its rows sit in and the spacing between
//! them, is `derisk-frame`'s, which every derisk app draws with, so they
//! all look like Settings.

use derisk_frame::Nav;
pub use derisk_frame::{card, label_width, note};
use mcsapi_components::Tokens;
use mcsapi_ui::egui;

use crate::Page;

/// The sidebar's sections, in order. A page missing from the list the app
/// shows (a choice screen the policy leaves off) is skipped, and so is a
/// section left empty.
pub const GROUPS: [(&str, &[Page]); 4] = [
    (
        "Personalize",
        &[
            Page::Appearance,
            Page::Wallpaper,
            Page::TopBar,
            Page::Desktop,
        ],
    ),
    (
        "Devices",
        &[Page::Input, Page::Shortcuts, Page::Network, Page::Bluetooth],
    ),
    ("Security", &[Page::Privacy, Page::SignIn, Page::Users]),
    (
        "System",
        &[
            Page::Notifications,
            Page::Power,
            Page::Backups,
            Page::DefaultApps,
            Page::System,
            Page::About,
        ],
    ),
];

/// The sidebar, or a narrow window's tabs: each section's title over its
/// pages, the open one filled. Returns the page picked this frame.
pub fn sidebar(nav: &mut Nav<'_>, open: Page, listed: impl Fn(Page) -> bool) -> Option<Page> {
    let mut picked = None;
    for (title, pages) in GROUPS {
        let pages: Vec<Page> = pages.iter().copied().filter(|&p| listed(p)).collect();
        if pages.is_empty() {
            continue;
        }
        nav.section(title);
        for page in pages {
            if nav.entry(page.label(), page == open).clicked() {
                picked = Some(page);
            }
        }
    }
    picked
}

/// The page's title and the line under it saying what it holds.
pub fn header(ui: &mut egui::Ui, tokens: &Tokens, page: Page) {
    derisk_frame::header(ui, tokens, page.label(), page.description());
}
