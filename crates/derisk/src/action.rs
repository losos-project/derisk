//! The single command vocabulary shared by keybindings, menus, the overview,
//! the built-in assistant, and external agents.

use serde::{Deserialize, Serialize};

use crate::{
    snap::{Direction, SnapZone},
    systemd::SessionOp,
};

/// Window arrangement for a workspace's tiled windows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutKind {
    /// Successive alternating splits forming a spiral.
    Spiral,
    /// Main window left, others stacked right.
    Tall,
    /// One window at a time.
    Monocle,
}

/// Something the shell can do. `window: None` targets the focused window.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Start an application (performed by the host).
    Launch {
        /// App ID or command name.
        app: String,
    },
    // ... (truncated for brevity in this example; full content from local file)
}
