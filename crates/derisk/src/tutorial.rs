//! The tour of the desktop that the first login starts with.
//!
//! A few cards, each pointing at one part of the desktop (the top bar, the
//! window buttons, the search field, the overview) with a sentence or two
//! on what it is for and the keys that reach it. Finishing or skipping it
//! leaves a mark under `$XDG_STATE_HOME/derisk`, so later logins go
//! straight to the desktop; the command palette's "Take the Tour" shows it
//! again.

use std::path::PathBuf;

/// What a step points at; the shell draws a ring around it and dims the
/// rest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// Nothing: the card stands alone in the middle of the screen.
    Nothing,
    /// The whole top bar (the status bar on a phone).
    TopBar,
    /// The focused window's buttons and title in the top bar; the whole
    /// bar while no window has the focus.
    WindowControls,
    /// The search field that opens the command palette.
    Search,
    /// The overview button.
    Overview,
    /// A phone's navigation bar.
    NavBar,
}

/// One card of the tour.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Step {
    /// Its heading.
    pub title: &'static str,
    /// What it says.
    pub body: &'static str,
    /// What it points at.
    pub target: Target,
}

const DESKTOP: &[Step] = &[
    Step {
        title: "Welcome to LosOS",
        body: "This short tour shows where things are. Use the arrow keys or \
               the buttons below to go through it. You can take it again \
               from the command palette whenever you like.",
        target: Target::Nothing,
    },
    Step {
        title: "The top bar",
        body: "It names the window you are using and holds that window's \
               menus. A window from a virtual machine colours the bar with \
               its machine's label, so you always know where it runs.",
        target: Target::TopBar,
    },
    Step {
        title: "Window buttons",
        body: "Windows have no title bar of their own. Close, minimize and \
               maximize the window you are using from here, drag its title \
               to move it, and double-click the title to maximize it.",
        target: Target::WindowControls,
    },
    Step {
        title: "Search and commands",
        body: "Press Super+Space, or click here, to open apps, files, \
               settings and commands, or to ask for something in your own \
               words.",
        target: Target::Search,
    },
    Step {
        title: "Overview",
        body: "Tap Super on its own, or click here, to see every window and \
               workspace at once.",
        target: Target::Overview,
    },
    Step {
        title: "Moving windows",
        body: "Windows tile by themselves. Hold Super and drag a window to \
               move it, and drop it on an edge or a corner of the screen to \
               snap it there.",
        target: Target::Nothing,
    },
    Step {
        title: "Keyboard shortcuts",
        body: "Press Super+/ to see them all. Some take two steps: Super+W \
               and then an arrow snaps a window to that side.",
        target: Target::Nothing,
    },
    Step {
        title: "You're all set",
        body: "Settings (Super+I) changes the look, the shortcuts and more.",
        target: Target::Nothing,
    },
];

const PHONE: &[Step] = &[
    Step {
        title: "Welcome to LosOS",
        body: "This short tour shows where things are. You can take it \
               again from Apps whenever you like.",
        target: Target::Nothing,
    },
    Step {
        title: "The status bar",
        body: "It shows the time and names the app on screen. An app from a \
               virtual machine colours it with its machine's label.",
        target: Target::TopBar,
    },
    Step {
        title: "Getting around",
        body: "Back goes back, Home shows your open apps and widgets, Apps \
               lists every app and searches, and the last button brings up \
               the keyboard.",
        target: Target::NavBar,
    },
    Step {
        title: "You're all set",
        body: "Settings changes the look and more.",
        target: Target::Nothing,
    },
];

/// The tour's steps, in order, for a phone or a bigger screen.
pub fn steps(phone: bool) -> &'static [Step] {
    if phone { PHONE } else { DESKTOP }
}

/// Where finishing the tour leaves its mark:
/// `$XDG_STATE_HOME/derisk/tutorial-seen`, falling back to
/// `~/.local/state`. `None` with neither set.
pub fn mark_path() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(state.join("derisk").join("tutorial-seen"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_tours_start_with_a_welcome_and_end_on_nothing() {
        for phone in [false, true] {
            let steps = steps(phone);
            assert!(steps.len() >= 3);
            assert_eq!(steps[0].title, "Welcome to LosOS");
            assert_eq!(steps.last().unwrap().target, Target::Nothing);
        }
    }

    #[test]
    fn a_phone_tour_points_at_nothing_a_phone_lacks() {
        assert!(steps(true).iter().all(|s| !matches!(
            s.target,
            Target::WindowControls | Target::Search | Target::Overview
        )));
        assert!(steps(false).iter().all(|s| s.target != Target::NavBar));
    }
}
