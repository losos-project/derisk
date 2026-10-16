//! The window buttons, which the top bar shows for the focused window,
//! left to right in [`Button::ORDER`].

use serde::{Deserialize, Serialize};

/// A window button.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    /// Close the window.
    Close,
    /// Minimize the window.
    Minimize,
    /// Toggle maximization.
    Maximize,
}

impl Button {
    /// Left-to-right order of the buttons, starting at the left edge.
    pub const ORDER: [Self; 3] = [Self::Close, Self::Minimize, Self::Maximize];
}

// Each window had a server-side title bar here, with its own hit test and
// metrics per form factor. derisk draws no headers any more: the top bar
// carries the focused window's buttons and title, tinted in its machine's
// colour, so every window gets its full frame for content. Windows move
// with Super+drag or by dragging the title in the top bar, and the bar's
// egui counts the double click that maximizes, so the click tracker that
// did went with it.
